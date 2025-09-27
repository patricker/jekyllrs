use std::collections::HashSet;

use magnus::{
    function, prelude::*, r_hash::ForEach, Error, IntoValue, RArray, RHash, RModule, Ruby,
    TryConvert, Value,
};

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
    let file: Value = ruby.class_object().const_get("File")?;
    let exp: Value = file.funcall("expand_path", (path,))?;
    String::try_convert(exp)
}

pub(crate) fn engine_build_process(options: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    // Logger and verbosity
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let logger: Value = jekyll.funcall("logger", ())?;
    let _: Value = logger.funcall("adjust_verbosity", (options,))?;

    // Resolve configuration from options
    let command: Value = jekyll.const_get("Command")?;
    let config: Value = command.funcall("configuration_from_options", (options,))?;

    // Instantiate site
    let site_class: Value = jekyll.const_get("Site")?;
    let site: Value = site_class.funcall("new", (config,))?;

    // Initial build
    let skip_initial = fetch_bool(options, "skip_initial_build", false)?;
    let serving = fetch_bool(options, "serving", false)?;
    let livereload_enabled = fetch_bool(options, "livereload", false)?;
    let collect_livereload_changes = serving && livereload_enabled;
    let mut changed_entries: Vec<ChangedEntry> = Vec::new();

    if skip_initial {
        let _: Value = logger.funcall(
            "warn",
            (
                "Build Warning:",
                "Skipping the initial build. This may result in an out-of-date site.",
            ),
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
        let inc_msg = if incremental {
            "enabled"
        } else {
            "disabled. Enable with --incremental"
        };

        let _: Value = logger.funcall("info", ("Source:", source))?;
        let _: Value = logger.funcall("info", ("Destination:", destination))?;
        let _: Value = logger.funcall("info", ("Incremental build:", inc_msg))?;
        let _: Value = logger.funcall("info", ("Generating...",))?;

        let profile_enabled = fetch_bool(config, "profile", false)?;
        let timings = crate::engine::run_site_phases(site, profile_enabled)?;

        if collect_livereload_changes {
            changed_entries = collect_changed_entries(site)?;
        }

        let secs = t0.elapsed().as_secs_f64();
        let _: Value = logger.funcall("info", ("", format!("done in {:.3} seconds.", secs)))?;

        if let Some(ref timings) = timings {
            crate::engine::emit_build_summary(site, timings)?;
        }
    }

    // Watch handling
    let detach = fetch_bool(config, "detach", false)?;
    let watch = fetch_bool(config, "watch", false)?;
    if serving {
        // Serve path handles watch messaging separately.
    } else if detach {
        let _: Value = logger.funcall(
            "info",
            (
                "Auto-regeneration:",
                "disabled when running server detached.",
            ),
        )?;
    } else if watch {
        let _: Value = logger.funcall(
            "warn",
            (
                "Auto-regeneration:",
                "watch requested; native watcher not yet implemented so no rebuilds will run.",
            ),
        )?;
    } else {
        let _: Value = logger.funcall(
            "info",
            ("Auto-regeneration:", "disabled. Use --watch to enable."),
        )?;
    }

    entries_to_value(&ruby, changed_entries)
}

struct ChangedEntry {
    relative_path: String,
    url: Option<String>,
}

fn entries_to_value(ruby: &Ruby, entries: Vec<ChangedEntry>) -> Result<Value, Error> {
    let array = ruby.ary_new_capa(entries.len());
    for entry in entries {
        let tuple = ruby.ary_new_capa(2);
        tuple.push(ruby.str_new(entry.relative_path.as_str()))?;
        match entry.url {
            Some(url) => tuple.push(ruby.str_new(&url))?,
            None => tuple.push(ruby.qnil())?,
        }
        array.push(tuple.into_value_with(ruby))?;
    }
    Ok(array.into_value_with(ruby))
}

fn collect_changed_entries(site: Value) -> Result<Vec<ChangedEntry>, Error> {
    let regenerator: Value = site.funcall("regenerator", ())?;
    let mut seen = HashSet::new();
    let mut entries = Vec::new();

    collect_from_array(
        site.funcall("pages", ())?,
        &regenerator,
        &mut seen,
        &mut entries,
    )?;
    collect_static_files(
        site.funcall("static_files", ())?,
        &regenerator,
        &mut seen,
        &mut entries,
    )?;
    collect_collections(
        site.funcall("collections", ())?,
        &regenerator,
        &mut seen,
        &mut entries,
    )?;

    Ok(entries)
}

fn collect_from_array(
    array_value: Value,
    regenerator: &Value,
    seen: &mut HashSet<String>,
    entries: &mut Vec<ChangedEntry>,
) -> Result<(), Error> {
    if let Some(array) = RArray::from_value(array_value) {
        for item in array.each() {
            let value = item?;
            push_if_changed(value, regenerator, seen, entries, true)?;
        }
    }
    Ok(())
}

fn collect_static_files(
    array_value: Value,
    regenerator: &Value,
    seen: &mut HashSet<String>,
    entries: &mut Vec<ChangedEntry>,
) -> Result<(), Error> {
    if let Some(array) = RArray::from_value(array_value) {
        for item in array.each() {
            let value = item?;
            push_if_changed(
                value,
                regenerator,
                seen,
                entries,
                should_consider_write_flag(value)?,
            )?;
        }
    }
    Ok(())
}

fn collect_collections(
    collections_value: Value,
    regenerator: &Value,
    seen: &mut HashSet<String>,
    entries: &mut Vec<ChangedEntry>,
) -> Result<(), Error> {
    if let Some(collections) = RHash::from_value(collections_value) {
        collections.foreach(|_key: Value, coll: Value| {
            let docs = coll.funcall("docs", ())?;
            if let Some(array) = RArray::from_value(docs) {
                for doc in array.each() {
                    let doc_value = doc?;
                    push_if_changed(
                        doc_value,
                        regenerator,
                        seen,
                        entries,
                        should_consider_write_flag(doc_value)?,
                    )?;
                }
            }
            Ok(ForEach::Continue)
        })?;
    }
    Ok(())
}

fn should_consider_write_flag(value: Value) -> Result<bool, Error> {
    let writes: Value = value.funcall("write?", ())?;
    Ok(!writes.is_nil() && writes.to_bool())
}

fn push_if_changed(
    entry: Value,
    regenerator: &Value,
    seen: &mut HashSet<String>,
    entries: &mut Vec<ChangedEntry>,
    should_include: bool,
) -> Result<(), Error> {
    if !should_include {
        return Ok(());
    }

    let should_regenerate: Value = regenerator.funcall("regenerate?", (entry,))?;
    if should_regenerate.is_nil() || !should_regenerate.to_bool() {
        return Ok(());
    }

    let relative_path_val: Value = entry.funcall("relative_path", ())?;
    let relative_path = String::try_convert(relative_path_val)?;

    if !seen.insert(relative_path.clone()) {
        return Ok(());
    }

    let url_val: Value = entry.funcall("url", ())?;
    let url = if url_val.is_nil() {
        None
    } else {
        Some(String::try_convert(url_val)?)
    };

    entries.push(ChangedEntry { relative_path, url });
    Ok(())
}
