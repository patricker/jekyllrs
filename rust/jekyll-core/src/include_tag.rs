use std::fs;
use std::path::{Path, PathBuf};

use magnus::exception::ExceptionClass;
use magnus::{function, prelude::*, Error, IntoValue, RArray, RModule, Ruby, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("include_tag_resolve", function!(include_tag_resolve, 3))?;
    bridge.define_singleton_method(
        "include_relative_resolve",
        function!(include_relative_resolve, 2),
    )?;
    Ok(())
}

fn include_tag_resolve(context: Value, file: String, safe: bool) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let site = site_from_context(&ruby, context)?;
    let dirs = includes_directories(site)?;
    let safe = safe || site_safe(&site)?;

    if let Some(path) = resolve_from_dirs(&dirs, &file, safe)? {
        return Ok(ruby.str_new(&path).into_value_with(&ruby));
    }

    let message = include_error_message(&file, &dirs, safe);
    Err(io_error(&ruby, message))
}

fn include_relative_resolve(context: Value, file: String) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let site = site_from_context(&ruby, context)?;
    let dirs = relative_directories(&ruby, context, site)?;
    let safe = site_safe(&site)?;

    if let Some(path) = resolve_from_dirs(&dirs, &file, safe)? {
        return Ok(ruby.str_new(&path).into_value_with(&ruby));
    }

    let message = include_error_message(&file, &dirs, safe);
    Err(io_error(&ruby, message))
}

fn site_from_context(ruby: &Ruby, context: Value) -> Result<Value, Error> {
    let registers: Value = context.funcall::<_, _, Value>("registers", ())?;
    let site_key = ruby.sym_new("site").into_value_with(ruby);
    registers.funcall::<_, _, Value>("[]", (site_key,))
}

fn site_safe(site: &Value) -> Result<bool, Error> {
    let value: Value = site.funcall::<_, _, Value>("safe", ())?;
    Ok(value.to_bool())
}

fn includes_directories(site: Value) -> Result<Vec<String>, Error> {
    let dirs_value: Value = site.funcall::<_, _, Value>("includes_load_paths", ())?;
    array_to_strings(dirs_value)
}

fn relative_directories(ruby: &Ruby, context: Value, site: Value) -> Result<Vec<String>, Error> {
    let registers: Value = context.funcall::<_, _, Value>("registers", ())?;
    let page_key = ruby.sym_new("page").into_value_with(ruby);
    let page: Value = registers.funcall::<_, _, Value>("[]", (page_key,))?;

    if page.is_nil() {
        let source: Value = site.funcall::<_, _, Value>("source", ())?;
        let source = String::try_convert(source)?;
        return Ok(vec![source]);
    }

    let path_key = ruby.str_new("path").into_value_with(ruby);
    let page_path_value: Value = page.funcall::<_, _, Value>("[]", (path_key,))?;
    if page_path_value.is_nil() {
        let source: Value = site.funcall::<_, _, Value>("source", ())?;
        let source = String::try_convert(source)?;
        return Ok(vec![source]);
    }
    let mut resource_path = String::try_convert(page_path_value)?;

    let collection_key = ruby.str_new("collection").into_value_with(ruby);
    let collection_value: Value = page.funcall::<_, _, Value>("[]", (collection_key,))?;
    if collection_value.to_bool() {
        let config: Value = site.funcall::<_, _, Value>("config", ())?;
        let collections_dir_key = ruby.str_new("collections_dir").into_value_with(ruby);
        let collections_dir_value: Value =
            config.funcall::<_, _, Value>("[]", (collections_dir_key,))?;
        if !collections_dir_value.is_nil() {
            let collections_dir = String::try_convert(collections_dir_value)?;
            resource_path = Path::new(&collections_dir)
                .join(resource_path)
                .to_string_lossy()
                .into_owned();
        }
    }

    if resource_path.ends_with("/#excerpt") {
        let new_len = resource_path.len() - "/#excerpt".len();
        resource_path.truncate(new_len);
    }

    let relative_dir = Path::new(&resource_path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".".to_string());

    let dir_value = ruby.str_new(&relative_dir).into_value_with(ruby);
    let absolute: Value = site.funcall::<_, _, Value>("in_source_dir", (dir_value,))?;
    let absolute = String::try_convert(absolute)?;
    Ok(vec![absolute])
}

fn array_to_strings(value: Value) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    if let Some(array) = RArray::from_value(value) {
        for entry in array.each() {
            let item = entry?;
            let string_value: Value = item.funcall("to_s", ())?;
            result.push(String::try_convert(string_value)?);
        }
    }
    Ok(result)
}

fn resolve_from_dirs(dirs: &[String], file: &str, safe: bool) -> Result<Option<String>, Error> {
    for dir in dirs {
        let candidate = PathBuf::from(dir).join(file);
        if !candidate.is_file() {
            continue;
        }

        if safe {
            let dir_real = match fs::canonicalize(dir) {
                Ok(path) => path,
                Err(_) => continue,
            };
            let file_real = match fs::canonicalize(&candidate) {
                Ok(path) => path,
                Err(_) => continue,
            };
            if !file_real.starts_with(&dir_real) {
                continue;
            }
        }

        return Ok(Some(candidate.to_string_lossy().into_owned()));
    }
    Ok(None)
}

fn include_error_message(file: &str, dirs: &[String], safe: bool) -> String {
    let rendered_dirs = if dirs.is_empty() {
        "[]".to_string()
    } else {
        let joined = dirs
            .iter()
            .map(|d| format!("\"{}\"", d))
            .collect::<Vec<_>>()
            .join(", ");
        format!("[{}]", joined)
    };

    let base = format!(
        "Could not locate the included file '{}' in any of {}. Ensure it exists in one of those directories and",
        file, rendered_dirs
    );

    if safe {
        format!(
            "{} is not a symlink as those are not allowed in safe mode.",
            base
        )
    } else {
        format!(
            "{} , if it is a symlink, does not point outside your site source.",
            base
        )
    }
}

fn io_error(ruby: &Ruby, message: String) -> Error {
    let io_error_class = ruby
        .class_object()
        .const_get::<_, ExceptionClass>("IOError")
        .unwrap_or_else(|_| ruby.exception_runtime_error());
    Error::new(io_error_class, message)
}
