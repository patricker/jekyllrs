use magnus::{
    function, prelude::*, Error, IntoValue, RArray, RHash, RModule, RString, Ruby, Value,
};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("reader_classify", function!(reader_classify, 2))?;
    bridge.define_singleton_method("reader_walk", function!(reader_walk, 2))?;
    bridge.define_singleton_method("reader_get_entries", function!(reader_get_entries_walk, 3))?;
    bridge.define_singleton_method(
        "reader_get_entries_posts",
        function!(reader_get_entries_posts_walk, 3),
    )?;
    bridge.define_singleton_method(
        "reader_get_entries_drafts",
        function!(reader_get_entries_drafts_walk, 3),
    )?;
    bridge.define_singleton_method(
        "data_reader_entries",
        function!(data_reader_entries_native, 2),
    )?;
    bridge.define_singleton_method(
        "data_reader_csv_read",
        function!(data_reader_csv_read, 2),
    )?;
    bridge.define_singleton_method(
        "data_reader_tsv_read",
        function!(data_reader_tsv_read, 2),
    )?;
    bridge.define_singleton_method("layout_entries", function!(layout_entries_walk, 2))?;
    Ok(())
}

fn reader_classify(site: Value, base: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    let file: Value = ruby.class_object().const_get("File")?;
    let base_path = base.to_string()?;
    let is_dir: bool = file.funcall("directory?", (ruby.str_new(&base_path),))?;
    let result = ruby.hash_new();

    let dirs = ruby.ary_new();
    let pages = ruby.ary_new();
    let statics = ruby.ary_new();
    if !is_dir {
        result.aset(ruby.to_symbol("dirs"), dirs)?;
        result.aset(ruby.to_symbol("pages"), pages)?;
        result.aset(ruby.to_symbol("static"), statics)?;
        return Ok(result.into_value_with(&ruby));
    }

    // Read entries and apply EntryFilter
    let dir_mod: Value = ruby.class_object().const_get("Dir")?;
    let entries_val: Value = dir_mod.funcall("entries", (base,))?;

    // Call back into our Bridge.entry_filter to reuse filtering logic
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;
    let filtered: Value = bridge.funcall("entry_filter", (site, entries_val, base))?;
    let arr = RArray::try_convert(filtered)?;

    for item in arr.each() {
        let entry_val = item?;
        let entry_name: RString = entry_val.funcall("to_s", ())?;
        let full: RString = file.funcall("join", (ruby.str_new(&base_path), entry_name))?;
        let full_str = full.to_string()?;
        let is_dir: bool = file.funcall("directory?", (ruby.str_new(&full_str),))?;
        if is_dir {
            dirs.push(entry_val)?;
            continue;
        }
        // Page if has YAML header
        let has_header: bool = bridge.funcall("has_yaml_header?", (ruby.str_new(&full_str),))?;
        if has_header {
            pages.push(entry_val)?;
        } else {
            statics.push(entry_val)?;
        }
    }

    result.aset(ruby.to_symbol("dirs"), dirs)?;
    result.aset(ruby.to_symbol("pages"), pages)?;
    result.aset(ruby.to_symbol("static"), statics)?;
    Ok(result.into_value_with(&ruby))
}

