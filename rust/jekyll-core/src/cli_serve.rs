use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use globset::{GlobBuilder, GlobMatcher};
use hyper::header::{HeaderName, HeaderValue, CACHE_CONTROL, CONTENT_TYPE};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Method, Request, Response, Server, StatusCode};
use magnus::{
    function, prelude::*, Error, IntoValue, RArray, RHash, RModule, Ruby, TryConvert, Value,
};
use mime_guess::MimeGuess;
use once_cell::sync::Lazy;
use percent_encoding::percent_decode_str;
use tokio::net::TcpListener;
use tokio::runtime::Builder as RuntimeBuilder;
use tokio::sync::{broadcast, watch};
use tokio_tungstenite::{accept_async, tungstenite::protocol::Message};

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
    open_url: bool,
    detach: bool,
    ssl_cert: Option<String>,
    ssl_key: Option<String>,
}

impl ServeOptions {
    fn from_rhash(ruby: &Ruby, hash: RHash) -> Result<Self, Error> {
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
        let open_url = fetch_bool(ruby, &hash, "open_url")?.unwrap_or(false);
        let detach = fetch_bool(ruby, &hash, "detach")?.unwrap_or(false);
        let ssl_cert = fetch_string(ruby, &hash, "ssl_cert")?.filter(|s| !s.is_empty());
        let ssl_key = fetch_string(ruby, &hash, "ssl_key")?.filter(|s| !s.is_empty());

        Ok(Self {
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

    run_server(&ruby, opts)
}

fn run_server(ruby: &Ruby, opts: ServeOptions) -> Result<(), Error> {
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

    runtime.block_on(async move {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let lr_handle = if let Some(cfg) = livereload_cfg.clone() {
            let rx = shutdown_rx.clone();
            Some(tokio::spawn(run_livereload(cfg, rx)))
        } else {
            None
        };

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

        let ctrl_c = async {
            let _ = tokio::signal::ctrl_c().await;
            let _ = shutdown_tx.send(true);
        };

        tokio::select! {
            res = &mut graceful => {
                if let Err(err) = res {
                    eprintln!("serve error: {err}");
                }
            }
            _ = ctrl_c => {
                if let Err(err) = graceful.await {
                    eprintln!("serve error: {err}");
                }
            }
        }

        let _ = shutdown_tx.send(true);

        if let Some(handle) = lr_handle {
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

                if req.method() == Method::HEAD {
                    builder.body(Body::empty()).unwrap()
                } else {
                    builder.body(Body::from(bytes)).unwrap()
                }
            }
            Err(_) => state.not_found(),
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

            if req.method() == Method::HEAD {
                builder.body(Body::empty()).unwrap()
            } else {
                builder.body(Body::from(html)).unwrap()
            }
        }
        None => state.not_found(),
    }
}

fn warn_unimplemented_features(ruby: &Ruby, opts: &ServeOptions) -> Result<(), Error> {
    let logger = jekyll_logger(ruby)?;
    if opts.watch {
        let _: Value = logger.funcall(
            "warn",
            (
                "Auto-regeneration:",
                "watch flag acknowledged but in-progress; automatic rebuilds are currently disabled.",
            ),
        )?;
    }
    if opts.open_url {
        let _: Value = logger.funcall(
            "warn",
            (
                "Serve:",
                "`--open-url` is not yet implemented in the Rust CLI.",
            ),
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

    fn not_found(&self) -> Response<Body> {
        if let Some(path) = &self.not_found_path {
            if let Ok(bytes) = std::fs::read(path) {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .header("Content-Type", "text/html; charset=utf-8")
                    .body(Body::from(bytes))
                    .unwrap();
            }
        }
        Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header("Content-Type", "text/plain; charset=utf-8")
            .body(Body::from("404 Not Found"))
            .unwrap()
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
    let ruby = ruby_handle()?;
    let (logger, broadcaster) = {
        let logger = jekyll_logger(&ruby)?;
        let guard = LIVE_RELOAD_HANDLE
            .lock()
            .expect("live reload broadcaster mutex poisoned");
        (logger, guard.clone())
    };

    let Some(broadcaster) = broadcaster else {
        let _: Value = logger.funcall(
            "debug",
            ("LiveReload:", "No active server; ignoring reload request."),
        )?;
        return Ok(());
    };

    let paths = collect_string_list(paths)?;
    if paths.is_empty() {
        return Ok(());
    }

    for path in paths {
        broadcaster.trigger_reload(path);
    }

    Ok(())
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
