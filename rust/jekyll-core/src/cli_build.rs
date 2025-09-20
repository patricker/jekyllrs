use magnus::{function, prelude::*, Error, RModule, Ruby, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("engine_build_process", function!(engine_build_process, 1))?;
    Ok(())
}

fn fetch_bool(hash: Value, key: &str, default: bool) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    let k = ruby.str_new(key);
    let v: Value = hash.funcall("fetch", (k, default))?;
    Ok(!v.is_nil() && v.to_bool())
}

fn rb_expand_path(path: Value) -> Result<String, Error> {
    let ruby = ruby_handle()?;
    let file: RModule = ruby.class_object().const_get("File")?;
    let exp: Value = file.funcall("expand_path", (path,))?;
    exp.try_convert()
}

fn engine_build_site_with_profile(site: Value, profile_enabled: bool) -> Result<(), Error> {
    if profile_enabled {
        let profiler: Value = site.funcall("profiler", ())?;
        let _: Value = profiler.funcall("profile_process", ())?;
        return Ok(());
    }
    let _: Value = site.funcall("reset", ())?;
    let _: Value = site.funcall("read", ())?;
    let _: Value = site.funcall("generate", ())?;
    let _: Value = site.funcall("render", ())?;
    let _: Value = site.funcall("cleanup", ())?;
    let _: Value = site.funcall("write", ())?;
    Ok(())
}

fn engine_build_process(options: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;

    // Logger and verbosity
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let logger: Value = jekyll.funcall("logger", ())?;
    let _: Value = logger.funcall("adjust_verbosity", (options,))?;

    // Resolve configuration from options
    let command: RModule = jekyll.const_get("Command")?;
    let config: Value = command.funcall("configuration_from_options", (options,))?;

    // Instantiate site
    let site_class: Value = jekyll.const_get("Site")?;
    let site: Value = site_class.funcall("new", (config,))?;

    // Initial build
    let skip_initial = fetch_bool(options, "skip_initial_build", false)?;
    if skip_initial {
        let _: Value = logger.funcall(
            "warn",
            ("Build Warning:", "Skipping the initial build. This may result in an out-of-date site."),
        )?;
    } else {
        // Build with logging identical to Ruby
        let t0 = std::time::Instant::now();

        // Source and destination from config
        let src_val: Value = config.funcall("[]", (ruby.str_new("source"),))?;
        let dst_val: Value = config.funcall("[]", (ruby.str_new("destination"),))?;
        let source = rb_expand_path(src_val)?;
        let destination = rb_expand_path(dst_val)?;

        let incremental_val: Value = config.funcall("[]", (ruby.str_new("incremental"),))?;
        let incremental = !incremental_val.is_nil() && incremental_val.to_bool();
        let inc_msg = if incremental { "enabled" } else { "disabled. Enable with --incremental" };

        let _: Value = logger.funcall("info", ("Source:", source))?;
        let _: Value = logger.funcall("info", ("Destination:", destination))?;
        let _: Value = logger.funcall("info", ("Incremental build:", inc_msg))?;
        let _: Value = logger.funcall("info", ("Generating...",))?;

        // Profile-aware engine run
        let profile_enabled = fetch_bool(config, "profile", false)?;
        engine_build_site_with_profile(site, profile_enabled)?;

        let secs = t0.elapsed().as_secs_f64();
        let _: Value = logger.funcall("info", ("", format!("done in {:.3} seconds.", secs)))?;
    }

    // Watch handling
    let detach = fetch_bool(config, "detach", false)?;
    let watch = fetch_bool(config, "watch", false)?;
    if detach {
        let _: Value = logger.funcall(
            "info",
            ("Auto-regeneration:", "disabled when running server detached."),
        )?;
    } else if watch {
        let external: RModule = jekyll.const_get("External")?;
        let _: Value = external.funcall("require_with_graceful_fail", ("jekyll-watch",))?;
        let watcher: RModule = jekyll.const_get("Watcher")?;
        let _: Value = watcher.funcall("watch", (config, site))?;
    } else {
        let _: Value = logger.funcall(
            "info",
            ("Auto-regeneration:", "disabled. Use --watch to enable."),
        )?;
    }

    Ok(())
}
