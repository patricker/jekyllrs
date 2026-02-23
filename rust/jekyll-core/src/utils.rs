use magnus::r_hash::ForEach;
use magnus::{
    function, prelude::*, Error, IntoValue, RArray, RClass, RHash, RModule, RString, Ruby, Symbol,
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

static INT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"^\s*-?\d+\s*$").expect("valid integer regex"));
static FLOAT_RE: Lazy<regex::Regex> =
    Lazy::new(|| regex::Regex::new(r"^\s*-?(?:\d+\.?\d*|\.\d+)\s*$").expect("valid float regex"));

fn hash_lookup(ruby: &Ruby, h: Value, key: &str) -> Result<Value, Error> {
    let sym = ruby.to_symbol(key);
    let mut v: Value = h.funcall("[]", (sym,))?;
    if v.is_nil() {
        v = h.funcall("[]", (ruby.str_new(key),))?;
    }
    Ok(v)
}

fn coerce_lookup_target(ruby: &Ruby, value: Value) -> Result<Value, Error> {
    if value.is_nil() || RHash::from_value(value).is_some() {
        return Ok(value);
    }

    if value.respond_to("to_liquid", false)? {
        let liquid: Value = value.funcall("to_liquid", ())?;
        if liquid.is_nil() {
            return Ok(liquid);
        }
        let same: bool = value.funcall("equal?", (liquid,))?;
        if !same {
            return coerce_lookup_target(ruby, liquid);
        }
    }

    if value.respond_to("data", false)? {
        let data: Value = value.funcall("data", ())?;
        if data.is_nil() {
            return Ok(data);
        }
        return coerce_lookup_target(ruby, data);
    }

    Ok(value)
}

fn arrayify_for_filter(ruby: &Ruby, value: Value) -> Result<RArray, Error> {
    if value.is_nil() {
        return Ok(ruby.ary_new());
    }

    if let Some(array) = RArray::from_value(value) {
        return Ok(array);
    }

    if value.respond_to("to_ary", false)? {
        let converted: Value = value.funcall("to_ary", ())?;
        return arrayify_for_filter(ruby, converted);
    }

    if value.respond_to("to_a", false)? {
        let converted: Value = value.funcall("to_a", ())?;
        return arrayify_for_filter(ruby, converted);
    }

    let array = ruby.ary_new();
    array.push(value)?;
    Ok(array)
}

fn value_to_string(value: Value) -> Result<String, Error> {
    let rs: RString = value.funcall("to_s", ())?;
    rs.to_string()
}

fn lookup_key(ruby: &Ruby, current: Value, key: &str) -> Result<Value, Error> {
    if RHash::from_value(current).is_some() {
        return hash_lookup(ruby, current, key);
    }

    if current.respond_to("[]", false)? {
        let key_str = ruby.str_new(key);
        let mut value: Value = current.funcall("[]", (key_str,))?;
        if value.is_nil() {
            let sym = ruby.to_symbol(key);
            value = current.funcall("[]", (sym,))?;
        }
        return Ok(value);
    }

    if current.respond_to(key, false)? {
        let value: Value = current.funcall(key, ())?;
        return Ok(value);
    }

    Ok(ruby.qnil().into_value_with(ruby))
}

fn fetch_nested_prop(ruby: &Ruby, obj: Value, prop: &str) -> Result<Value, Error> {
    if prop.is_empty() {
        return Ok(ruby.qnil().into_value_with(ruby));
    }

    let parts: Vec<&str> = prop.split('.').collect();
    if parts.is_empty() {
        return Ok(ruby.qnil().into_value_with(ruby));
    }

    let mut current = coerce_lookup_target(ruby, obj)?;

    for (idx, part) in parts.iter().enumerate() {
        if part.is_empty() {
            return Ok(ruby.qnil().into_value_with(ruby));
        }

        let value = lookup_key(ruby, current, part)?;
        if idx + 1 == parts.len() {
            return Ok(value);
        }

        current = coerce_lookup_target(ruby, value)?;
        if current.is_nil() {
            return Ok(current);
        }
    }

    Ok(current)
}

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
    bridge.define_singleton_method("normalize_whitespace", function!(normalize_whitespace, 1))?;
    bridge.define_singleton_method("number_of_words", function!(number_of_words, 2))?;
    bridge.define_singleton_method("where_filter_fast", function!(where_filter_fast2, 3))?;
    bridge.define_singleton_method("sort_filter_fast", function!(sort_filter_fast, 3))?;
    bridge.define_singleton_method("group_by_fast", function!(group_by_fast, 2))?;
    bridge.define_singleton_method("find_filter_fast", function!(find_filter_fast, 3))?;
    bridge.define_singleton_method("where_exp_fast", function!(where_exp_fast, 3))?;
    bridge.define_singleton_method("map_filter_fast", function!(map_filter_fast, 2))?;
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

