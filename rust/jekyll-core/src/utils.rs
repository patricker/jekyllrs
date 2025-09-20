use magnus::r_hash::ForEach;
use magnus::{
    function, prelude::*, Error, IntoValue, RClass, RHash, RModule, RString, Ruby, Symbol,
    TryConvert, Value,
};
use once_cell::sync::Lazy;
use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
};

use crate::ruby_utils::ruby_handle;

static SAFE_GLOB_CACHE: Lazy<Mutex<HashMap<(String, String, i64), Vec<String>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("safe_glob", function!(safe_glob, 3))?;
    bridge.define_singleton_method(
        "pluralized_array_from_hash",
        function!(pluralized_array_from_hash, 3),
    )?;
    bridge.define_singleton_method("symbolize_hash_keys", function!(symbolize_hash_keys, 1))?;
    bridge.define_singleton_method("stringify_hash_keys", function!(stringify_hash_keys, 1))?;
    bridge.define_singleton_method("mergable?", function!(mergable, 1))?;
    bridge.define_singleton_method("duplicable?", function!(duplicable, 1))?;
    bridge.define_singleton_method("titleize_slug", function!(titleize_slug, 1))?;
    bridge.define_singleton_method("add_permalink_suffix", function!(add_permalink_suffix, 2))?;
    Ok(())
}

fn safe_glob(dir: RString, patterns: Value, flags: i64) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let directory = dir.to_string()?;

    if !Path::new(&directory).exists() {
        return Ok(ruby.ary_new().into_value_with(&ruby));
    }

    let pattern = patterns_to_pattern(&ruby, patterns)?;
    if pattern.is_empty() {
        let array = ruby.ary_new();
        array.push(dir)?;
        return Ok(array.into_value_with(&ruby));
    }

    let key = (directory.clone(), pattern.clone(), flags);
    if let Some(cached) = SAFE_GLOB_CACHE
        .lock()
        .expect("safe_glob cache poisoned")
        .get(&key)
        .cloned()
    {
        let array = ruby.ary_new();
        for entry in cached {
            array.push(ruby.str_new(&entry))?;
        }
        return Ok(array.into_value_with(&ruby));
    }

    let results = glob_with_dir(&ruby, &directory, &pattern, flags)?;
    let cached_results = results.clone();

    SAFE_GLOB_CACHE
        .lock()
        .expect("safe_glob cache poisoned")
        .insert(key, cached_results);

    let array = ruby.ary_new();
    for entry in results {
        array.push(ruby.str_new(&entry))?;
    }

    Ok(array.into_value_with(&ruby))
}

fn patterns_to_pattern(ruby: &Ruby, patterns: Value) -> Result<String, Error> {
    if patterns.is_nil() {
        return Ok(String::new());
    }

    if let Ok(string) = String::try_convert(patterns) {
        return Ok(string);
    }

    let file = ruby.class_object().const_get::<_, Value>("File")?;
    let joined: RString = file.funcall::<_, _, RString>("join", (patterns,))?;
    joined.to_string()
}

fn glob_with_dir(ruby: &Ruby, dir: &str, pattern: &str, flags: i64) -> Result<Vec<String>, Error> {
    let results: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let results_capture = Arc::clone(&results);
    let dir_owned = dir.to_string();
    let pattern_owned = pattern.to_string();

    let block = ruby.proc_from_fn(move |_args, _block| {
        let ruby_inner = ruby_handle()?;
        let dir_module = ruby_inner.class_object().const_get::<_, Value>("Dir")?;
        let file = ruby_inner.class_object().const_get::<_, Value>("File")?;

        let glob_value: Value =
            dir_module.funcall::<_, _, Value>("glob", (pattern_owned.as_str(), flags))?;
        let entries = Vec::<String>::try_convert(glob_value)?;

        let mut collected = Vec::with_capacity(entries.len());
        let array = ruby_inner.ary_new();

        for entry in entries {
            let full_path: RString =
                file.funcall::<_, _, RString>("join", (dir_owned.as_str(), entry.as_str()))?;
            let path_string = full_path.to_string()?;
            collected.push(path_string.clone());
            array.push(full_path)?;
        }

        results_capture
            .lock()
            .expect("safe_glob results cache poisoned")
            .extend(collected.into_iter());

        Ok(array.into_value_with(&ruby_inner))
    });

    let dir_module = ruby.class_object().const_get::<_, Value>("Dir")?;
    let _ = dir_module.funcall_with_block::<_, _, Value>("chdir", (ruby.str_new(dir),), block)?;

    let final_results = results
        .lock()
        .expect("safe_glob results cache poisoned")
        .clone();

    Ok(final_results)
}

