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
    let command_mod: Value = jekyll.const_get("Command")?;
    let cmd: Value = if trace {
        eval("o = Object.new; def o.trace; true; end; def o.name; 'build'; end; o")?
    } else {
        eval("o = Object.new; def o.trace; false; end; def o.name; 'build'; end; o")?
    };
    let build_klass: Value = eval("Jekyll::Commands::Build")?;
    let _: Value = command_mod.funcall("process_with_graceful_fail", (cmd, options, build_klass))?;
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
                other => {
            eprintln!("unsupported subcommand: {} (currently only 'build')", other);
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