fn normalize_whitespace(input: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let s = input.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !in_ws {
                out.push(' ');
                in_ws = true;
            }
        } else {
            out.push(ch);
            in_ws = false;
        }
    }
    let trimmed = out.trim().to_string();
    Ok(ruby.str_new(&trimmed).into_value_with(&ruby))
}

fn number_of_words(input: Value, mode: Option<Value>) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let s = input.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let use_cjk = match mode {
        Some(v) => {
            if v.is_nil() {
                false
            } else {
                true
            }
        }
        None => false,
    };

    if use_cjk {
        let cjk_re = regex::Regex::new(r"[\p{Han}\p{Katakana}\p{Hiragana}\p{Hangul}]").unwrap();
        let word_re =
            regex::Regex::new(r"[^\p{Han}\p{Katakana}\p{Hiragana}\p{Hangul}\s]+").unwrap();
        let cjk_count = cjk_re.find_iter(&s).count();
        let count = if cjk_count == 0 {
            s.split_whitespace().count()
        } else {
            cjk_count + word_re.find_iter(&s).count()
        };
        Ok((count as i64).into_value_with(&ruby))
    } else {
        let count = s.split_whitespace().count();
        Ok((count as i64).into_value_with(&ruby))
    }
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
            add_permalink_suffix_internal("/:basename", PermalinkStyle::Other("foo".to_string()))
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

// appended new where_filter_fast

fn is_empty_value(_ruby: &magnus::Ruby, val: Value) -> Result<bool, Error> {
    if let Some(rs) = RString::from_value(val) {
        return Ok(rs.to_string()?.is_empty());
    }
    if let Some(a) = RArray::from_value(val) {
        let len: i64 = i64::try_convert(a.funcall("length", ())?)?;
        return Ok(len == 0);
    }
    if let Some(h) = RHash::from_value(val) {
        let len: i64 = i64::try_convert(h.funcall("length", ())?)?;
        return Ok(len == 0);
    }
    Ok(false)
}

fn is_blank_value(_ruby: &magnus::Ruby, val: Value) -> Result<bool, Error> {
    if val.is_nil() {
        return Ok(true);
    }
    if let Some(rs) = RString::from_value(val) {
        return Ok(rs.to_string()?.trim().is_empty());
    }
    if let Some(a) = RArray::from_value(val) {
        let len: i64 = i64::try_convert(a.funcall("length", ())?)?;
        return Ok(len == 0);
    }
    if let Some(h) = RHash::from_value(val) {
        let len: i64 = i64::try_convert(h.funcall("length", ())?)?;
        return Ok(len == 0);
    }
    Ok(false)
}

