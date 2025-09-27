use std::collections::HashSet;
use std::convert::Infallible;
use std::io::{self, Cursor, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::{mpsc as sync_mpsc, Arc, Mutex};
use std::time::Duration;

use brotli::{enc::BrotliEncoderParams, BrotliCompress};
use futures_util::{SinkExt, StreamExt};
use globset::{GlobBuilder, GlobMatcher};
use hyper::header::{
    HeaderName, HeaderValue, ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH,
    CONTENT_TYPE, VARY,
};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use magnus::{
    function, prelude::*, Error, IntoValue, RArray, RHash, RModule, Ruby, TryConvert, Value,
};
use mime_guess::MimeGuess;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use once_cell::sync::Lazy;
use percent_encoding::percent_decode_str;
use tokio::net::TcpListener;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task;
use tokio_tungstenite::{accept_async, tungstenite::protocol::Message};

use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;

use crate::ruby_utils::ruby_handle;
use crate::time_utils::ensure_time_required;

const DEFAULT_LIVERELOAD_PORT: u16 = 35_729;

static DIRECTORY_INDEX: Lazy<Vec<&'static str>> = Lazy::new(|| {
    vec![
        "index.html",
        "index.htm",
        "index.rhtml",
        "index.xht",
        "index.xhtml",
        "index.cgi",
        "index.xml",
        "index.json",
    ]
});

static LIVE_RELOAD_HANDLE: Lazy<Mutex<Option<LiveReloadBroadcaster>>> =
    Lazy::new(|| Mutex::new(None));

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("engine_serve_process", function!(engine_serve_process, 1))?;
    bridge.define_singleton_method("livereload_reload", function!(livereload_reload, 1))?;
    Ok(())
}

#[derive(Clone, Debug)]
struct ServeOptions {
    source: PathBuf,
    destination: PathBuf,
    host: String,
    port: u16,
    baseurl: Option<String>,
    show_dir_listing: bool,
    watch: bool,
    livereload: bool,
    livereload_port: u16,
    livereload_min_delay: u64,
    livereload_max_delay: u64,
    livereload_ignore: Vec<String>,
    exclude: Vec<String>,
    cache_dir: String,
    open_url: bool,
    detach: bool,
    ssl_cert: Option<String>,
    ssl_key: Option<String>,
}

impl ServeOptions {
    fn from_rhash(ruby: &Ruby, hash: RHash) -> Result<Self, Error> {
        let source = expand_path(ruby, &hash, "source", ".")?;
        let destination = expand_path(ruby, &hash, "destination", "_site")?;
        let host = fetch_string(ruby, &hash, "host")?.unwrap_or_else(|| "127.0.0.1".to_string());
        let port_raw = fetch_integer(ruby, &hash, "port")?.unwrap_or(4000);
        let port = u16::try_from(port_raw).unwrap_or(4000);
        let baseurl = fetch_string(ruby, &hash, "baseurl")?.filter(|s| !s.is_empty());
        let show_dir_listing = fetch_bool(ruby, &hash, "show_dir_listing")?.unwrap_or(false);
        let watch = fetch_bool(ruby, &hash, "watch")?.unwrap_or(false);
        let livereload = fetch_bool(ruby, &hash, "livereload")?.unwrap_or(false);
        let livereload_port = fetch_integer(ruby, &hash, "livereload_port")?
            .and_then(|v| u16::try_from(v).ok())
            .unwrap_or(DEFAULT_LIVERELOAD_PORT);
        let livereload_min_delay = fetch_integer(ruby, &hash, "livereload_min_delay")?
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0);
        let livereload_max_delay = fetch_integer(ruby, &hash, "livereload_max_delay")?
            .and_then(|v| u64::try_from(v).ok())
            .unwrap_or(0);
        let livereload_ignore = fetch_string_list(ruby, &hash, "livereload_ignore")?;
        let exclude = fetch_string_list(ruby, &hash, "exclude")?;
        let cache_dir = fetch_string(ruby, &hash, "cache_dir")?
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| ".jekyll-cache".to_string());
        let open_url = fetch_bool(ruby, &hash, "open_url")?.unwrap_or(false);
        let detach = fetch_bool(ruby, &hash, "detach")?.unwrap_or(false);
        let ssl_cert = fetch_string(ruby, &hash, "ssl_cert")?.filter(|s| !s.is_empty());
        let ssl_key = fetch_string(ruby, &hash, "ssl_key")?.filter(|s| !s.is_empty());

        Ok(Self {
            source,
            destination,
            host,
            port,
            baseurl,
            show_dir_listing,
            watch,
            livereload,
            livereload_port,
            livereload_min_delay,
            livereload_max_delay,
            livereload_ignore,
            exclude,
            cache_dir,
            open_url,
            detach,
            ssl_cert,
            ssl_key,
        })
    }
}

fn engine_serve_process(options: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let hash = RHash::from_value(options)
        .ok_or_else(|| Error::new(ruby.exception_type_error(), "serve options must be a hash"))?;

    let opts = ServeOptions::from_rhash(&ruby, hash)?;
    warn_unimplemented_features(&ruby, &opts)?;

    run_server(&ruby, opts, options)
}