fn reader_walk(site: Value, rel_dir: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let source_dir: RString = site.funcall("in_source_dir", (rel_dir,))?;
    let base_path = source_dir.to_string()?;

    let pages = ruby.ary_new();
    let statics = ruby.ary_new();
    let dirs = ruby.ary_new();

    fn walk(
        ruby: &Ruby,
        site: Value,
        file: Value,
        bridge: RModule,
        ef: Value,
        base_path: &str,
        rel_prefix: &str,
        pages: &RArray,
        statics: &RArray,
        dirs: &RArray,
    ) -> Result<(), Error> {
        let dir_mod: Value = ruby.class_object().const_get("Dir")?;
        let base_rs = ruby.str_new(base_path);
        let entries_val: Value = dir_mod.funcall("entries", (base_rs,))?;

        let filtered: Value = bridge.funcall("entry_filter", (site, entries_val, base_rs))?;
        let arr = RArray::try_convert(filtered)?;
        for item in arr.each() {
            let entry_val = item?;
            let entry_name: RString = entry_val.funcall("to_s", ())?;
            let entry_str = entry_name.to_string()?;
            let full: RString = file.funcall("join", (ruby.str_new(base_path), entry_name))?;
            let full_str = full.to_string()?;
            let is_dir: bool = file.funcall("directory?", (ruby.str_new(&full_str),))?;
            if is_dir {
                // Exclude symlinked directories (outside source in safe mode)
                let is_bad: bool = ef.funcall("symlink?", (ruby.str_new(&full_str),))?;
                if is_bad {
                    continue;
                }
                let child_rel = if rel_prefix.is_empty() {
                    entry_str.clone()
                } else {
                    format!("{}/{}", rel_prefix, entry_str)
                };
                // Record directory relative path for post/draft scanning on Ruby side
                dirs.push(ruby.str_new(&child_rel))?;
                let child_base: RString =
                    file.funcall("join", (ruby.str_new(base_path), ruby.str_new(&entry_str)))?;
                walk(
                    ruby,
                    site,
                    file,
                    bridge,
                    ef,
                    &child_base.to_string()?,
                    &child_rel,
                    pages,
                    statics,
                    dirs,
                )?;
            } else {
                let entry_value = ruby.str_new(&full_str);
                // Skip unsafe symlinks in safe mode
                let is_bad: bool = ef.funcall("symlink?", (entry_value,))?;
                if is_bad {
                    continue;
                }
                // If this is a symlink, always treat as static (even if it has YAML header)
                let is_symlink: bool = file.funcall("symlink?", (entry_value,))?;
                let rel_path = if rel_prefix.is_empty() {
                    entry_str
                } else {
                    format!("{}/{}", rel_prefix, entry_str)
                };
                if is_symlink {
                    // If symlink points outside source, treat as static; otherwise, treat normally
                    let outside: bool = ef.funcall("symlink_outside_site_source?", (entry_value,))?;
                    if outside {
                        statics.push(ruby.str_new(&rel_path))?;
                    } else {
                        let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
                        let rust: RModule = jekyll.const_get("Rust")?;
                        let bridge2: RModule = rust.const_get("Bridge")?;
                        let has_header: bool =
                            bridge2.funcall("has_yaml_header?", (ruby.str_new(&full_str),))?;
                        if has_header {
                            pages.push(ruby.str_new(&rel_path))?;
                        } else {
                            statics.push(ruby.str_new(&rel_path))?;
                        }
                    }
                } else {
                    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
                    let rust: RModule = jekyll.const_get("Rust")?;
                    let bridge2: RModule = rust.const_get("Bridge")?;
                    let has_header: bool =
                        bridge2.funcall("has_yaml_header?", (ruby.str_new(&full_str),))?;
                    if has_header {
                        pages.push(ruby.str_new(&rel_path))?;
                    } else {
                        statics.push(ruby.str_new(&rel_path))?;
                    }
                }
            }
        }
        Ok(())
    }

    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;
    let ef_class: Value = jekyll.const_get("EntryFilter")?;
    let ef: Value = ef_class.funcall("new", (site,))?;
    walk(
        &ruby, site, file, bridge, ef, &base_path, "", &pages, &statics, &dirs,
    )?;

    let result = ruby.hash_new();
    result.aset(ruby.to_symbol("pages"), pages)?;
    result.aset(ruby.to_symbol("static"), statics)?;
    result.aset(ruby.to_symbol("dirs"), dirs)?;
    Ok(result.into_value_with(&ruby))
}

// Native-walker-backed implementation for get_entries
fn reader_get_entries_walk(site: Value, dir: RString, subfolder: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let base: RString = site.funcall("in_source_dir", (dir, subfolder))?;
    let base_str = base.to_string()?;
    let exists: bool = file.funcall("exist?", (ruby.str_new(&base_str),))?;
    if !exists {
        return Ok(ruby.ary_new().into_value_with(&ruby));
    }

    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;

    let list = crate::fs_walk::recursive_list_site(site, &base_str).unwrap_or_else(|_| Vec::new());
    let arr = ruby.ary_new();
    for s in list.iter() {
        let _ = arr.push(ruby.str_new(s));
    }
    let filtered: Value = bridge.funcall("entry_filter", (site, arr, base))?;
    let entries = RArray::try_convert(filtered)?;

    let out = ruby.ary_new();
    for item in entries.each() {
        let e: RString = item?.funcall("to_s", ())?;
        let joined: RString = site.funcall("in_source_dir", (ruby.str_new(&base_str), e))?;
        let is_dir: bool = file.funcall("directory?", (joined,))?;
        if !is_dir {
            out.push(e)?;
        }
    }
    Ok(out.into_value_with(&ruby))
}