pub(crate) fn pluralized_array_from_hash(
    hash: Value,
    singular_key: Value,
    plural_key: Value,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let array = ruby.ary_new();

    if RHash::from_value(hash).is_none() {
        return Ok(array.into_value_with(&ruby));
    }

    if let Some(value) = value_from_singular(hash, singular_key)? {
        array.push(value)?;
    } else if let Some(value) = value_from_plural(&ruby, hash, plural_key)? {
        array.push(value)?;
    }

    array.funcall::<_, _, Value>("flatten!", ())?;
    array.funcall::<_, _, Value>("compact!", ())?;

    Ok(array.into_value_with(&ruby))
}

fn symbolize_hash_keys(hash: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let source = match RHash::from_value(hash) {
        Some(hash) => hash,
        None => return Ok(hash),
    };

    let result = ruby.hash_new();
    source.foreach(|key: Value, value: Value| {
        let symbol = match key.funcall::<_, _, Value>("to_sym", ()) {
            Ok(sym) => sym,
            Err(_) => key,
        };
        result.aset(symbol, value)?;
        Ok(ForEach::Continue)
    })?;

    Ok(result.into_value_with(&ruby))
}

fn stringify_hash_keys(hash: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let source = match RHash::from_value(hash) {
        Some(hash) => hash,
        None => return Ok(hash),
    };

    let result = ruby.hash_new();
    source.foreach(|key: Value, value: Value| {
        let string = match key.funcall::<_, _, Value>("to_s", ()) {
            Ok(str_val) => str_val,
            Err(_) => key,
        };
        result.aset(string, value)?;
        Ok(ForEach::Continue)
    })?;

    Ok(result.into_value_with(&ruby))
}

fn mergable(value: Value) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    if value.is_kind_of(ruby.class_hash()) {
        return Ok(true);
    }

    let drop_class: Value = {
        let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
        let drops: RModule = jekyll.const_get("Drops")?;
        drops.const_get("Drop")?
    };
    let drop_module = RClass::try_convert(drop_class)?;
    Ok(value.is_kind_of(drop_module))
}

fn duplicable(value: Value) -> Result<bool, Error> {
    if value.is_nil() {
        return Ok(false);
    }

    if !value.to_bool() {
        return Ok(false);
    }

    let ruby = ruby_handle()?;
    if value.is_kind_of(ruby.class_symbol()) {
        return Ok(false);
    }

    if value.is_kind_of(ruby.class_numeric()) {
        return Ok(false);
    }

    Ok(true)
}

fn value_from_singular(hash: Value, key: Value) -> Result<Option<Value>, Error> {
    let has_key: bool = hash.funcall("key?", (key,))?;
    if has_key {
        let value = hash.funcall::<_, _, Value>("[]", (key,))?;
        if value.is_nil() {
            return Ok(None);
        }
        return Ok(Some(value));
    }

    let default_proc: Value = hash.funcall("default_proc", ())?;
    if default_proc.is_nil() {
        return Ok(None);
    }

    let value = hash.funcall::<_, _, Value>("[]", (key,))?;
    if value.is_nil() || !value.to_bool() {
        return Ok(None);
    }

    Ok(Some(value))
}

fn value_from_plural(ruby: &Ruby, hash: Value, key: Value) -> Result<Option<Value>, Error> {
    let has_key: bool = hash.funcall("key?", (key,))?;
    let default_proc: Value = hash.funcall("default_proc", ())?;

    if !has_key && default_proc.is_nil() {
        return Ok(None);
    }

    let value = hash.funcall::<_, _, Value>("[]", (key,))?;
    if value.is_nil() {
        return Ok(None);
    }

    if value.is_kind_of(ruby.class_string()) {
        let split = value.funcall::<_, _, Value>("split", ())?;
        return Ok(Some(split));
    }

    if value.is_kind_of(ruby.class_array()) {
        let compact = value.funcall::<_, _, Value>("compact", ())?;
        return Ok(Some(compact));
    }

    Ok(None)
}

fn titleize_slug(slug: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let slug_str = slug.to_string()?;
    let result = titleize_slug_internal(&slug_str);
    Ok(ruby.str_new(&result).into_value_with(&ruby))
}

