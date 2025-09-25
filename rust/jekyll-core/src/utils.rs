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

fn hash_lookup(ruby: &Ruby, h: Value, key: &str) -> Result<Value, Error> {
    let sym = ruby.to_symbol(key);
    let mut v: Value = h.funcall("[]", (sym,))?;
    if v.is_nil() {
        v = h.funcall("[]", (ruby.str_new(key),))?;
    }
    Ok(v)
}

fn fetch_nested_prop(ruby: &Ruby, obj: Value, prop: &str) -> Result<Value, Error> {
    let mut current = obj;
    let parts: Vec<&str> = prop.split('.').collect();
    if parts.is_empty() {
        return Ok(ruby.qnil().into_value_with(ruby));
    }
    let mut i = 0usize;
    loop {
        if let Some(_h) = RHash::from_value(current) {
            let v = hash_lookup(ruby, current, parts[i])?;
            if i + 1 == parts.len() {
                return Ok(v);
            } else {
                if RHash::from_value(v).is_some() {
                    current = v;
                    i += 1;
                    continue;
                } else {
                    // Intermediary is not a hash; treat as missing
                    return Ok(ruby.qnil().into_value_with(ruby));
                }
            }
        } else {
            return Ok(ruby.qnil().into_value_with(ruby));
        }
    }
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

    // Bail on unsupported operators/keywords
    if expr.contains(" contains ")
        || expr.contains(" and ")
        || expr.contains(" or ")
        || expr.contains('>')
        || expr.contains('<')
        || expr.contains(" site.")
    {
        return Ok(ruby.qnil().into_value_with(&ruby));
    }

    let (lhs, op, rhs) = if let Some(pos) = expr.find("==") {
        (expr[..pos].trim(), "==", expr[pos + 2..].trim())
    } else if let Some(pos) = expr.find("!=") {
        (expr[..pos].trim(), "!=", expr[pos + 2..].trim())
    } else {
        return Ok(ruby.qnil().into_value_with(&ruby));
    };

    let prefix = format!("{}.", var);
    if !lhs.starts_with(&prefix) {
        return Ok(ruby.qnil().into_value_with(&ruby));
    }
    let prop = &lhs[prefix.len()..];
    if prop.is_empty() {
        return Ok(ruby.qnil().into_value_with(&ruby));
    }

    enum RHS {
        Nil,
        Empty,
        Blank,
        Literal(String),
    }
    let rhs_kind = if rhs == "nil" || rhs == "null" {
        RHS::Nil
    } else if rhs == "empty" {
        RHS::Empty
    } else if rhs == "blank" {
        RHS::Blank
    } else if (rhs.starts_with('\'') && rhs.ends_with('\''))
        || (rhs.starts_with('"') && rhs.ends_with('"'))
    {
        RHS::Literal(rhs[1..rhs.len() - 1].to_string())
    } else {
        RHS::Literal(rhs.to_string())
    };

    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    let out = ruby.ary_new();
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        if RHash::from_value(obj).is_none() {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
        let v = fetch_nested_prop(&ruby, obj, prop)?;
        let matched = match &rhs_kind {
            RHS::Nil => {
                if op == "==" {
                    v.is_nil()
                } else {
                    !v.is_nil()
                }
            }
            RHS::Empty => {
                let is_emp = is_empty_value(&ruby, v)?;
                if op == "==" {
                    is_emp
                } else {
                    !is_emp
                }
            }
            RHS::Blank => {
                let is_blk = is_blank_value(&ruby, v)?;
                if op == "==" {
                    is_blk
                } else {
                    !is_blk
                }
            }
            RHS::Literal(ref s) => {
                if v.is_nil() || v.is_kind_of(ruby.class_array()) || v.is_kind_of(ruby.class_hash())
                {
                    false
                } else {
                    let vs: RString = v.funcall("to_s", ())?;
                    let vs_s = vs.to_string()?;
                    if op == "==" {
                        vs_s == *s
                    } else {
                        vs_s != *s
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

fn where_filter_fast2(input: Value, property: Value, target: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let arr = match RArray::from_value(input) {
        Some(a) => a,
        None => return Ok(ruby.qnil().into_value_with(&ruby)),
    };
    let prop_str = property.funcall::<_, _, RString>("to_s", ())?.to_string()?;
    let out = ruby.ary_new();
    let target_is_nil = target.is_nil();
    let target_s: RString = if target_is_nil {
        ruby.str_new("")
    } else {
        target.funcall("to_s", ())?
    };
    let target_str = target_s.to_string()?;
    let len: i64 = i64::try_convert(arr.funcall("length", ())?)?;
    for i in 0..len {
        let obj: Value = arr.funcall("[]", (i,))?;
        if RHash::from_value(obj).is_none() {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
        let val = fetch_nested_prop(&ruby, obj, &prop_str)?;
        if val.is_kind_of(ruby.class_array()) || val.is_kind_of(ruby.class_hash()) {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
        if target_is_nil {
            if val.is_nil() {
                out.push(obj)?;
            }
        } else if !val.is_nil() {
            let val_s: RString = val.funcall("to_s", ())?;
            if val_s.to_string()? == target_str {
                out.push(obj)?;
            }
        }
    }
    Ok(out.into_value_with(&ruby))
}

#[derive(Debug, Clone)]
enum SortKey {
    Num(f64),
    Str(String),
}

fn parse_sort_key(val: Value) -> Option<SortKey> {
    if let Ok(n) = f64::try_convert(val) {
        return Some(SortKey::Num(n));
    }
    if let Some(rs) = RString::from_value(val) {
        if let Ok(s0) = rs.to_string() {
            let st = s0.trim();
            static INT_RE: once_cell::sync::Lazy<regex::Regex> =
                once_cell::sync::Lazy::new(|| regex::Regex::new(r"^\s*-?\d+\s*$").unwrap());
            static FLOAT_RE: once_cell::sync::Lazy<regex::Regex> =
                once_cell::sync::Lazy::new(|| {
                    regex::Regex::new(r"^\s*-?(?:\d+\.?\d*|\.\d+)\s*$").unwrap()
                });
            if INT_RE.is_match(&s0) {
                if let Ok(i) = st.parse::<f64>() {
                    return Some(SortKey::Num(i));
                }
            }
            if FLOAT_RE.is_match(&s0) {
                if let Ok(f) = st.parse::<f64>() {
                    return Some(SortKey::Num(f));
                }
            }
            return Some(SortKey::Str(s0));
        }
    }
    None
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
        if RHash::from_value(obj).is_none() {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
        let v = fetch_nested_prop(&ruby, obj, &prop)?;
        let key = if v.is_nil() { None } else { parse_sort_key(v) };
        if !v.is_nil() && key.is_none() {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
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
        if RHash::from_value(obj).is_none() {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
        let v = fetch_nested_prop(&ruby, obj, &prop)?;
        let name_rs: RString = v.funcall("to_s", ())?;
        let name = name_rs.to_string()?;
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
        let h = RHash::new();
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
        if RHash::from_value(obj).is_none() {
            return Ok(ruby.qnil().into_value_with(&ruby));
        }
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