/// Evaluate a simple Liquid comparison expression for a single object.
///
/// Supports operators: ==, !=, >, <, >=, <=, contains
/// `var_name` is the loop variable name. `obj` is the current element.
/// Returns Ok(true/false) or Err if the expression is not parseable here.
fn eval_where_exp_condition(
    ruby: &Ruby,
    var_name: &str,
    obj: Value,
    condition: &str,
) -> Result<Option<bool>, Error> {
    let prefix = format!("{}.", var_name);

    // Determine operator and split
    // Order matters: check >= and <= before > and <, and != before contains
    let (lhs_raw, op, rhs_raw) = if let Some(pos) = condition.find(" contains ") {
        (&condition[..pos], "contains", condition[pos + 10..].trim())
    } else if let Some(pos) = condition.find(">=") {
        (condition[..pos].trim(), ">=", condition[pos + 2..].trim())
    } else if let Some(pos) = condition.find("<=") {
        (condition[..pos].trim(), "<=", condition[pos + 2..].trim())
    } else if let Some(pos) = condition.find("!=") {
        (condition[..pos].trim(), "!=", condition[pos + 2..].trim())
    } else if let Some(pos) = condition.find("==") {
        (condition[..pos].trim(), "==", condition[pos + 2..].trim())
    } else if let Some(pos) = condition.find('>') {
        (condition[..pos].trim(), ">", condition[pos + 1..].trim())
    } else if let Some(pos) = condition.find('<') {
        (condition[..pos].trim(), "<", condition[pos + 1..].trim())
    } else {
        // Bare truthiness check: `item.published` (no operator)
        let lhs = condition.trim();
        if lhs.starts_with(&prefix) {
            let prop = &lhs[prefix.len()..];
            if prop.is_empty() {
                return Ok(None);
            }
            let v = fetch_nested_prop(ruby, obj, prop)?;
            // Liquid truthiness: nil and false are falsy, everything else truthy.
            let truthy = !v.is_nil() && v.to_bool();
            return Ok(Some(truthy));
        }
        return Ok(None);
    };

    let lhs = lhs_raw.trim();

    // LHS must be a property access on the loop variable
    if !lhs.starts_with(&prefix) {
        return Ok(None);
    }
    let prop = &lhs[prefix.len()..];
    if prop.is_empty() {
        return Ok(None);
    }

    let v = fetch_nested_prop(ruby, obj, prop)?;
    let rhs = rhs_raw.trim();

    // Parse the RHS literal
    enum RhsVal {
        Nil,
        Empty,
        Blank,
        Bool(bool),
        Literal(String),
    }
    let rhs_val = if rhs == "nil" || rhs == "null" {
        RhsVal::Nil
    } else if rhs == "empty" {
        RhsVal::Empty
    } else if rhs == "blank" {
        RhsVal::Blank
    } else if rhs == "true" {
        RhsVal::Bool(true)
    } else if rhs == "false" {
        RhsVal::Bool(false)
    } else if (rhs.starts_with('\'') && rhs.ends_with('\''))
        || (rhs.starts_with('"') && rhs.ends_with('"'))
    {
        RhsVal::Literal(rhs[1..rhs.len() - 1].to_string())
    } else {
        RhsVal::Literal(rhs.to_string())
    };

    // ----- contains -----
    if op == "contains" {
        let target = match &rhs_val {
            RhsVal::Nil | RhsVal::Empty | RhsVal::Blank => return Ok(Some(false)),
            RhsVal::Bool(b) => b.to_string(),
            RhsVal::Literal(s) => s.clone(),
        };
        if v.is_nil() {
            return Ok(Some(false));
        }
        // String contains substring
        if let Some(rs) = RString::from_value(v) {
            return Ok(Some(rs.to_string()?.contains(&target)));
        }
        // Array contains element
        if let Some(arr) = RArray::from_value(v) {
            let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
            for idx in 0..len {
                let el: Value = arr.funcall("[]", (idx,))?;
                if !el.is_nil() {
                    let el_s = value_to_string(el)?;
                    if el_s == target {
                        return Ok(Some(true));
                    }
                }
            }
            return Ok(Some(false));
        }
        return Ok(Some(false));
    }

    // ----- comparison operators -----
    match &rhs_val {
        RhsVal::Nil => {
            let result = match op {
                "==" => v.is_nil(),
                "!=" => !v.is_nil(),
                _ => false, // >, <, >=, <= with nil => false
            };
            Ok(Some(result))
        }
        RhsVal::Empty => {
            let is_emp = if v.is_nil() { true } else { is_empty_value(ruby, v)? };
            let result = match op {
                "==" => is_emp,
                "!=" => !is_emp,
                _ => false,
            };
            Ok(Some(result))
        }
        RhsVal::Blank => {
            let is_blk = is_blank_value(ruby, v)?;
            let result = match op {
                "==" => is_blk,
                "!=" => !is_blk,
                _ => false,
            };
            Ok(Some(result))
        }
        RhsVal::Bool(expected) => {
            let v_bool = !v.is_nil() && v.to_bool();
            let result = match op {
                "==" => v_bool == *expected,
                "!=" => v_bool != *expected,
                _ => false,
            };
            Ok(Some(result))
        }
        RhsVal::Literal(ref s) => {
            if v.is_nil() {
                // nil compared with a literal
                return Ok(Some(match op {
                    "!=" => true,
                    _ => false,
                }));
            }
            // Try numeric comparison first
            let vs_str = value_to_string(v)?;
            let lhs_num = vs_str.trim().parse::<f64>().ok();
            let rhs_num = s.trim().parse::<f64>().ok();
            if let (Some(ln), Some(rn)) = (lhs_num, rhs_num) {
                let result = match op {
                    "==" => (ln - rn).abs() < f64::EPSILON,
                    "!=" => (ln - rn).abs() >= f64::EPSILON,
                    ">" => ln > rn,
                    "<" => ln < rn,
                    ">=" => ln >= rn,
                    "<=" => ln <= rn,
                    _ => false,
                };
                return Ok(Some(result));
            }
            // String comparison
            let result = match op {
                "==" => vs_str == *s,
                "!=" => vs_str != *s,
                ">" => vs_str.as_str() > s.as_str(),
                "<" => (vs_str.as_str()) < s.as_str(),
                ">=" => vs_str.as_str() >= s.as_str(),
                "<=" => vs_str.as_str() <= s.as_str(),
                _ => false,
            };
            Ok(Some(result))
        }
    }
}

