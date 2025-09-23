use magnus::r_regexp::RRegexp;
use magnus::{function, prelude::*, Error, IntoValue, RArray, RModule, RString, Ruby, Value};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::PathBuf;

use crate::ruby_utils::ruby_handle;

static SPECIAL_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[._#~]").expect("special regex"));

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("entry_filter", function!(entry_filter, 3))?;
    Ok(())
}

fn entry_filter(
    site: Value,
    entries: Value,
    base_directory: Option<RString>,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file_class = ruby.class_object().const_get::<_, Value>("File")?;

    let include_value: Value = site.funcall("include", ())?;
    let exclude_value: Value = site.funcall("exclude", ())?;
    let exclude_diff = exclude_value.funcall("-", (include_value,))?;

    let source: String = site.funcall("source", ())?;
    let include_patterns = extract_patterns(&ruby, include_value)?;
    let exclude_patterns = extract_patterns(&ruby, exclude_diff)?;

    let mut base_dir = match base_directory {
        Some(dir) => dir.to_string()?,
        None => String::new(),
    };
    if !base_dir.is_empty() && base_dir.starts_with(&source) {
        base_dir = base_dir[source.len()..].to_string();
    }

    let entries_array = RArray::try_convert(entries)?;
    let mut filtered = Vec::new();

    for entry_value in entries_array.each() {
        let entry_value = entry_value?;
        let entry_str = String::try_convert(entry_value)?;

        if entry_str.ends_with('.') {
            continue;
        }

        let included = matches_patterns(&ruby, file_class, &source, &include_patterns, &entry_str)?;

        let relative_path = relative_to_source(&ruby, file_class, &base_dir, &entry_str)?;

        if !included
            && matches_patterns(
                &ruby,
                file_class,
                &source,
                &exclude_patterns,
                &relative_path,
            )?
        {
            continue;
        }

        if is_symlink_filtered(&ruby, site, file_class, &base_dir, &entry_str)? {
            continue;
        }

        if included {
            filtered.push(entry_value);
            continue;
        }

        if is_special(&entry_str) || entry_str.ends_with('~') {
            continue;
        }

        filtered.push(entry_value);
    }

    let array = ruby.ary_new();
    for value in filtered {
        array.push(value)?;
    }

    Ok(array.into_value_with(&ruby))
}

fn extract_patterns(ruby: &Ruby, list: Value) -> Result<Vec<Pattern>, Error> {
    let mut patterns = Vec::new();
    if let Some(array) = RArray::from_value(list) {
        for item in array.each() {
            let item = item?;
            if item.is_nil() {
                continue;
            }

            if let Ok(string) = String::try_convert(item) {
                patterns.push(Pattern::String(string));
            } else if item.respond_to("to_str", false)? {
                let string: String = item.funcall("to_str", ())?;
                patterns.push(Pattern::String(string));
            } else if let Ok(regexp) = RRegexp::try_convert(item) {
                patterns.push(Pattern::Regex(regexp.into_value_with(ruby)));
            }
        }
    }
    Ok(patterns)
}

fn matches_patterns(
    ruby: &Ruby,
    file_class: Value,
    source: &str,
    patterns: &[Pattern],
    entry: &str,
) -> Result<bool, Error> {
    if patterns.is_empty() {
        return Ok(false);
    }

    let entry_with_source = join_paths(ruby, file_class, source, entry)?;
    let entry_value = ruby.str_new(entry_with_source.as_str());
    let entry_is_directory: bool = file_class.funcall("directory?", (entry_value,))?;

    for pattern in patterns {
        match pattern {
            Pattern::String(pattern_str) => {
                let pattern_with_source = join_paths(ruby, file_class, source, pattern_str)?;

                let fnmatch_result: bool = file_class.funcall(
                    "fnmatch?",
                    (
                        ruby.str_new(pattern_with_source.as_str()),
                        ruby.str_new(entry_with_source.as_str()),
                    ),
                )?;

                if fnmatch_result
                    || entry_with_source.starts_with(&pattern_with_source)
                    || (entry_is_directory
                        && format!("{}/", entry_with_source) == pattern_with_source)
                {
                    return Ok(true);
                }
            }
            Pattern::Regex(regexp) => {
                let matched: bool =
                    regexp.funcall("match?", (ruby.str_new(entry_with_source.as_str()),))?;
                if matched {
                    return Ok(true);
                }
            }
        }
    }

    Ok(false)
}

fn relative_to_source(
    ruby: &Ruby,
    file_class: Value,
    base_directory: &str,
    entry: &str,
) -> Result<String, Error> {
    join_paths(ruby, file_class, base_directory, entry)
}

fn is_symlink_filtered(
    ruby: &Ruby,
    site: Value,
    file_class: Value,
    base_directory: &str,
    entry: &str,
) -> Result<bool, Error> {
    let safe: bool = site.funcall("safe", ())?;
    if !safe {
        return Ok(false);
    }

    // Evaluate symlink status against the path relative to the filter base directory
    let full_path = join_paths(ruby, file_class, base_directory, entry)?;
    let is_symlink: bool = file_class.funcall("symlink?", (ruby.str_new(full_path.as_str()),))?;
    if !is_symlink {
        return Ok(false);
    }

    let realpath: RString = file_class.funcall("realpath", (ruby.str_new(full_path.as_str()),))?;
    let root: RString = site.funcall("in_source_dir", ())?;

    let real = PathBuf::from(realpath.to_string()?);
    let root_path = PathBuf::from(root.to_string()?);

    Ok(!real.starts_with(&root_path))
}

fn is_special(entry: &str) -> bool {
    if SPECIAL_RE.is_match(entry) {
        return true;
    }

    entry
        .split('/')
        .last()
        .map(|segment| SPECIAL_RE.is_match(segment))
        .unwrap_or(false)
}

fn join_paths(ruby: &Ruby, file_class: Value, base: &str, item: &str) -> Result<String, Error> {
    let joined: RString = file_class.funcall(
        "join",
        (
            ruby.str_new(base).into_value_with(ruby),
            ruby.str_new(item).into_value_with(ruby),
        ),
    )?;
    joined.to_string()
}

enum Pattern {
    String(String),
    Regex(Value),
}