fn run_server(ruby: &Ruby, opts: ServeOptions, options: Value) -> Result<(), Error> {
    ensure_time_required(ruby)?;

    std::fs::create_dir_all(&opts.destination).map_err(|err| {
        Error::new(
            ruby.exception_runtime_error(),
            format!(
                "failed to create destination {}: {err}",
                opts.destination.display()
            ),
        )
    })?;

    let logger = jekyll_logger(ruby)?;
    let normalized_base = opts
        .baseurl
        .as_ref()
        .and_then(|base| normalize_baseurl(base));
    let address_string = if let Some(base) = normalized_base.as_deref() {
        if base == "/" {
            format!("http://{}:{}", opts.host, opts.port)
        } else {
            format!("http://{}:{}{}", opts.host, opts.port, base)
        }
    } else {
        format!("http://{}:{}", opts.host, opts.port)
    };
    let _: Value = logger.funcall("info", ("Server address:", address_string.clone()))?;
    if opts.show_dir_listing {
        let _: Value = logger.funcall("info", ("Directory listings:", "enabled"))?;
    }
    let _: Value = logger.funcall("info", ("Server running:", "press Ctrl+C to stop"))?;

    if opts.open_url {
        if let Err(err) = launch_browser(&address_string) {
            let _: Value = logger.funcall(
                "warn",
                (
                    "Serve:",
                    format!("failed to open browser automatically: {err}"),
                ),
            )?;
        }
    }

    let livereload_cfg = if opts.livereload {
        Some(LiveReloadConfig::from_options(&opts))
    } else {
        None
    };

    if let Some(cfg) = &livereload_cfg {
        let _: Value = logger.funcall("info", ("LiveReload address:", cfg.log_url()))?;
    }

    let livereload_broadcaster = livereload_cfg.as_ref().map(|cfg| cfg.broadcaster.clone());
    let livereload_meta = livereload_cfg.as_ref().map(LivereloadMeta::new);
    set_livereload_handle(livereload_broadcaster.clone());

    let state = Arc::new(ServerState::new(
        &opts,
        normalized_base.clone(),
        livereload_meta,
    ));
    let watch_enabled = opts.watch;
    let addr = socket_addr_from(&opts.host, opts.port).map_err(|err| {
        Error::new(
            ruby.exception_arg_error(),
            format!("invalid host/port: {err}"),
        )
    })?;

    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_io()
        .enable_time()
        .worker_threads(2)
        .build()
        .map_err(|err| {
            Error::new(
                ruby.exception_runtime_error(),
                format!("failed to start runtime: {err}"),
            )
        })?;

    let watch_filters = WatchFilters::new(&opts);

    runtime.block_on(async move {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (rebuild_tx, mut rebuild_rx) = mpsc::channel::<Vec<PathBuf>>(8);

        let lr_handle = if let Some(cfg) = livereload_cfg.clone() {
            let rx = shutdown_rx.clone();
            Some(tokio::spawn(run_livereload(cfg, rx)))
        } else {
            None
        };

        let watcher_handle = if watch_enabled {
            let filters = watch_filters.clone();
            let rx = shutdown_rx.clone();
            let tx = rebuild_tx.clone();
            Some(tokio::spawn(run_file_watcher(filters, rx, tx)))
        } else {
            None
        };

        drop(rebuild_tx);

        let make_service = make_service_fn(move |_conn| {
            let state = state.clone();
            async move {
                Ok::<_, Infallible>(service_fn(move |req| {
                    let state = state.clone();
                    async move { Ok::<_, Infallible>(handle_request(req, state)) }
                }))
            }
        });

        let server = Server::bind(&addr).serve(make_service);
        let graceful = server.with_graceful_shutdown(shutdown_signal(shutdown_rx.clone()));
        tokio::pin!(graceful);

        let mut ctrl_c = Box::pin(async {
            let _ = tokio::signal::ctrl_c().await;
        });
        let mut shutting_down = false;

        loop {
            tokio::select! {
                res = &mut graceful => {
                    if let Err(err) = res {
                        eprintln!("serve error: {err}");
                    }
                    break;
                }
                _ = &mut ctrl_c, if !shutting_down => {
                    shutting_down = true;
                    let _ = shutdown_tx.send(true);
                }
                maybe_paths = rebuild_rx.recv(), if watch_enabled => {
                    if let Some(paths) = maybe_paths {
                        if let Err(err) = process_rebuild_event(options, &paths, livereload_cfg.as_ref()) {
                            eprintln!("auto-regeneration failed: {err}");
                        }
                    }
                }
            }
        }

        let _ = shutdown_tx.send(true);

        if let Some(handle) = lr_handle {
            let _ = handle.await;
        }
        if let Some(handle) = watcher_handle {
            let _ = handle.await;
        }
    });

    let _: Value = logger.funcall("info", ("Server exited:", address_string))?;
    set_livereload_handle(None);
    Ok(())
}