fn where_exp_fast(input: Value, variable: Value, expression: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let var: String = variable.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let expr_s: String = expression
        .funcall::<_, _, RString>("to_s", ())?
        .to_string()?;
    let expr = expr_s.trim();

    // Bail on `site.` references — these need the Liquid runtime context
    // which we don't have access to in this code path.
    if expr.contains(" site.") || expr.starts_with("site.") {
        return Ok(ruby.qnil().into_value_with(&ruby));
    }

    // Try to split on `and` / `or` connectors (only top-level, no nesting)
    let has_and = expr.contains(" and ");
    let has_or = expr.contains(" or ");

    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    let out = ruby.ary_new();

    if has_and || has_or {
        // Split on `and` / `or`. We handle a flat chain:
        //   cond1 and cond2 and cond3
        //   cond1 or cond2 or cond3
        // Mixed `and`/`or` without parens: evaluate left-to-right (Liquid semantics).
        // Split into clauses and connectors.
        let mut clauses: Vec<&str> = Vec::new();
        let mut connectors: Vec<&str> = Vec::new();
        let mut remainder = expr;
        loop {
            let and_pos = remainder.find(" and ");
            let or_pos = remainder.find(" or ");
            match (and_pos, or_pos) {
                (Some(ap), Some(op)) => {
                    if ap < op {
                        clauses.push(remainder[..ap].trim());
                        connectors.push("and");
                        remainder = &remainder[ap + 5..];
                    } else {
                        clauses.push(remainder[..op].trim());
                        connectors.push("or");
                        remainder = &remainder[op + 4..];
                    }
                }
                (Some(ap), None) => {
                    clauses.push(remainder[..ap].trim());
                    connectors.push("and");
                    remainder = &remainder[ap + 5..];
                }
                (None, Some(op)) => {
                    clauses.push(remainder[..op].trim());
                    connectors.push("or");
                    remainder = &remainder[op + 4..];
                }
                (None, None) => {
                    clauses.push(remainder.trim());
                    break;
                }
            }
        }

        for i in 0..len {
            let obj: Value = arr.funcall("[]", (i,))?;
            let mut result: Option<bool> = None;
            for (idx, clause) in clauses.iter().enumerate() {
                let clause_result = eval_where_exp_condition(&ruby, &var, obj, clause)?;
                let Some(cr) = clause_result else {
                    // Unsupported expression — bail to Ruby
                    return Ok(ruby.qnil().into_value_with(&ruby));
                };
                if idx == 0 {
                    result = Some(cr);
                } else {
                    let connector = connectors[idx - 1];
                    let prev = result.unwrap_or(false);
                    result = Some(if connector == "and" {
                        prev && cr
                    } else {
                        prev || cr
                    });
                }
            }
            if result.unwrap_or(false) {
                out.push(obj)?;
            }
        }
    } else {
        // Single condition
        for i in 0..len {
            let obj: Value = arr.funcall("[]", (i,))?;
            let matched = eval_where_exp_condition(&ruby, &var, obj, expr)?;
            let Some(m) = matched else {
                return Ok(ruby.qnil().into_value_with(&ruby));
            };
            if m {
                out.push(obj)?;
            }
        }
    }
    Ok(out.into_value_with(&ruby))
}

enum TargetKind {
    Nil,
    MethodLiteral(String),
    String(String),
}