// Native-walker-backed implementation for layout entries
fn layout_entries_walk(site: Value, dir: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let dir_str = dir.to_string()?;
    let exists: bool = file.funcall("exist?", (ruby.str_new(&dir_str),))?;
    let out = ruby.ary_new();
    if !exists {
        return Ok(out.into_value_with(&ruby));
    }

    // Traverse and collect files with an extension (like **/*.*)
    let mut entries_vec: Vec<String> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(&dir_str)];
    while let Some(p) = stack.pop() {
        if let Ok(iter) = std::fs::read_dir(&p) {
            for ent in iter.flatten() {
                let path = ent.path();
                let rel = match path.strip_prefix(&dir_str) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let name = {
                    let s = rel.to_string_lossy();
                    if std::path::MAIN_SEPARATOR == '/' {
                        s.into_owned()
                    } else {
                        s.replace(std::path::MAIN_SEPARATOR, "/")
                    }
                };
                if name.is_empty() {
                    continue;
                }
                if let Ok(ft) = ent.file_type() {
                    if ft.is_dir() {
                        stack.push(path);
                    } else if name.contains('.') {
                        entries_vec.push(name);
                    }
                }
            }
        }
    }

    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;
    let arr = ruby.ary_new();
    for s in entries_vec.iter() {
        let _ = arr.push(ruby.str_new(s));
    }
    let filtered: Value = bridge.funcall("entry_filter", (site, arr, dir))?;

    // Filter out symlinked files outside the source when in safe mode
    let ef_class: Value = jekyll.const_get("EntryFilter")?;
    let ef: Value = ef_class.funcall("new", (site,))?;
    if let Some(arr) = RArray::from_value(filtered.clone()) {
        let out = ruby.ary_new();
        for item in arr.each() {
            let name: RString = item?.funcall("to_s", ())?;
            let full: RString = file.funcall("join", (dir, name))?;
            let skip: bool = ef.funcall("symlink?", (full,))?;
            if !skip {
                out.push(name)?;
            }
        }
        return Ok(out.into_value_with(&ruby));
    }
    Ok(filtered)
}

// Helper that mirrors collect_entries but uses native walker
fn collect_entries_walk(
    site: Value,
    dir: RString,
    subfolder: RString,
) -> Result<(Ruby, Value, RArray, String), Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let base: RString = site.funcall("in_source_dir", (dir, subfolder))?;
    let base_str = base.to_string()?;
    let exists: bool = file.funcall("exist?", (ruby.str_new(&base_str),))?;
    if !exists {
        let arr = ruby.ary_new();
        return Ok((ruby, file, arr, base_str));
    }
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;
    let list = crate::fs_walk::recursive_list_site(site, &base_str).unwrap_or_else(|_| Vec::new());
    let arr = ruby.ary_new();
    for s in list.iter() {
        let _ = arr.push(ruby.str_new(s));
    }
    let filtered: Value = bridge.funcall("entry_filter", (site, arr, base))?;
    let entries = RArray::try_convert(filtered)?;
    Ok((ruby, file, entries, base_str))
}

fn reader_get_entries_posts_walk(
    site: Value,
    dir: RString,
    subfolder: RString,
) -> Result<Value, Error> {
    let (ruby, file, entries, base_str) = collect_entries_walk(site, dir, subfolder)?;
    static POST_RE: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"^(?:.+/)*?(\d{2,4}-\d{1,2}-\d{1,2})-([^/]*)(\.[^.]+)$").unwrap()
    });
    let out = ruby.ary_new();
    for item in entries.each() {
        let e: RString = item?.funcall("to_s", ())?;
        let joined: RString = site.funcall("in_source_dir", (ruby.str_new(&base_str), e))?;
        let is_dir: bool = file.funcall("directory?", (joined,))?;
        if is_dir {
            continue;
        }
        let s = e.to_string()?;
        if POST_RE.is_match(&s) {
            out.push(e)?;
        }
    }
    Ok(out.into_value_with(&ruby))
}