fn handle_request(req: Request<Body>, state: Arc<ServerState>) -> Response<Body> {
    if req.method() != Method::GET && req.method() != Method::HEAD {
        return Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .header("Allow", "GET, HEAD")
            .body(Body::from("Method not allowed"))
            .unwrap();
    }

    let path = req.uri().path();
    match state.resolve_path(path) {
        Some(ResolvedPath::File(file_path)) => match std::fs::read(&file_path) {
            Ok(mut bytes) => {
                let content_type = MimeGuess::from_path(&file_path).first_or_octet_stream();
                let mut builder = Response::builder().status(StatusCode::OK);
                if let Some(headers) = builder.headers_mut() {
                    let ct_value = HeaderValue::from_str(content_type.as_ref())
                        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
                    headers.insert(CONTENT_TYPE, ct_value);
                    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                }

                if req.method() == Method::GET {
                    if let Some(meta) = &state.livereload_meta {
                        if let Some(injected) =
                            maybe_inject_livereload(&bytes, meta.snippet(), content_type.as_ref())
                        {
                            bytes = injected;
                            if let Some(headers) = builder.headers_mut() {
                                headers.insert(
                                    HeaderName::from_static("x-rack-livereload"),
                                    HeaderValue::from_static("1"),
                                );
                            }
                        }
                    }
                }

                return finalize_response(&req, builder, bytes);
            }
            Err(_) => state.not_found(&req),
        },
        Some(ResolvedPath::DirectoryListing(mut html)) => {
            let mut builder = Response::builder().status(StatusCode::OK);
            if let Some(headers) = builder.headers_mut() {
                headers.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                );
                headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            }

            if let Some(meta) = &state.livereload_meta {
                html.push_str(meta.snippet());
                if let Some(headers) = builder.headers_mut() {
                    headers.insert(
                        HeaderName::from_static("x-rack-livereload"),
                        HeaderValue::from_static("1"),
                    );
                }
            }

            return finalize_response(&req, builder, html.into_bytes());
        }
        None => state.not_found(&req),
    }
}

fn warn_unimplemented_features(ruby: &Ruby, opts: &ServeOptions) -> Result<(), Error> {
    let logger = jekyll_logger(ruby)?;
    if opts.watch {
        let message = format!("enabled for {}", opts.source.display());
        let _: Value = logger.funcall("info", ("Auto-regeneration:", message))?;
    } else {
        let _: Value = logger.funcall(
            "info",
            ("Auto-regeneration:", "disabled. Use --watch to enable."),
        )?;
    }
    if opts.detach {
        let _: Value = logger.funcall(
            "warn",
            (
                "Serve:",
                "`--detach` is not available in the Rust server implementation.",
            ),
        )?;
    }
    if opts.ssl_cert.is_some() || opts.ssl_key.is_some() {
        let _: Value = logger.funcall(
            "warn",
            (
                "Serve:",
                "Ignoring --ssl-cert/--ssl-key; TLS support is not yet implemented.",
            ),
        )?;
    }
    Ok(())
}

fn jekyll_logger(ruby: &Ruby) -> Result<Value, Error> {
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    jekyll.funcall("logger", ())
}

fn fetch_bool(ruby: &Ruby, hash: &RHash, key: &str) -> Result<Option<bool>, Error> {
    let value: Value = hash.funcall("fetch", (ruby.str_new(key), ruby.qnil()))?;
    if value.is_nil() {
        Ok(None)
    } else {
        Ok(Some(value.to_bool()))
    }
}

fn fetch_integer(ruby: &Ruby, hash: &RHash, key: &str) -> Result<Option<i64>, Error> {
    let value: Value = hash.funcall("fetch", (ruby.str_new(key), ruby.qnil()))?;
    if value.is_nil() {
        Ok(None)
    } else {
        Ok(Some(i64::try_convert(value)?))
    }
}

fn fetch_string(ruby: &Ruby, hash: &RHash, key: &str) -> Result<Option<String>, Error> {
    let value: Value = hash.funcall("fetch", (ruby.str_new(key), ruby.qnil()))?;
    if value.is_nil() {
        Ok(None)
    } else {
        Ok(Some(String::try_convert(value)?))
    }
}

fn fetch_string_list(ruby: &Ruby, hash: &RHash, key: &str) -> Result<Vec<String>, Error> {
    let value: Value = hash.funcall("fetch", (ruby.str_new(key), ruby.qnil()))?;
    if value.is_nil() {
        return Ok(Vec::new());
    }

    if let Some(array) = RArray::from_value(value) {
        let mut out = Vec::with_capacity(array.len());
        for item in array.each() {
            let item = String::try_convert(item?)?;
            if !item.is_empty() {
                out.push(item);
            }
        }
        return Ok(out);
    }

    let raw = String::try_convert(value)?;
    let list = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    Ok(list)
}

fn collect_string_list(value: Value) -> Result<Vec<String>, Error> {
    if value.is_nil() {
        return Ok(Vec::new());
    }

    if let Some(array) = RArray::from_value(value) {
        let mut out = Vec::with_capacity(array.len());
        for item in array.each() {
            let item = String::try_convert(item?)?;
            if !item.is_empty() {
                out.push(item);
            }
        }
        return Ok(out);
    }

    let raw = String::try_convert(value)?;
    if raw.is_empty() {
        Ok(Vec::new())
    } else {
        Ok(vec![raw])
    }
}

fn expand_path(ruby: &Ruby, hash: &RHash, key: &str, default: &str) -> Result<PathBuf, Error> {
    let file: Value = ruby.class_object().const_get("File")?;
    let fetched: Value = hash.funcall("fetch", (ruby.str_new(key), ruby.qnil()))?;
    let value = if fetched.is_nil() {
        ruby.str_new(default).into_value_with(ruby)
    } else {
        fetched
    };
    let expanded: Value = file.funcall("expand_path", (value,))?;
    let path = String::try_convert(expanded)?;
    Ok(PathBuf::from(path))
}

