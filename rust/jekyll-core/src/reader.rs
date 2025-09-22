use magnus::{
    function, prelude::*, Error, IntoValue, RArray, RHash, RModule, RString, Ruby, Symbol, Value,
};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("reader_classify", function!(reader_classify, 2))?;
    bridge.define_singleton_method("reader_walk", function!(reader_walk, 2))?;
    bridge.define_singleton_method("reader_get_entries", function!(reader_get_entries, 3))?;
    Ok(())
}

fn reader_classify(site: Value, base: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    let file: Value = ruby.class_object().const_get("File")?;
    let base_path = base.to_string()?;
    let is_dir: bool = file.funcall("directory?", (ruby.str_new(&base_path),))?;
    let result = RHash::new();

    let dirs = ruby.ary_new();
    let pages = ruby.ary_new();
    let statics = ruby.ary_new();

    if !is_dir {
        result.aset(Symbol::new("dirs"), dirs)?;
        result.aset(Symbol::new("pages"), pages)?;
        result.aset(Symbol::new("static"), statics)?;
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

    result.aset(Symbol::new("dirs"), dirs)?;
    result.aset(Symbol::new("pages"), pages)?;
    result.aset(Symbol::new("static"), statics)?;
    Ok(result.into_value_with(&ruby))
}

fn reader_walk(site: Value, rel_dir: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let source_dir: RString = site.funcall("in_source_dir", (rel_dir,))?;
    let base_path = source_dir.to_string()?;

    let pages = ruby.ary_new();
    let statics = ruby.ary_new();

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
                let child_rel = if rel_prefix.is_empty() {
                    entry_str.clone()
                } else {
                let is_bad: bool = ef.funcall("symlink?", (ruby.str_new(&full_str),))?;
                if is_bad { continue; }
                format!("{}/{}", rel_prefix, entry_str)
                };
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
                )?;
            } else {
                let is_bad: bool = ef.funcall("symlink?", (ruby.str_new(&full_str),))?;
                if is_bad { continue; }
let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
                let rust: RModule = jekyll.const_get("Rust")?;
                let bridge2: RModule = rust.const_get("Bridge")?;
                let has_header: bool =
                    bridge2.funcall("has_yaml_header?", (ruby.str_new(&full_str),))?;
                let rel_path = if rel_prefix.is_empty() {
                    entry_str
                } else {
                    format!("{}/{}", rel_prefix, entry_str)
                };
                if has_header {
                    pages.push(ruby.str_new(&rel_path))?;
                } else {
                    statics.push(ruby.str_new(&rel_path))?;
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
    walk(&ruby, site, file, bridge, ef, &base_path, "", &pages, &statics)?;

    let result = RHash::new();
    result.aset(Symbol::new("pages"), pages)?;
    result.aset(Symbol::new("static"), statics)?;
    Ok(result.into_value_with(&ruby))
}

fn reader_get_entries(site: Value, dir: RString, subfolder: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    let base: RString = site.funcall("in_source_dir", (dir, subfolder))?;
    let base_str = base.to_string()?;
    let exists: bool = file.funcall("exist?", (ruby.str_new(&base_str),))?;
    if !exists {
        return Ok(ruby.ary_new().into_value_with(&ruby));
    }

    let dir_mod: Value = ruby.class_object().const_get("Dir")?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;

    let block = ruby.proc_from_fn(|_args: &[Value], _block| {
        let ruby = crate::ruby_utils::ruby_handle()?;
        let dir_mod: Value = ruby.class_object().const_get("Dir")?;
        let glob: Value = dir_mod.funcall("glob", ("**/*",))?;
        Ok(glob)
    });
    let glob_val: Value = dir_mod.funcall_with_block("chdir", (ruby.str_new(&base_str),), block)?;
    let filtered: Value = bridge.funcall("entry_filter", (site, glob_val, base))?;
    let entries = RArray::try_convert(filtered)?;

    let mut out = ruby.ary_new();
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

