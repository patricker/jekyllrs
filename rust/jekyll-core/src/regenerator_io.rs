use magnus::{function, prelude::*, Error, IntoValue, RArray, RHash, RModule, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method(
        "regenerator_read_metadata",
        function!(regenerator_read_metadata, 2),
    )?;
    bridge.define_singleton_method(
        "regenerator_write_metadata",
        function!(regenerator_write_metadata, 3),
    )?;
    bridge.define_singleton_method(
        "regenerator_existing_file_modified",
        function!(regenerator_existing_file_modified, 2),
    )?;
    bridge.define_singleton_method(
        "regenerator_source_modified_or_dest_missing",
        function!(regenerator_source_modified_or_dest_missing, 3),
    )?;
    Ok(())
}

fn regenerator_read_metadata(metadata_file: String, disabled: bool) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    if disabled {
        return Ok(ruby.hash_new().into_value_with(&ruby));
    }
    // return {} unless File.file?(metadata_file)
    let file_class: Value = ruby.class_object().const_get("File")?;
    let is_file: bool = file_class.funcall("file?", (ruby.str_new(&metadata_file),))?;
    if !is_file {
        return Ok(ruby.hash_new().into_value_with(&ruby));
    }

    // content = File.binread(metadata_file)
    let content: Value = file_class.funcall("binread", (ruby.str_new(&metadata_file),))?;

    // Try Marshal.load(content)
    let marshal: Value = ruby.class_object().const_get("Marshal")?;
    let loaded: Result<Value, Error> = marshal.funcall("load", (content,));
    match loaded {
        Ok(val) => return Ok(val),
        Err(err) => {
            // If TypeError -> SafeYAML.load(content)
            let type_error = ruby.exception_type_error();
            if err.is_kind_of(type_error) {
                // Ensure SafeYAML is loaded
                let safe_yaml = match ruby.class_object().const_get::<_, Value>("SafeYAML") {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = ruby.eval::<Value>("require 'safe_yaml'");
                        ruby.class_object().const_get("SafeYAML")?
                    }
                };
                let result: Value = safe_yaml.funcall("load", (content,))?;
                return Ok(result);
            }
            // If ArgumentError -> warn and return {}
            let arg_error = ruby.exception_arg_error();
            if err.is_kind_of(arg_error) {
                let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
                let logger: Value = jekyll.funcall("logger", ())?;
                let message = format!("Failed to load {}: {}", metadata_file, err);
                let _ = logger.funcall::<_, _, Value>("warn", (ruby.str_new(""), ruby.str_new(&message)));
                return Ok(ruby.hash_new().into_value_with(&ruby));
            }
            // Re-raise other errors
            return Err(err);
        }
    }
}

fn regenerator_write_metadata(
    metadata_file: String,
    metadata: Value,
    disabled: bool,
) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    if disabled {
        return Ok(());
    }
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let logger: Value = jekyll.funcall("logger", ())?;
    let _ = logger.funcall::<_, _, Value>(
        "debug",
        (ruby.str_new("Writing Metadata:"), ruby.str_new(".jekyll-metadata")),
    );

    let marshal: Value = ruby.class_object().const_get("Marshal")?;
    let dumped: Value = marshal.funcall("dump", (metadata,))?;

    let file_class: Value = ruby.class_object().const_get("File")?;
    let _ = file_class.funcall::<_, _, Value>(
        "binwrite",
        (ruby.str_new(&metadata_file), dumped),
    )?;

    Ok(())
}


fn regenerator_existing_file_modified(this_obj: Value, path: String) -> Result<bool, Error> {
    let ruby = ruby_handle()?;

    let metadata: Value = this_obj.funcall("metadata", ())?;
    let meta_hash = RHash::from_value(metadata)
        .ok_or_else(|| Error::new(ruby.exception_type_error(), "metadata not a Hash"))?;
    let cache: Value = this_obj.funcall("cache", ())?;
    let cache_hash = RHash::from_value(cache)
        .ok_or_else(|| Error::new(ruby.exception_type_error(), "cache not a Hash"))?;

    let key = ruby.str_new(&path);
    let entry: Value = meta_hash.aref(key)?;

    let deps_key = ruby.str_new("deps");
    let deps_val: Value = entry.funcall("[]", (deps_key,))?;
    if let Some(deps) = RArray::from_value(deps_val) {
        for item in deps.each() {
            let dep = item?;
            let changed: bool = this_obj.funcall("modified?", (dep,))?;
            if changed {
                cache_hash.aset(dep, true.into_value_with(&ruby))?;
                cache_hash.aset(ruby.str_new(&path), true.into_value_with(&ruby))?;
                return Ok(true);
            }
        }
    }

    let file_class: Value = ruby.class_object().const_get("File")?;
    let exists: bool = file_class.funcall("exist?", (ruby.str_new(&path),))?;
    if exists {
        let mtime_key = ruby.str_new("mtime");
        let meta_mtime: Value = entry.funcall("[]", (mtime_key,))?;
        let file_mtime: Value = file_class.funcall("mtime", (ruby.str_new(&path),))?;
        let equal: bool = meta_mtime.funcall("eql?", (file_mtime,))?;
        if equal {
            cache_hash.aset(ruby.str_new(&path), false.into_value_with(&ruby))?;
            return Ok(false);
        }
    }

    let _ = this_obj.funcall::<_, _, Value>("add", (ruby.str_new(&path),))?;
    Ok(true)
}


fn regenerator_source_modified_or_dest_missing(
    this_obj: Value,
    source_path: Value,
    dest_path: Value,
) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    let source_changed: bool;
    if source_path.is_nil() {
        source_changed = true;
    } else {
        let changed: bool = this_obj.funcall("modified?", (source_path,))?;
        source_changed = changed;
    }

    let mut dest_missing = false;
    if !dest_path.is_nil() {
        let file_class: Value = ruby.class_object().const_get("File")?;
        let exists: bool = file_class.funcall("exist?", (dest_path,))?;
        dest_missing = !exists;
    }

    Ok(source_changed || dest_missing)
}