fn set_livereload_handle(handle: Option<LiveReloadBroadcaster>) {
    let mut guard = LIVE_RELOAD_HANDLE
        .lock()
        .expect("live reload broadcaster mutex poisoned");
    *guard = handle;
}

#[derive(Clone)]
struct WatchFilters {
    source: PathBuf,
    absolute_ignores: Vec<PathBuf>,
    glob_ignores: Arc<Vec<GlobMatcher>>,
}

impl WatchFilters {
    fn new(opts: &ServeOptions) -> Self {
        let mut absolute_ignores = Vec::new();
        absolute_ignores.push(opts.destination.clone());

        let cache_path = if Path::new(&opts.cache_dir).is_absolute() {
            PathBuf::from(&opts.cache_dir)
        } else {
            opts.source.join(&opts.cache_dir)
        };
        absolute_ignores.push(cache_path.clone());
        absolute_ignores.push(opts.source.join(".jekyll-metadata"));
        absolute_ignores.push(opts.source.join(".sass-cache"));
        absolute_ignores.push(opts.source.join("node_modules"));
        absolute_ignores.push(opts.source.join("vendor"));

        let mut glob_patterns: Vec<String> = opts.exclude.clone();

        if let Some(rel) = relative_path_string(&opts.source, &opts.destination) {
            if !rel.is_empty() {
                glob_patterns.push(format!("{rel}/**"));
            }
        }

        if let Some(rel) = relative_path_string(&opts.source, &cache_path) {
            if !rel.is_empty() {
                glob_patterns.push(format!("{rel}/**"));
            }
        }

        glob_patterns.push(".jekyll-metadata".to_string());
        glob_patterns.push(".sass-cache/**".to_string());
        glob_patterns.push("node_modules/**".to_string());
        glob_patterns.push("vendor/**".to_string());

        let glob_ignores = glob_patterns
            .into_iter()
            .filter_map(|pattern| {
                GlobBuilder::new(&pattern)
                    .literal_separator(true)
                    .backslash_escape(true)
                    .build()
                    .ok()
                    .map(|glob| glob.compile_matcher())
            })
            .collect();

        Self {
            source: opts.source.clone(),
            absolute_ignores,
            glob_ignores: Arc::new(glob_ignores),
        }
    }

    fn should_ignore(&self, path: &Path) -> bool {
        if self
            .absolute_ignores
            .iter()
            .any(|ignore| path.starts_with(ignore))
        {
            return true;
        }

        if let Ok(rel) = path.strip_prefix(&self.source) {
            if let Some(rel_str) = normalize_relative_string(rel) {
                if self
                    .glob_ignores
                    .iter()
                    .any(|glob| glob.is_match(rel_str.as_str()))
                {
                    return true;
                }
            }
        }

        false
    }

    fn relative_for(&self, path: &Path) -> Option<PathBuf> {
        let rel = path.strip_prefix(&self.source).ok()?;
        let mut out = PathBuf::new();
        let mut has_component = false;
        for component in rel.components() {
            match component {
                Component::CurDir => {}
                Component::Normal(part) => {
                    out.push(part);
                    has_component = true;
                }
                _ => return None,
            }
        }
        if has_component {
            Some(out)
        } else {
            None
        }
    }
}

struct ServerState {
    destination: PathBuf,
    base_prefix: Option<String>,
    show_dir_listing: bool,
    not_found_path: Option<PathBuf>,
    livereload_meta: Option<LivereloadMeta>,
}

impl ServerState {
    fn new(
        opts: &ServeOptions,
        base_prefix: Option<String>,
        livereload_meta: Option<LivereloadMeta>,
    ) -> Self {
        let not_found = opts.destination.join("404.html");
        let not_found_path = if not_found.exists() {
            Some(not_found)
        } else {
            None
        };
        Self {
            destination: opts.destination.clone(),
            base_prefix,
            show_dir_listing: opts.show_dir_listing,
            not_found_path,
            livereload_meta,
        }
    }

    fn resolve_path(&self, request_path: &str) -> Option<ResolvedPath> {
        let mut path = request_path;
        if let Some(prefix) = &self.base_prefix {
            if prefix != "/" {
                if let Some(stripped) = path.strip_prefix(prefix) {
                    path = if stripped.is_empty() { "/" } else { stripped };
                } else {
                    return None;
                }
            }
        }

        let decoded = percent_decode_str(path).decode_utf8().ok()?;
        let sanitized = sanitize_path(&decoded)?;
        let full_path = self.destination.join(&sanitized);

        if full_path.is_file() {
            return Some(ResolvedPath::File(full_path));
        }
        if full_path.is_dir() {
            for candidate in DIRECTORY_INDEX.iter() {
                let candidate_path = full_path.join(candidate);
                if candidate_path.is_file() {
                    return Some(ResolvedPath::File(candidate_path));
                }
            }
            if self.show_dir_listing {
                return Some(ResolvedPath::DirectoryListing(render_directory_listing(
                    &full_path, path,
                )));
            }
        }
        None
    }

