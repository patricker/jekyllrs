use magnus::{function, prelude::*, Error, RModule, RString, Ruby, Value};
use std::path::{Path, PathBuf};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method(
        "frontmatter_applies_path",
        function!(frontmatter_applies_path, 4),
    )?;
    bridge.define_singleton_method(
        "frontmatter_has_precedence",
        function!(frontmatter_has_precedence, 2),
    )?;
    Ok(())
}

fn frontmatter_applies_path(
    path: RString,
    scope_path_value: Value,
    site_source: RString,
    collections_dir_value: Value,
) -> Result<bool, Error> {
    let ruby = ruby_handle()?;

    let sanitized_path = sanitize_path(&path.to_string()?);

    // If scope path is not a String or is empty, apply to all paths.
    let rel_scope_path = match String::try_convert(scope_path_value) {
        Ok(s) => s,
        Err(_) => return Ok(true),
    };
    if rel_scope_path.is_empty() {
        return Ok(true);
    }

    let site_source = site_source.to_string()?;
    let collections_dir = match String::try_convert(collections_dir_value) {
        Ok(s) => s,
        Err(_) => String::new(),
    };

    if rel_scope_path.contains('*') {
        // Glob against absolute pattern
        let abs_scope_path = Path::new(&site_source).join(rel_scope_path);
        let abs_scope_str = abs_scope_path.to_string_lossy().to_string();

        let dir_module: Value = ruby.class_object().const_get("Dir")?;
        let glob_value: Value = dir_module.funcall("glob", (ruby.str_new(&abs_scope_str),))?;
        let entries = Vec::<String>::try_convert(glob_value).unwrap_or_default();

        for entry in entries {
            // Compute path relative to site_source
            let mut rel = match Path::new(&entry).strip_prefix(&site_source) {
                Ok(p) => p.to_string_lossy().to_string(),
                Err(_) => entry.clone(),
            };
            if rel.starts_with(std::path::MAIN_SEPARATOR) || rel.starts_with('/') {
                rel = rel.trim_start_matches(std::path::MAIN_SEPARATOR).trim_start_matches('/').to_string();
            }
            // Remove collections_dir prefix if present
            let rel_stripped = strip_collections_dir(&rel, &collections_dir);

            // Log debug like the Ruby implementation
            if !rel.is_empty() {
                let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
                let logger: Value = jekyll.funcall("logger", ())?;
                let _ = logger.funcall::<_, _, Value>(
                    "debug",
                    (ruby.str_new("Globbed Scope Path:"), ruby.str_new(&rel_stripped)),
                );
            }

            if path_is_subpath(&sanitized_path, &rel_stripped) {
                return Ok(true);
            }
        }
        return Ok(false);
    }

    let rel_stripped = strip_collections_dir(&rel_scope_path, &collections_dir);
    Ok(path_is_subpath(&sanitized_path, &rel_stripped))
}

fn frontmatter_has_precedence(old_scope: Value, new_scope: Value) -> Result<bool, Error> {
    // If no old scope, new has precedence
    if old_scope.is_nil() {
        return Ok(true);
    }

    let ruby = ruby_handle()?;

    let new_path = sanitize_path(&string_or_empty(new_scope.funcall::<_, _, Value>("[]", (ruby.str_new("path"),))?));
    let old_path = sanitize_path(&string_or_empty(old_scope.funcall::<_, _, Value>("[]", (ruby.str_new("path"),))?));

    if new_path.len() != old_path.len() {
        return Ok(new_path.len() >= old_path.len());
    }

    // If new scope has a type, it has precedence, else new has precedence only if old has no type
    let new_has_type: bool = new_scope.funcall("key?", (ruby.str_new("type"),))?;
    if new_has_type {
        return Ok(true);
    }
    let old_has_type: bool = old_scope.funcall("key?", (ruby.str_new("type"),))?;
    Ok(!old_has_type)
}

fn sanitize_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut s = path.to_string();
    while s.starts_with('/') || s.starts_with(std::path::MAIN_SEPARATOR) {
        s.remove(0);
    }
    s
}

fn strip_collections_dir(path: &str, collections_dir: &str) -> String {
    if collections_dir.is_empty() {
        return path.to_string();
    }
    let prefix1 = format!("{}/", collections_dir);
    let prefix2 = format!("{}{}", collections_dir, std::path::MAIN_SEPARATOR);
    if path.starts_with(&prefix1) {
        path[prefix1.len()..].to_string()
    } else if path.starts_with(&prefix2) {
        path[prefix2.len()..].to_string()
    } else {
        path.to_string()
    }
}

fn path_is_subpath(path: &str, parent_path: &str) -> bool {
    path.starts_with(parent_path)
}

fn string_or_empty(v: Value) -> String {
    String::try_convert(v).unwrap_or_default()
}