fn titleize_slug_internal(slug: &str) -> String {
    slug.split('-')
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(part: &str) -> String {
    if part.is_empty() {
        return String::new();
    }

    let lowercase = part.to_lowercase();
    let mut chars = lowercase.chars();
    let Some(first) = chars.next() else {
        return lowercase;
    };

    let mut capitalized = String::new();
    capitalized.extend(first.to_uppercase());
    capitalized.push_str(chars.as_str());
    capitalized
}

fn add_permalink_suffix(template: RString, permalink_style: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let template_str = template.to_string()?;
    let style = parse_permalink_style(permalink_style, &ruby)?;
    let result = add_permalink_suffix_internal(&template_str, style);
    Ok(ruby.str_new(&result).into_value_with(&ruby))
}

#[derive(Debug, PartialEq, Eq)]
enum PermalinkStyle {
    Pretty,
    Date,
    Ordinal,
    NoneSymbol,
    Other(String),
}

fn parse_permalink_style(value: Value, ruby: &Ruby) -> Result<PermalinkStyle, Error> {
    if let Ok(symbol) = Symbol::try_convert(value) {
        if symbol == ruby.to_symbol("pretty") {
            return Ok(PermalinkStyle::Pretty);
        }
        if symbol == ruby.to_symbol("date") {
            return Ok(PermalinkStyle::Date);
        }
        if symbol == ruby.to_symbol("ordinal") {
            return Ok(PermalinkStyle::Ordinal);
        }
        if symbol == ruby.to_symbol("none") {
            return Ok(PermalinkStyle::NoneSymbol);
        }
    }

    if value.is_nil() {
        return Ok(PermalinkStyle::Other(String::new()));
    }

    let style_string = value.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    Ok(PermalinkStyle::Other(style_string))
}

fn add_permalink_suffix_internal(template: &str, style: PermalinkStyle) -> String {
    let mut updated = template.to_owned();
    match style {
        PermalinkStyle::Pretty => updated.push('/'),
        PermalinkStyle::Date | PermalinkStyle::Ordinal | PermalinkStyle::NoneSymbol => {
            updated.push_str(":output_ext")
        }
        PermalinkStyle::Other(name) => {
            if name.ends_with('/') {
                updated.push('/');
            }
            if name.ends_with(":output_ext") {
                updated.push_str(":output_ext");
            }
        }
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titleize_slug_capitalizes_words() {
        let result = titleize_slug_internal("This-is-a-Long-title-with-Mixed-capitalization");
        assert_eq!("This Is A Long Title With Mixed Capitalization", result);
    }

    #[test]
    fn add_permalink_suffix_handles_styles() {
        assert_eq!(
            "/:basename/",
            add_permalink_suffix_internal("/:basename", PermalinkStyle::Pretty)
        );

        assert_eq!(
            "/:basename:output_ext",
            add_permalink_suffix_internal("/:basename", PermalinkStyle::Date)
        );

        assert_eq!(
            "/:basename:output_ext",
            add_permalink_suffix_internal("/:basename", PermalinkStyle::Ordinal)
        );

        assert_eq!(
            "/:basename:output_ext",
            add_permalink_suffix_internal("/:basename", PermalinkStyle::NoneSymbol)
        );

        assert_eq!(
            "/:basename/",
            add_permalink_suffix_internal(
                "/:basename",
                PermalinkStyle::Other("/:title/".to_string())
            )
        );

        assert_eq!(
            "/:basename:output_ext",
            add_permalink_suffix_internal(
                "/:basename",
                PermalinkStyle::Other("/:title:output_ext".to_string())
            )
        );

        assert_eq!(
            "/:basename",
            add_permalink_suffix_internal(
                "/:basename",
                PermalinkStyle::Other("/:title".to_string())
            )
        );
    }


    #[test]
    fn add_permalink_suffix_handles_nil_and_unknown_symbol() {
        // Internal helper expects parsed style; simulate by calling internal with Other("")
        assert_eq!(
            "/:basename",
            add_permalink_suffix_internal("/:basename", PermalinkStyle::Other(String::new()))
        );

        // Unknown symbol becomes a string via to_s and should not modify template
        assert_eq!(
            "/:basename",
            add_permalink_suffix_internal(
                "/:basename",
                PermalinkStyle::Other("foo".to_string())
            )
        );
    }

    #[test]
    fn add_permalink_suffix_handles_extra_trailing_slashes_in_style() {
        assert_eq!(
            "/:basename/",
            add_permalink_suffix_internal(
                "/:basename",
                PermalinkStyle::Other("/:title///".to_string())
            )
        );
    }
}