fn where_filter_fast2(input: Value, property: Value, target: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let prop_str = property.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let out = ruby.ary_new();
    let target_kind = if target.is_nil() {
        TargetKind::Nil
    } else {
        let cls: Value = target.funcall("class", ())?;
        let name_rs: RString = cls.funcall("name", ())?;
        let name = name_rs.to_string()?;
        let value_str = value_to_string(target)?;
        if name == "Liquid::Expression::MethodLiteral" {
            TargetKind::MethodLiteral(value_str)
        } else {
            TargetKind::String(value_str)
        }
    };
    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        let val = fetch_nested_prop(&ruby, obj, &prop_str)?;
        let matched = match &target_kind {
            TargetKind::Nil => val.is_nil(),
            TargetKind::MethodLiteral(_expected) => {
                // Treat method literals as Liquid's special literals like `empty`.
                // Match nil as empty too.
                if val.is_nil() {
                    true
                } else if let Some(rs) = RString::from_value(val) {
                    rs.to_string()?.is_empty()
                } else if let Some(a) = RArray::from_value(val) {
                    let alen: i64 = i64::try_convert(a.funcall("length", ())?)?;
                    alen == 0
                } else if let Some(h) = RHash::from_value(val) {
                    let hlen: i64 = i64::try_convert(h.funcall("length", ())?)?;
                    hlen == 0
                } else {
                    let vs: RString = val.funcall("to_s", ())?;
                    vs.to_string()?.is_empty()
                }
            }
            TargetKind::String(expected) => {
                if expected.is_empty() || expected == "empty" {
                    // In Jekyll, `empty` matches nil as well as empty strings/arrays/hashes
                    let is_emp = if val.is_nil() { true } else { is_empty_value(&ruby, val)? };
                    is_emp
                } else if expected == "blank" {
                    is_blank_value(&ruby, val)?
                } else {
                if let Some(rs) = RString::from_value(val) {
                    rs.to_string()? == *expected
                } else {
                    let array = arrayify_for_filter(&ruby, val)?;
                    let mut any_match = false;
                    for element in array.each() {
                        let element = element?;
                        if value_to_string(element)? == *expected {
                            any_match = true;
                            break;
                        }
                    }
                    any_match
                }
                }
            }
        };
        if matched {
            out.push(obj)?;
        }
    }
    Ok(out.into_value_with(&ruby))
}

#[derive(Debug, Clone)]
enum SortKey {
    Num(f64),
    Str(String),
}

fn parse_sort_key(ruby: &Ruby, val: Value) -> Result<Option<SortKey>, Error> {
    if val.is_nil() {
        return Ok(None);
    }

    let resolved = coerce_lookup_target(ruby, val)?;
    if resolved.is_nil() {
        return Ok(None);
    }

    if let Ok(n) = f64::try_convert(resolved) {
        return Ok(Some(SortKey::Num(n)));
    }

    if let Some(rs) = RString::from_value(resolved) {
        if let Ok(s0) = rs.to_string() {
            let trimmed = s0.trim();
            if INT_RE.is_match(trimmed) {
                if let Ok(i) = trimmed.parse::<f64>() {
                    return Ok(Some(SortKey::Num(i)));
                }
            }
            if FLOAT_RE.is_match(trimmed) {
                if let Ok(f) = trimmed.parse::<f64>() {
                    return Ok(Some(SortKey::Num(f)));
                }
            }
            return Ok(Some(SortKey::Str(s0)));
        }
    }

    let stringified = value_to_string(resolved)?;
    let trimmed = stringified.trim();
    if INT_RE.is_match(trimmed) {
        if let Ok(i) = trimmed.parse::<f64>() {
            return Ok(Some(SortKey::Num(i)));
        }
    }
    if FLOAT_RE.is_match(trimmed) {
        if let Ok(f) = trimmed.parse::<f64>() {
            return Ok(Some(SortKey::Num(f)));
        }
    }

    Ok(Some(SortKey::Str(stringified)))
}