fn reader_get_entries_drafts_walk(
    site: Value,
    dir: RString,
    subfolder: RString,
) -> Result<Value, Error> {
    let (ruby, file, entries, base_str) = collect_entries_walk(site, dir, subfolder)?;
    static DRAFT_RE: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"^(?:.+/)*(.*)(\.[^.]+)$").unwrap());
    let out = ruby.ary_new();
    for item in entries.each() {
        let e: RString = item?.funcall("to_s", ())?;
        let joined: RString = site.funcall("in_source_dir", (ruby.str_new(&base_str), e))?;
        let is_dir: bool = file.funcall("directory?", (joined,))?;
        if is_dir {
            continue;
        }
        let s = e.to_string()?;
        if DRAFT_RE.is_match(&s) {
            out.push(e)?;
        }
    }
    Ok(out.into_value_with(&ruby))
}

fn data_reader_entries_native(site: Value, dir: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let dir_str = dir.to_string()?;

    let is_dir: bool = file.funcall("directory?", (ruby.str_new(&dir_str),))?;
    let result = ruby.hash_new();
    let files = ruby.ary_new();
    let dirs = ruby.ary_new();
    result.aset(ruby.to_symbol("files"), files)?;
    result.aset(ruby.to_symbol("dirs"), dirs)?;

    if !is_dir {
        return Ok(result.into_value_with(&ruby));
    }

    // EntryFilter for symlink checks
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let ef_class: Value = jekyll.const_get("EntryFilter")?;
    let ef: Value = ef_class.funcall("new", (site,))?;

    if let Ok(iter) = std::fs::read_dir(&dir_str) {
        for ent in iter.flatten() {
            let name_os = match ent.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let full: RString =
                file.funcall("join", (ruby.str_new(&dir_str), ruby.str_new(&name_os)))?;
            let skip: bool = ef.funcall("symlink?", (full,))?;
            if skip {
                continue;
            }
            if let Ok(ft) = ent.file_type() {
                if ft.is_dir() {
                    if name_os != "." && name_os != ".." {
                        let _ = dirs.push(ruby.str_new(&name_os));
                    }
                } else {
                    let lower = name_os.to_lowercase();
                    if lower.ends_with(".yaml")
                        || lower.ends_with(".yml")
                        || lower.ends_with(".json")
                        || lower.ends_with(".csv")
                        || lower.ends_with(".tsv")
                    {
                        let _ = files.push(ruby.str_new(&name_os));
                    }
                }
            }
        }
    }

    Ok(result.into_value_with(&ruby))
}

fn csv_read_common(path: RString, options: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    // Ensure CSV is available (jekyll.rb requires it, but be safe)
    let _ = ruby.eval::<Value>("CSV");
    let csv_mod: Value = ruby.class_object().const_get("CSV")?;

    if let Some(hash) = RHash::from_value(options) {
        // Ensure symbol keys for Ruby 3 keyword arguments
        let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
        let rust: RModule = jekyll.const_get("Rust")?;
        let sym_hash_val: Value = rust.funcall("symbolize_hash_keys", (hash,))?;
        let sym_hash = RHash::from_value(sym_hash_val).ok_or_else(|| {
            Error::new(ruby.exception_runtime_error(), "expected Hash from symbolize_hash_keys")
        })?;
        let kwargs = magnus::KwArgs(sym_hash);
        let result: Value = csv_mod.funcall("read", (path, kwargs))?;
        Ok(result)
    } else {
        let result: Value = csv_mod.funcall("read", (path,))?;
        Ok(result)
    }
}

fn data_reader_csv_read(path: RString, options: Value) -> Result<Value, Error> {
    csv_read_common(path, options)
}

fn data_reader_tsv_read(path: RString, options: Value) -> Result<Value, Error> {
    csv_read_common(path, options)
}
