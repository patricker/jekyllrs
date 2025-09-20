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

fn parse_build_args(args: &[String]) -> Result<RHash, Error> {
    // Defaults mirror Jekyll's CLI defaults
    let hash = RHash::new();
    hash.aset("serving", false)?;
    hash.aset("incremental", false)?;
    hash.aset("watch", false)?;
    hash.aset("profile", false)?;

    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "-s" | "--source" => {
                if i + 1 < args.len() {
                    hash.aset("source", args[i + 1].as_str())?;
                    i += 1;
                }
            }
            "-d" | "--destination" => {
                if i + 1 < args.len() {
                    hash.aset("destination", args[i + 1].as_str())?;
                    i += 1;
                }
            }
            "--safe" => {
                hash.aset("safe", true)?;
            }
            "-p" | "--plugins" => {
                if i + 1 < args.len() {
                    hash.aset("plugins_dir", args[i + 1].as_str())?;
                    i += 1;
                }
            }
            "--layouts" => {
                if i + 1 < args.len() {
                    hash.aset("layouts_dir", args[i + 1].as_str())?;
                    i += 1;
                }
            }
            "--profile" => {
                hash.aset("profile", true)?;
            }
            "--incremental" => {
                hash.aset("incremental", true)?;
            }
            "--watch" => {
                hash.aset("watch", true)?;
            }
            _ => {}
        }
        i += 1;
    }

    Ok(hash)
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

fn run_build(args: &[String]) -> Result<(), Error> {
    // Require jekyll library and call Jekyll::Commands::Build.process(options)
    eval::<Value>("require 'jekyll'")?;
    // Make stdout synchronous for real-time logs.
    let _ = eval::<Value>("STDOUT.sync = true; STDERR.sync = true");

    let options = parse_build_args(args)?;

    // Build.process expects a Hash-like options map.
    let jekyll: RModule = eval("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let _: Value = rust.funcall("engine_build_process", (options,))?;
    Ok(())
}

fn run_main() -> Result<(), Error> {
    set_env_defaults();
    unsafe {
        let _cleanup = magnus::embed::init();
        // Prepare Ruby load path to respect Gemfile if present.
        ensure_ruby_load_path_for_gemfile()?;

        let mut args = env::args().skip(1).collect::<Vec<String>>();
        // Default subcommand is build if omitted.
        let sub = if args.is_empty() { String::from("build") } else { args.remove(0) };
        match sub.as_str() {
            "build" | "b" => run_build(&args),
            "-v" | "--version" | "version" => {
                // Print Jekyll version from Ruby for parity.
                eval::<Value>("require 'jekyll'; puts Jekyll::VERSION").map(|_| ())
            }
            "-h" | "--help" | "help" => {
                eprintln!("Usage: jekyllrs build [options]\n(serve/new/etc. to be added)\n");
                Ok(())
            }
            other => {
                eprintln!("unsupported subcommand: {} (currently only 'build')", other);
                Err(Error::new(exception::arg_error(), "unsupported subcommand"))
            }
        }
    }
}

fn main() -> ExitCode {
    match run_main() {
        Ok(()) => ExitCode::from(0),
        Err(e) => {
            eprintln!("error: {}", e);
            ExitCode::from(1)
        }
    }
}