fn sort_filter_fast(input: Value, property: Value, nils: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let prop = property.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let nils_s: String = nils.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let order = match nils_s.as_str() {
        "first" => -1,
        "last" => 1,
        _ => -1,
    };
    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    let mut items: Vec<(Option<SortKey>, Value)> = Vec::with_capacity(len as usize);
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        let v = fetch_nested_prop(&ruby, obj, &prop)?;
        let key = parse_sort_key(&ruby, v)?;
        items.push((key, obj));
    }
    items.sort_unstable_by(|a, b| match (&a.0, &b.0) {
        (Some(_), None) => {
            if order < 0 {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Less
            }
        }
        (None, Some(_)) => {
            if order < 0 {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Greater
            }
        }
        (None, None) => std::cmp::Ordering::Equal,
        (Some(ka), Some(kb)) => match (ka, kb) {
            (SortKey::Num(x), SortKey::Num(y)) => {
                x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
            }
            (SortKey::Str(sa), SortKey::Str(sb)) => sa.cmp(sb),
            (SortKey::Num(x), SortKey::Str(sb)) => x.to_string().cmp(sb),
            (SortKey::Str(sa), SortKey::Num(y)) => sa.cmp(&y.to_string()),
        },
    });
    let out = ruby.ary_new();
    for (_, obj) in items.into_iter() {
        out.push(obj).unwrap();
    }
    Ok(out.into_value_with(&ruby))
}

fn group_by_fast(input: Value, property: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let prop = property.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    use std::collections::HashMap;
    let mut order: Vec<String> = Vec::new();
    let mut map: HashMap<String, Vec<Value>> = HashMap::new();
    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        let v = fetch_nested_prop(&ruby, obj, &prop)?;
        let resolved = coerce_lookup_target(&ruby, v)?;
        let name = value_to_string(resolved)?;
        if !map.contains_key(&name) {
            order.push(name.clone());
            map.insert(name.clone(), Vec::new());
        }
        map.get_mut(&name).unwrap().push(obj);
    }
    let groups = ruby.ary_new();
    for name in order.into_iter() {
        let items = ruby.ary_new();
        if let Some(vec) = map.remove(&name) {
            for v in vec {
                items.push(v).unwrap();
            }
        }
        let h = ruby.hash_new();
        h.aset("name", ruby.str_new(&name))?;
        h.aset("items", items)?;
        let size = i64::try_convert(items.funcall("length", ())?)?;
        h.aset("size", size)?;
        groups.push(h)?;
    }
    Ok(groups.into_value_with(&ruby))
}

fn find_filter_fast(input: Value, property: Value, target: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let prop = property.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let target_is_nil = target.is_nil();
    let method_literal = if target_is_nil {
        false
    } else {
        let cls: Value = target.funcall("class", ()).unwrap();
        let name_rs: RString = cls.funcall("name", ()).unwrap();
        let name = name_rs.to_string().unwrap();
        name == "Liquid::Expression::MethodLiteral"
    };
    let target_str = if target_is_nil {
        String::new()
    } else {
        RString::try_convert(target.funcall("to_s", ())?)?.to_string()?
    };
    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        let val = fetch_nested_prop(&ruby, obj, &prop)?;
        if target_is_nil {
            if val.is_nil() {
                return Ok(obj);
            } else {
                continue;
            }
        }
        if val.is_nil() {
            continue;
        }
        if method_literal {
            let is_empty = if let Some(rs) = RString::from_value(val) {
                rs.to_string()?.is_empty()
            } else if let Some(a) = RArray::from_value(val) {
                let alen: i64 = i64::try_convert(a.funcall("length", ())?)?;
                alen == 0
            } else if let Some(h) = RHash::from_value(val) {
                let hlen: i64 = i64::try_convert(h.funcall("length", ())?)?;
                hlen == 0
            } else {
                let vs: RString = val.funcall("to_s", ())?;
                vs.to_string()?.is_empty()
            };
            if is_empty {
                return Ok(obj);
            }
        } else if let Some(rs) = RString::from_value(val) {
            if rs.to_string()? == target_str {
                return Ok(obj);
            }
        } else if let Some(a) = RArray::from_value(val) {
            let alen: i64 = i64::try_convert(a.funcall("length", ())?)?;
            for j in 0..alen {
                let av: Value = a.funcall("[]", (j,))?;
                let avs: RString = av.funcall("to_s", ())?;
                if avs.to_string()? == target_str {
                    return Ok(obj);
                }
            }
        } else {
            let vs: RString = val.funcall("to_s", ())?;
            if vs.to_string()? == target_str {
                return Ok(obj);
            }
        }
    }
    Ok(ruby.qnil().into_value_with(&ruby))
}

fn map_filter_fast(input: Value, property: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let prop = property.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let out = ruby.ary_new();
    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        let val = fetch_nested_prop(&ruby, obj, &prop)?;
        out.push(val)?;
    }
    Ok(out.into_value_with(&ruby))
}
