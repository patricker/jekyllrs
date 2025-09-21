use magnus::{eval, exception, prelude::*, Error, RHash, RModule, Value};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

fn locate_rust_lib() -> Option<PathBuf> {
    if let Some(v) = env::var_os("JEKYLL_RUST_LIB") {
        return Some(PathBuf::from(v));
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join("libjekyll_core.so");
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

fn set_env_defaults() {
    env::set_var("JEKYLL_RS", "1");
    if env::var_os("FORCE_COLOR").is_none() {
        env::set_var("FORCE_COLOR", "1");
    }
    if let Some(lib) = locate_rust_lib() {
        env::set_var("JEKYLL_RUST_LIB", lib);
    }
}

fn ensure_ruby_load_path_for_gemfile() -> Result<(), Error> {
    // If a Gemfile exists in CWD, mimic `bundle exec` to get correct load paths.
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let gemfile = cwd.join("Gemfile");
    if gemfile.exists() {
        // Set BUNDLE_GEMFILE to prefer this Gemfile and require bundler/setup
        env::set_var("BUNDLE_GEMFILE", gemfile);
        let _ = eval::<Value>("begin; require 'bundler/setup'; rescue LoadError; end");
    }
    Ok(())
}
fn print_help_global() {
    println!("Usage: jekyllrs <subcommand> [options]");
    println!("\nSubcommands:");
    println!("  build    Build your site");
    println!("  clean    Clean the site output and metadata");
    println!("  serve    Serve your site locally");
}

fn print_help_build() {
    println!("Usage: jekyllrs build [options]\n");
    println!("Options:");
    println!("    -s, --source [DIR]             Source directory (defaults to ./)");
    println!("    -d, --destination [DIR]        Destination directory (defaults to ./_site)");
    println!("        --safe                     Safe mode (defaults to false)");
    println!("    -p, --plugins PLUGINS_DIRS     Plugins directory (defaults to ./_plugins)");
    println!("        --layouts DIR              Layouts directory (defaults to ./_layouts)");
    println!("        --profile                  Generate a Liquid rendering profile");
    println!("    -I, --incremental              Enable incremental rebuild");
    println!("    -w, --watch                    Watch for changes and rebuild");
    println!("        --trace                    Show full backtrace on errors");
}

fn strip_trace_flag(argv: &[String]) -> (Vec<String>, bool) {
    let mut out = Vec::with_capacity(argv.len());
    let mut trace = false;
    for a in argv {
        if a == "--trace" { trace = true; } else { out.push(a.clone()); }
    }
    (out, trace)
}

fn run_build(args: &[String], trace: bool) -> Result<(), Error> {
    // Require jekyll library and call build via Command.process_with_graceful_fail
    eval::<Value>("require 'jekyll'")?;
    // Make stdout synchronous for real-time logs.
    let _ = eval::<Value>("STDOUT.sync = true; STDERR.sync = true");

    let options = parse_build_args(args)?;

    let jekyll: RModule = eval("Jekyll")?;
    let rust_mod: RModule = jekyll.const_get("Rust")?;
    let res = rust_mod.funcall::<_,_,Value>("engine_build_process", (options,));
    match res {
        Ok(_) => Ok(()),
        Err(e) => {
            if trace { return Err(e); }
            let _ = eval::<Value>(r#"
              msg = " Please append `--trace` to the `build` command "
              dashes = "-" * msg.length
              Jekyll.logger.error "", dashes
              Jekyll.logger.error "Jekyll #{Jekyll::VERSION} ", msg
              Jekyll.logger.error "", " for any additional information or backtrace. "
              Jekyll.logger.abort_with "", dashes
            "#);
            // Return original error to signal failure
            Err(e)
        }
    }?;
    Ok(())
}

fn run_core(argv: Vec<String>) -> Result<(), Error> {
    // Prepare Ruby load path to respect Gemfile if present.
    ensure_ruby_load_path_for_gemfile()?;

    let (argv, trace) = strip_trace_flag(&argv);
    let mut args = argv;
    let sub = if args.is_empty() { String::from("build") } else { args.remove(0) };
    match sub.as_str() {
        "build" | "b" => {
            if args.iter().any(|a| a == "-h" || a == "--help" || a == "help") {
                print_help_build();
                Ok(())
            } else {
                run_build(&args, trace)
            }
        }
        "-v" | "--version" | "version" => {
            eval::<Value>("require 'jekyll'; puts Jekyll::VERSION").map(|_| ())
        }
        "-h" | "--help" | "help" => {
            print_help_global();
            Ok(())
        }
        "clean" => {
            if args.iter().any(|a| a == "-h" || a == "--help" || a == "help") {
                println!("Usage: jekyllrs clean [options]
");
                println!("Options: same as build for config selection");
                Ok(())
            } else {
                eval::<Value>("require 'jekyll'")?;
                let options = parse_build_args(&args)?;
                let jekyll: RModule = eval("Jekyll")?;
                let rust_mod: RModule = jekyll.const_get("Rust")?;
                let _: Value = rust_mod.funcall("engine_clean_process", (options,))?;
                Ok(())
            }
        }
        "serve" | "s" | "server" => {
            if args.iter().any(|a| a == "-h" || a == "--help" || a == "help") {
                print_help_serve();
                Ok(())
            } else {
                run_serve(&args, trace)
            }
        }
        other => {

            eprintln!("unsupported subcommand: {} (supported: build|clean|serve)", other);
            Err(Error::new(exception::arg_error(), "unsupported subcommand"))
        }
    }
}

fn main() -> ExitCode {
    set_env_defaults();
    let argv: Vec<String> = env::args().skip(1).collect();
    let code = unsafe {
        let _cleanup = magnus::embed::init();
        match run_core(argv) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {}", e);
                1
            }
        }
    };
    ExitCode::from(code)
}
fn parse_build_args(args: &[String]) -> Result<RHash, Error> {
    use magnus::RArray;
    let hash = RHash::new();

    // Defaults mirroring Ruby CLI behavior
    hash.aset("serving", false)?;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        // Handle --key=value form
        if a.starts_with("--") && a.contains('=') {
            let mut parts = a.splitn(2, '=');
            let key = parts.next().unwrap().trim_start_matches("--");
            let val = parts.next().unwrap_or("");
            let k = key.replace('-', "_");
            match k.as_str() {
                "config" => {
                    let arr = RArray::new();
                    for p in val.split(',') { arr.push(p.trim())?; }
                    hash.aset("config", arr)?;
                }
                "plugins" | "plugins_dir" => {
                    let arr = RArray::new();
                    for p in val.split(',') { arr.push(p.trim())?; }
                    hash.aset("plugins_dir", arr)?;
                }
                "limit_posts" => {
                    if let Ok(n) = val.parse::<i64>() { hash.aset("limit_posts", n)?; }
                }
                _ => {
                    hash.aset(k.as_str(), val)?;
                }
            }
            i += 1;
            continue;
        }

        match a {
            // Booleans
            "--safe" => { hash.aset("safe", true)?; }
            "--profile" => { hash.aset("profile", true)?; }
            "--incremental" => { hash.aset("incremental", true)?; }
            "--watch" => { hash.aset("watch", true)?; }
            "--no-watch" => { hash.aset("watch", false)?; }
            "--future" => { hash.aset("future", true)?; }
            "--force_polling" => { hash.aset("force_polling", true)?; }
            "--lsi" => { hash.aset("lsi", true)?; }
            "--drafts" => { hash.aset("show_drafts", true)?; }
            "--unpublished" => { hash.aset("unpublished", true)?; }
            "--disable-disk-cache" | "--disable_disk_cache" => { hash.aset("disable_disk_cache", true)?; }
            "--quiet" | "-q" => { hash.aset("quiet", true)?; }
            "--verbose" | "-V" => { hash.aset("verbose", true)?; }
            "--strict_front_matter" | "--strict-front-matter" => { hash.aset("strict_front_matter", true)?; }

            // With separate values
            "-s" | "--source" => {
                if i + 1 < args.len() { hash.aset("source", args[i+1].as_str())?; i += 1; }
            }
            "-d" | "--destination" => {
                if i + 1 < args.len() { hash.aset("destination", args[i+1].as_str())?; i += 1; }
            }
            "-p" | "--plugins" => {
                if i + 1 < args.len() {
                    let arr = RArray::new();
                    for p in args[i+1].split(',') { arr.push(p.trim())?; }
                    hash.aset("plugins_dir", arr)?; i += 1;
                }
            }
            "--layouts" => {
                if i + 1 < args.len() { hash.aset("layouts_dir", args[i+1].as_str())?; i += 1; }
            }
            "-b" | "--baseurl" => {
                if i + 1 < args.len() { hash.aset("baseurl", args[i+1].as_str())?; i += 1; }
            }
            "--config" => {
                if i + 1 < args.len() {
                    let arr = RArray::new();
                    for p in args[i+1].split(',') { arr.push(p.trim())?; }
                    hash.aset("config", arr)?; i += 1;
                }
            }
            "--limit_posts" => {
                if i + 1 < args.len() {
                    if let Ok(n) = args[i+1].parse::<i64>() { hash.aset("limit_posts", n)?; }
                    i += 1;
                }
            }
            // Short booleans
            "-I" => { hash.aset("incremental", true)?; }
            "-w" => { hash.aset("watch", true)?; }
            "-D" => { hash.aset("show_drafts", true)?; }

            _ => {
                // Generic handling for unknown "--key value" pairs
                if a.starts_with("--") {
                    let key = a.trim_start_matches("--").replace('-', "_");
                    if i + 1 < args.len() && !args[i+1].starts_with('-') {
                        hash.aset(key.as_str(), args[i+1].as_str())?; i += 1;
                    } else {
                        hash.aset(key.as_str(), true)?;
                    }
                }
            }
        }
        i += 1;
    }

    Ok(hash)
}

fn print_help_serve() {
    println!("Usage: jekyllrs serve [options]\n");
    println!("Options:");
    println!("    -s, --source [DIR]             Source directory (defaults to ./)");
    println!("    -d, --destination [DIR]        Destination directory (defaults to ./_site)");
    println!("        --safe                     Safe mode (defaults to false)");
    println!("    -p, --plugins PLUGINS_DIRS     Plugins directory (defaults to ./_plugins)");
    println!("        --layouts DIR              Layouts directory (defaults to ./_layouts)");
    println!("    -H, --host [HOST]              Host to bind to");
    println!("    -P, --port [PORT]              Port to listen on");
    println!("    -o, --open-url                 Launch your site in a browser");
    println!("    -B, --detach                   Run the server in the background");
    println!("    -l, --livereload               Use LiveReload to automatically refresh browsers");
    println!("        --livereload-ignore GLOBS  Files for LiveReload to ignore (comma-separated)");
    println!("        --livereload-min-delay N   Minimum reload delay");
    println!("        --livereload-max-delay N   Maximum reload delay");
    println!("        --livereload-port PORT     Port for LiveReload to listen on");
    println!("        --show-dir-listing         Show directory listing");
    println!("        --ssl-cert [CERT]          X.509 (SSL) certificate");
    println!("        --ssl-key [KEY]            X.509 (SSL) private key");
    println!("        --trace                    Show full backtrace on errors");
}

fn run_serve(args: &[String], trace: bool) -> Result<(), Error> {
    eval::<Value>("require 'jekyll'")?;
    let _ = eval::<Value>("STDOUT.sync = true; STDERR.sync = true");

    let mut options = parse_serve_args(args)?;
    if options.aref::<_, Value>("serving")?.is_nil() { options.aset("serving", true)?; }
    if options.aref::<_, Value>("watch")?.is_nil() { options.aset("watch", true)?; }

    let jekyll: RModule = eval("Jekyll")?;
    let rust_mod: RModule = jekyll.const_get("Rust")?;
    let res = rust_mod.funcall::<_,_,Value>("engine_build_process", (options,));
    match res {
        Ok(_) => {},
        Err(e) => {
            if trace { return Err(e); }
            let _ = eval::<Value>(r#"
              msg = " Please append `--trace` to the `serve` command "
              dashes = "-" * msg.length
              Jekyll.logger.error "", dashes
              Jekyll.logger.error "Jekyll #{Jekyll::VERSION} ", msg
              Jekyll.logger.error "", " for any additional information or backtrace. "
              Jekyll.logger.abort_with "", dashes
            "#);
            return Err(e);
        }
    }

    let serve_klass: Value = eval("Jekyll::Commands::Serve")?;
    serve_klass.funcall::<_,_,Value>("process", (options,))?;
    Ok(())
}

fn parse_serve_args(args: &[String]) -> Result<RHash, Error> {
    use magnus::RArray;
    let hash = parse_build_args(args)?;
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-H" | "--host" => {
                if i + 1 < args.len() { hash.aset("host", args[i+1].as_str())?; i += 1; }
            }
            "-P" | "--port" => {
                if i + 1 < args.len() {
                    if let Ok(n) = args[i+1].parse::<i64>() { hash.aset("port", n)?; } else { hash.aset("port", args[i+1].as_str())?; }
                    i += 1;
                }
            }
            "-o" | "--open-url" => { hash.aset("open_url", true)?; }
            "-B" | "--detach" => { hash.aset("detach", true)?; }
            "-l" | "--livereload" => { hash.aset("livereload", true)?; }
            "--livereload-ignore" => {
                if i + 1 < args.len() {
                    let arr = RArray::new();
                    for p in args[i+1].split(',') { arr.push(p.trim())?; }
                    hash.aset("livereload_ignore", arr)?; i += 1;
                }
            }
            "--livereload-min-delay" => {
                if i + 1 < args.len() { if let Ok(n) = args[i+1].parse::<i64>() { hash.aset("livereload_min_delay", n)?; } i += 1; }
            }
            "--livereload-max-delay" => {
                if i + 1 < args.len() { if let Ok(n) = args[i+1].parse::<i64>() { hash.aset("livereload_max_delay", n)?; } i += 1; }
            }
            "--livereload-port" => {
                if i + 1 < args.len() { if let Ok(n) = args[i+1].parse::<i64>() { hash.aset("livereload_port", n)?; } i += 1; }
            }
            "--show-dir-listing" => { hash.aset("show_dir_listing", true)?; }
            "--ssl-cert" => { if i + 1 < args.len() { hash.aset("ssl_cert", args[i+1].as_str())?; i += 1; } }
            "--ssl-key" => { if i + 1 < args.len() { hash.aset("ssl_key", args[i+1].as_str())?; i += 1; } }
            _ => {}
        }
        i += 1;
    }
    Ok(hash)
}