    fn not_found(&self, req: &Request<Body>) -> Response<Body> {
        if let Some(path) = &self.not_found_path {
            if let Ok(bytes) = std::fs::read(path) {
                let mut builder = Response::builder().status(StatusCode::NOT_FOUND);
                if let Some(headers) = builder.headers_mut() {
                    headers.insert(
                        CONTENT_TYPE,
                        HeaderValue::from_static("text/html; charset=utf-8"),
                    );
                    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
                }
                return finalize_response(req, builder, bytes);
            }
        }

        let mut builder = Response::builder().status(StatusCode::NOT_FOUND);
        if let Some(headers) = builder.headers_mut() {
            headers.insert(
                CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        }
        finalize_response(req, builder, b"404 Not Found".to_vec())
    }
}

enum ResolvedPath {
    File(PathBuf),
    DirectoryListing(String),
}

fn sanitize_path(input: &str) -> Option<PathBuf> {
    let mut result = PathBuf::new();
    let mut comps = Path::new(input).components();
    while let Some(comp) = comps.next() {
        match comp {
            Component::Prefix(_) | Component::RootDir => {}
            Component::CurDir => {}
            Component::ParentDir => {
                return None;
            }
            Component::Normal(part) => {
                result.push(part);
            }
        }
    }
    Some(result)
}

fn relative_path_string(base: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(base).ok()?;
    normalize_relative_string(rel)
}

fn normalize_relative_string(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => return None,
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn render_directory_listing(dir: &Path, request_path: &str) -> String {
    let mut entries = Vec::new();
    if let Ok(read) = std::fs::read_dir(dir) {
        for entry in read.flatten() {
            if let Ok(meta) = entry.metadata() {
                let name = entry.file_name().to_string_lossy().to_string();
                let display_name = if meta.is_dir() {
                    format!("{}/", name)
                } else {
                    name
                };
                entries.push(display_name);
            }
        }
    }
    entries.sort();
    let mut html = String::new();
    html.push_str("<html><head><title>Index of ");
    html.push_str(request_path);
    html.push_str("</title></head><body>");
    html.push_str("<h1>Index of ");
    html.push_str(request_path);
    html.push_str("</h1><ul>");
    if request_path != "/" {
        html.push_str("<li><a href=\"../\">../</a></li>");
    }
    for entry in entries {
        html.push_str("<li><a href=\"");
        html.push_str(&entry);
        html.push_str("\">");
        html.push_str(&entry);
        html.push_str("</a></li>");
    }
    html.push_str("</ul></body></html>");
    html
}

fn livereload_reload(paths: Value) -> Result<(), Error> {
    let paths = collect_string_list(paths)?;
    fire_livereload_events(&paths)
}

#[derive(Clone)]
struct LiveReloadBroadcaster {
    sender: broadcast::Sender<LiveReloadEvent>,
}

#[derive(Clone, Debug)]
enum LiveReloadEvent {
    Reload { path: String },
}

impl LiveReloadBroadcaster {
    fn new() -> Self {
        let (sender, _) = broadcast::channel(64);
        Self { sender }
    }

    fn subscribe(&self) -> broadcast::Receiver<LiveReloadEvent> {
        self.sender.subscribe()
    }

    fn trigger_reload<S: Into<String>>(&self, path: S) {
        let _ = self
            .sender
            .send(LiveReloadEvent::Reload { path: path.into() });
    }
}

#[derive(Clone)]
struct LiveReloadConfig {
    host: String,
    port: u16,
    min_delay: u64,
    max_delay: u64,
    ignore_matchers: Arc<Vec<GlobMatcher>>,
    broadcaster: LiveReloadBroadcaster,
}

impl LiveReloadConfig {
    fn from_options(opts: &ServeOptions) -> Self {
        let ignore_matchers = opts
            .livereload_ignore
            .iter()
            .filter_map(|pattern| {
                GlobBuilder::new(pattern)
                    .literal_separator(false)
                    .backslash_escape(true)
                    .build()
                    .ok()
                    .map(|glob| glob.compile_matcher())
            })
            .collect();
        Self {
            host: opts.host.clone(),
            port: opts.livereload_port,
            min_delay: opts.livereload_min_delay,
            max_delay: opts.livereload_max_delay,
            ignore_matchers: Arc::new(ignore_matchers),
            broadcaster: LiveReloadBroadcaster::new(),
        }
    }

    fn bind_addr(&self) -> String {
        if self.host.contains(':') && !self.host.contains(']') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    fn log_url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{}:{}", host, self.port)
    }

    fn should_ignore(&self, path: &str) -> bool {
        self.ignore_matchers.iter().any(|glob| glob.is_match(path))
    }

    fn should_ignore_relative(&self, relative: &str) -> bool {
        self.ignore_matchers
            .iter()
            .any(|glob| glob.is_match(relative))
    }
}

#[derive(Clone)]
struct LivereloadMeta {
    snippet: Arc<String>,
}

impl LivereloadMeta {
    fn new(cfg: &LiveReloadConfig) -> Self {
        Self {
            snippet: Arc::new(build_livereload_snippet(cfg)),
        }
    }

    fn snippet(&self) -> &str {
        &self.snippet
    }
}

async fn run_livereload(config: LiveReloadConfig, mut shutdown: watch::Receiver<bool>) {
    let bind_addr = config.bind_addr();
    let listener = match TcpListener::bind(&bind_addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("livereload bind error on {}: {}", bind_addr, err);
            return;
        }
    };

    loop {
        tokio::select! {
            accept = listener.accept() => {
                match accept {
                    Ok((stream, addr)) => {
                        let cfg = config.clone();
                        let mut rx = shutdown.clone();
                        tokio::spawn(async move {
                            if let Err(err) = handle_livereload_connection(stream, addr, cfg, &mut rx).await {
                                eprintln!("livereload connection error: {}", err);
                            }
                        });
                    }
                    Err(err) => {
                        eprintln!("livereload accept error: {}", err);
                    }
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
        }
    }
}

async fn run_file_watcher(
    filters: WatchFilters,
    mut shutdown: watch::Receiver<bool>,
    rebuild_tx: mpsc::Sender<Vec<PathBuf>>,
) {
    let (event_tx, mut event_rx) = mpsc::channel::<Vec<PathBuf>>(64);
    let (stop_tx, stop_rx) = sync_mpsc::channel();

    let watcher_thread = {
        let source = filters.source.clone();
        let tx = event_tx.clone();
        std::thread::spawn(move || {
            let handler = move |res: Result<Event, notify::Error>| match res {
                Ok(event) => {
                    if matches!(
                        event.kind,
                        EventKind::Modify(_)
                            | EventKind::Create(_)
                            | EventKind::Remove(_)
                            | EventKind::Other
                            | EventKind::Any
                    ) {
                        if !event.paths.is_empty() {
                            let _ = tx.blocking_send(event.paths);
                        }
                    }
                }
                Err(err) => eprintln!("file watcher error: {}", err),
            };

            let mut watcher = match RecommendedWatcher::new(handler, notify::Config::default()) {
                Ok(watcher) => watcher,
                Err(err) => {
                    eprintln!("failed to start file watcher: {}", err);
                    return;
                }
            };

            if let Err(err) = watcher.watch(source.as_path(), RecursiveMode::Recursive) {
                eprintln!("failed to watch {}: {}", source.display(), err);
                return;
            }

            let _ = stop_rx.recv();
        })
    };
    drop(event_tx);

    let mut pending: HashSet<PathBuf> = HashSet::new();
    let mut debounce: Option<Pin<Box<tokio::time::Sleep>>> = None;

    loop {
        tokio::select! {
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    break;
                }
            }
            maybe_paths = event_rx.recv() => {
                match maybe_paths {
                    Some(paths) => {
                        for path in paths {
                            if filters.should_ignore(&path) {
                                continue;
                            }
                            if let Some(rel) = filters.relative_for(&path) {
                                pending.insert(rel);
                            }
                        }
                        if !pending.is_empty() {
                            debounce = Some(Box::pin(tokio::time::sleep(Duration::from_millis(250))));
                        }
                    }
                    None => break,
                }
            }
            _ = async {
                if let Some(ref mut sleep) = debounce {
                    sleep.as_mut().await;
                }
            }, if debounce.is_some() => {
                debounce = None;
                if !pending.is_empty() {
                    let mut paths: Vec<PathBuf> = pending.drain().collect();
                    paths.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
                    if rebuild_tx.send(paths).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    let _ = stop_tx.send(());
    if let Err(err) = watcher_thread.join() {
        eprintln!("file watcher thread join error: {:?}", err);
    }
}

async fn handle_livereload_connection(
    stream: tokio::net::TcpStream,
    _addr: SocketAddr,
    config: LiveReloadConfig,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut ws = accept_async(stream).await?;

    let hello = serde_json::json!({
        "command": "hello",
        "protocols": ["http://livereload.com/protocols/official-7"],
        "serverName": "jekyllrs",
    });
    ws.send(Message::Text(hello.to_string())).await?;

    let mut reload_rx = config.broadcaster.subscribe();

    loop {
        tokio::select! {
            msg = ws.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
                            if value.get("command").and_then(|c| c.as_str()) == Some("ping") {
                                ws.send(Message::Text("{\"command\":\"pong\"}".into())).await?;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        ws.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Err(err)) => {
                        return Err(Box::new(err));
                    }
                    None => break,
                    _ => {}
                }
            }
            event = reload_rx.recv() => {
                if let Ok(LiveReloadEvent::Reload { path }) = event {
                    if config.should_ignore(&path) {
                        continue;
                    }

                    let mut payload = serde_json::json!({
                        "command": "reload",
                        "path": path,
                        "liveCSS": true,
                        "liveImg": true,
                    });
                    if config.min_delay != 0 {
                        payload["mindelay"] = serde_json::json!(config.min_delay);
                    }
                    if config.max_delay != 0 {
                        payload["maxdelay"] = serde_json::json!(config.max_delay);
                    }
                    ws.send(Message::Text(payload.to_string())).await?;
                }
            }
            _ = shutdown.changed() => {
                if *shutdown.borrow() {
                    let _ = ws.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    }

    Ok(())
}

fn socket_addr_from(host: &str, port: u16) -> Result<SocketAddr, std::net::AddrParseError> {
    match host.parse::<IpAddr>() {
        Ok(ip) => Ok(SocketAddr::new(ip, port)),
        Err(_) => Ok(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port)),
    }
}

fn normalize_baseurl(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut normalized = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{}", trimmed)
    };
    while normalized.ends_with('/') && normalized.len() > 1 {
        normalized.pop();
    }
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

async fn shutdown_signal(mut rx: watch::Receiver<bool>) {
    if *rx.borrow() {
        return;
    }
    while rx.changed().await.is_ok() {
        if *rx.borrow() {
            break;
        }
    }
}

fn process_rebuild_event(
    options: Value,
    paths: &[PathBuf],
    livereload_cfg: Option<&LiveReloadConfig>,
) -> Result<(), Error> {
    log_watch_summary(paths);
    let build_result = task::block_in_place(|| crate::cli_build::engine_build_process(options))?;

    if let Some(cfg) = livereload_cfg {
        let entries = parse_livereload_entries(build_result)?;
        handle_livereload_updates(cfg, &entries)?;
    }

    Ok(())
}

fn log_watch_summary(paths: &[PathBuf]) {
    if paths.is_empty() {
        return;
    }

    if let Ok(ruby) = ruby_handle() {
        if let Ok(logger) = jekyll_logger(&ruby) {
            if let Some(summary) = summarize_changed_paths(paths) {
                let _ = logger.funcall::<_, _, Value>("info", ("Watch:", summary));
            }
        }
    }
}

fn summarize_changed_paths(paths: &[PathBuf]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }

    let mut entries: Vec<String> = paths
        .iter()
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .collect();
    entries.sort();

    let message = match entries.len() {
        1 => format!("{} changed", entries[0]),
        2 => format!("{} and {} changed", entries[0], entries[1]),
        count => format!(
            "{}, {} and {} more changed",
            entries[0],
            entries[1],
            count - 2
        ),
    };

    Some(message)
}

struct LiveReloadEntry {
    relative_path: String,
    url: Option<String>,
}

fn parse_livereload_entries(value: Value) -> Result<Vec<LiveReloadEntry>, Error> {
    if value.is_nil() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    let Some(array) = RArray::from_value(value) else {
        return Ok(entries);
    };

    for item in array.each() {
        let entry = item?;
        let Some(tuple) = RArray::from_value(entry) else {
            continue;
        };
        if tuple.len() == 0 {
            continue;
        }

        let relative_path: String = tuple.entry(0)?;
        let url = if tuple.len() > 1 {
            tuple.entry::<Option<String>>(1)?
        } else {
            None
        };

        entries.push(LiveReloadEntry { relative_path, url });
    }

    Ok(entries)
}

fn handle_livereload_updates(
    cfg: &LiveReloadConfig,
    entries: &[LiveReloadEntry],
) -> Result<(), Error> {
    if entries.is_empty() {
        return Ok(());
    }

    let mut ignored = Vec::new();
    let mut to_reload = Vec::new();

    for entry in entries {
        if cfg.should_ignore_relative(entry.relative_path.as_str()) {
            ignored.push(entry.relative_path.clone());
            continue;
        }

        if let Some(url) = &entry.url {
            to_reload.push(url.clone());
        }
    }

    if !ignored.is_empty() {
        log_livereload_ignored(&ignored)?;
    }

    fire_livereload_events(&to_reload)
}

fn log_livereload_ignored(paths: &[String]) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }

    let ruby = ruby_handle()?;
    let logger = jekyll_logger(&ruby)?;
    let message = format!("Ignoring {:?}", paths);
    let _: Value = logger.funcall("debug", ("LiveReload:", message))?;
    Ok(())
}

fn fire_livereload_events(paths: &[String]) -> Result<(), Error> {
    if paths.is_empty() {
        return Ok(());
    }

    let ruby = ruby_handle()?;
    let logger = jekyll_logger(&ruby)?;
    let broadcaster = {
        let guard = LIVE_RELOAD_HANDLE
            .lock()
            .expect("live reload broadcaster mutex poisoned");
        guard.clone()
    };

    let Some(broadcaster) = broadcaster else {
        let _: Value = logger.funcall(
            "debug",
            ("LiveReload:", "No active server; ignoring reload request."),
        )?;
        return Ok(());
    };

    for path in paths {
        broadcaster.trigger_reload(path.clone());
    }

    Ok(())
}

fn build_livereload_snippet(cfg: &LiveReloadConfig) -> String {
    let mut args = String::new();
    if cfg.min_delay != 0 {
        args.push_str(&format!("&mindelay={}", cfg.min_delay));
    }
    if cfg.max_delay != 0 {
        args.push_str(&format!("&maxdelay={}", cfg.max_delay));
    }

    format!(
        "<script>\n  document.write('<script src=\"' + location.protocol + '//' +\n    (location.host || 'localhost').split(':')[0] +\n    ':{port}/livereload.js?snipver=1{args}\"' +\n    '></' + 'script>');\n</script>\n",
        port = cfg.port,
        args = args
    )
}

fn maybe_inject_livereload(bytes: &[u8], snippet: &str, content_type: &str) -> Option<Vec<u8>> {
    if !is_html_mime(content_type) {
        return None;
    }

    let mut html = String::from_utf8(bytes.to_vec()).ok()?;

    if let Some(head_pos) = find_case_insensitive(&html, "<head") {
        if let Some(insert_pos) = find_tag_close(&html, head_pos) {
            html.insert_str(insert_pos, snippet);
            return Some(html.into_bytes());
        }
    }

    if let Some(body_pos) = find_case_insensitive(&html, "</body>") {
        html.insert_str(body_pos, snippet);
    } else {
        html.push_str(snippet);
    }

    Some(html.into_bytes())
}

fn finalize_response(
    req: &Request<Body>,
    mut builder: hyper::http::response::Builder,
    bytes: Vec<u8>,
) -> Response<Body> {
    let encoding = negotiate_encoding(req.headers());
    let (encoded, encoding_header) = encode_bytes(bytes, encoding);

    if let Some(headers) = builder.headers_mut() {
        if let Some(header_value) = encoding_header {
            headers.insert(CONTENT_ENCODING, HeaderValue::from_static(header_value));
        }
        headers.insert(VARY, HeaderValue::from_static("Accept-Encoding"));
    }

    let length = encoded.len();
    if req.method() == Method::HEAD {
        if let Some(headers) = builder.headers_mut() {
            if let Ok(value) = HeaderValue::from_str(&length.to_string()) {
                headers.insert(CONTENT_LENGTH, value);
            }
        }
        builder.body(Body::empty()).unwrap()
    } else {
        builder.body(Body::from(encoded)).unwrap()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectedEncoding {
    Identity,
    Brotli,
    Gzip,
    Deflate,
}

fn negotiate_encoding(headers: &hyper::HeaderMap<HeaderValue>) -> SelectedEncoding {
    let value = headers
        .get(ACCEPT_ENCODING)
        .and_then(|val| val.to_str().ok())
        .unwrap_or("");

    let mut q_br = -1.0f32;
    let mut q_gzip = -1.0f32;
    let mut q_deflate = -1.0f32;
    let mut q_identity = 1.0f32;

    for raw in value.split(',') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split(';');
        let token = parts
            .next()
            .map(|t| t.trim().to_ascii_lowercase())
            .unwrap_or_default();
        let mut q = 1.0f32;
        for param in parts {
            let param = param.trim();
            if let Some(rest) = param.strip_prefix("q=") {
                if let Ok(parsed) = rest.trim().parse::<f32>() {
                    q = parsed;
                }
            }
        }

        match token.as_str() {
            "br" => q_br = q,
            "gzip" | "x-gzip" => q_gzip = q,
            "deflate" => q_deflate = q,
            "identity" => q_identity = q,
            "*" => {
                if q_br < 0.0 {
                    q_br = q;
                }
                if q_gzip < 0.0 {
                    q_gzip = q;
                }
                if q_deflate < 0.0 {
                    q_deflate = q;
                }
                q_identity = q_identity.min(q);
            }
            _ => {}
        }
    }

    if q_br < 0.0 {
        q_br = 1.0;
    }
    if q_gzip < 0.0 {
        q_gzip = 1.0;
    }
    if q_deflate < 0.0 {
        q_deflate = 1.0;
    }

    let mut best = SelectedEncoding::Identity;
    let mut best_q = if q_identity.is_sign_negative() {
        0.0
    } else {
        q_identity.max(0.0)
    };

    for (encoding, q) in [
        (SelectedEncoding::Brotli, q_br),
        (SelectedEncoding::Gzip, q_gzip),
        (SelectedEncoding::Deflate, q_deflate),
    ] {
        if q > best_q && q > 0.0 {
            best = encoding;
            best_q = q;
        }
    }

    if best_q <= 0.0 {
        SelectedEncoding::Identity
    } else {
        best
    }
}

fn encode_bytes(bytes: Vec<u8>, encoding: SelectedEncoding) -> (Vec<u8>, Option<&'static str>) {
    match encoding {
        SelectedEncoding::Identity => (bytes, None),
        SelectedEncoding::Brotli => match brotli_compress(&bytes) {
            Ok(out) => (out, Some("br")),
            Err(_) => (bytes, None),
        },
        SelectedEncoding::Gzip => match gzip_compress(&bytes) {
            Ok(out) => (out, Some("gzip")),
            Err(_) => (bytes, None),
        },
        SelectedEncoding::Deflate => match deflate_compress(&bytes) {
            Ok(out) => (out, Some("deflate")),
            Err(_) => (bytes, None),
        },
    }
}

fn gzip_compress(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input)?;
    encoder.finish()
}

fn deflate_compress(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(input)?;
    encoder.finish()
}

fn brotli_compress(input: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut params = BrotliEncoderParams::default();
    params.quality = 5;
    let mut output = Vec::new();
    let mut reader = Cursor::new(input);
    BrotliCompress(&mut reader, &mut output, &params).map(|_| output)
}

fn launch_browser(url: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let status = Command::new("cmd")
            .args(["/C", "start", "", url])
            .status()?;
        if status.success() {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "failed to launch browser",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let status = Command::new("open").arg(url).status()?;
        if status.success() {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "failed to launch browser",
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let status = Command::new("xdg-open").arg(url).status()?;
        if status.success() {
            return Ok(());
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "failed to launch browser",
        ));
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = url;
        Err(io::Error::new(
            io::ErrorKind::Other,
            "automatic browser launch is not supported on this platform",
        ))
    }
}

fn is_html_mime(content_type: &str) -> bool {
    let lowered = content_type.to_ascii_lowercase();
    lowered.starts_with("text/html") || lowered.starts_with("application/xhtml")
}

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let h_bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    if n_bytes.is_empty() {
        return Some(0);
    }

    h_bytes
        .windows(n_bytes.len())
        .position(|window| eq_ignore_ascii_case(window, n_bytes))
}

fn eq_ignore_ascii_case(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| x.to_ascii_lowercase() == y.to_ascii_lowercase())
}

fn find_tag_close(input: &str, start: usize) -> Option<usize> {
    input[start..].find('>').map(|offset| start + offset + 1)
}
