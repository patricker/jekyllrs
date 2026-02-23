use magnus::{
    function, prelude::*, Error, ExceptionClass, IntoValue, KwArgs, RHash, RModule, Ruby, Value,
};
use once_cell::sync::OnceCell;
use regex::Regex;
use serde_yaml::{self, value::TaggedValue, Value as YamlValue};
use serde_json::{self, Value as JsonValue};

use crate::{
    ruby_utils::ruby_handle,
    time_utils::{self, TimeStringKind},
};

static FRONT_MATTER_RE: OnceCell<Regex> = OnceCell::new();
static DATE_REQUIRED: OnceCell<()> = OnceCell::new();

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("document_read", function!(document_read, 2))?;
    bridge.define_singleton_method("yaml_load_file", function!(yaml_load_file, 1))?;
    bridge.define_singleton_method("json_load_file", function!(json_load_file, 1))?;
    Ok(())
}

fn document_read(path: String, file_opts: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    let file_class: Value = ruby.class_object().const_get("File")?;
    let args = ruby.str_new(&path);

    let content_value: Value = if let Some(hash) = RHash::from_value(file_opts) {
        if hash.len() > 0 {
            // Ensure keyword-args use symbol keys
            let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
            let rust: RModule = jekyll.const_get("Rust")?;
            let sym_hash_val: Value = rust.funcall("symbolize_hash_keys", (hash,))?;
            let sym_hash = RHash::from_value(sym_hash_val).ok_or_else(|| {
                Error::new(ruby.exception_runtime_error(), "expected Hash from symbolize_hash_keys")
            })?;

            // If an encoding is specified, use mode: "rb:ENC" to ensure compatibility
            let enc_key = ruby.to_symbol("encoding").into_value_with(&ruby);
            let mode_key = ruby.to_symbol("mode").into_value_with(&ruby);
            let enc_value: Value = sym_hash.aref(enc_key)?;
            if !enc_value.is_nil() {
                let enc = String::try_convert(enc_value.clone())?;
                let mode_string = format!("rb:{}", enc);
                sym_hash.aset(mode_key, ruby.str_new(&mode_string))?;
                // Remove encoding to avoid conflicts
                let _: Value = sym_hash.delete(enc_key)?;
            }

            let kwargs = KwArgs(sym_hash);
            file_class.funcall("read", (args, kwargs))?
        } else {
            file_class.funcall("read", (args,))?
        }
    } else {
        file_class.funcall("read", (args,))?
    };
    let content_string: String = String::try_convert(content_value)?;

    let regex = FRONT_MATTER_RE.get_or_init(|| {
        Regex::new(r"(?ms)\A(---\s*\n.*?\n?)((?:---|\.\.\.)\s*$\n?)")
            .expect("valid front matter regex")
    });

    let result = ruby.hash_new();

    if let Some(captures) = regex.captures(&content_string) {
        let front_matter = captures.get(1).map(|m| m.as_str()).unwrap_or_default();
        let offset = captures.get(0).map(|m| m.end()).unwrap_or(0);
        let body = &content_string[offset..];

        let data = parse_front_matter(&ruby, front_matter)?;

        result.aset(ruby.str_new("content"), ruby.str_new(body))?;
        result.aset(ruby.str_new("data"), data)?;
    } else {
        result.aset(ruby.str_new("content"), ruby.str_new(&content_string))?;
        result.aset(ruby.str_new("data"), ruby.qnil())?;
    }

    Ok(result.into_value_with(&ruby))
}

fn parse_front_matter(ruby: &Ruby, source: &str) -> Result<Value, Error> {
    if source.trim().is_empty() {
        return Ok(ruby.qnil().into_value_with(ruby));
    }

    match serde_yaml::from_str::<YamlValue>(source) {
        Ok(value) => yaml_value_to_ruby(ruby, value),
        Err(err) => {
            let message = err.to_string();

            // serde_yaml rejects duplicate keys, but Ruby's Psych is lenient
            // (last value wins). Re-parse with yaml-rust2 which handles
            // duplicates natively — pure Rust, no Ruby fallback.
            if message.contains("duplicate entry") {
                return parse_front_matter_lenient(ruby, source);
            }

            let psych: RModule = ruby.class_object().const_get("Psych")?;
            let syntax_error: ExceptionClass = psych.const_get("SyntaxError")?;
            let exception = syntax_error.new_instance((
                ruby.qnil().into_value_with(ruby),
                0.into_value_with(ruby),
                0.into_value_with(ruby),
                0.into_value_with(ruby),
                ruby.str_new(&message).into_value_with(ruby),
                ruby.qnil().into_value_with(ruby),
            ))?;
            Err(Error::from(exception))
        }
    }
}

/// When serde_yaml rejects duplicate keys, deduplicate them in the source
/// string (keeping the last occurrence of each top-level key, matching Ruby's
/// Psych "last wins" behavior) then re-parse with serde_yaml.
fn parse_front_matter_lenient(ruby: &Ruby, source: &str) -> Result<Value, Error> {
    let deduped = dedup_yaml_keys(source);
    match serde_yaml::from_str::<YamlValue>(&deduped) {
        Ok(value) => yaml_value_to_ruby(ruby, value),
        Err(err) => {
            let psych: RModule = ruby.class_object().const_get("Psych")?;
            let syntax_error: ExceptionClass = psych.const_get("SyntaxError")?;
            let message = err.to_string();
            let exception = syntax_error.new_instance((
                ruby.qnil().into_value_with(ruby),
                0.into_value_with(ruby),
                0.into_value_with(ruby),
                0.into_value_with(ruby),
                ruby.str_new(&message).into_value_with(ruby),
                ruby.qnil().into_value_with(ruby),
            ))?;
            Err(Error::from(exception))
        }
    }
}

/// Remove duplicate top-level YAML keys, keeping the last occurrence of each.
/// Works on frontmatter which is typically flat key-value pairs.
/// A "top-level key" line starts with a non-whitespace char and contains `:`.
/// Continuation lines (indented or non-key) are grouped with the preceding key.
fn dedup_yaml_keys(source: &str) -> String {
    use std::collections::HashMap;

    // Parse into (key, block_of_lines) pairs
    let mut blocks: Vec<(Option<String>, String)> = Vec::new();
    let mut current_key: Option<String> = None;
    let mut current_block = String::new();

    for line in source.lines() {
        let trimmed = line.trim_start();
        // A top-level key line: starts at column 0 (no leading whitespace) and has a colon
        let is_top_key = !line.is_empty()
            && !line.starts_with(' ')
            && !line.starts_with('\t')
            && !trimmed.starts_with('#')
            && !trimmed.starts_with('-')
            && trimmed.contains(':');

        if is_top_key {
            // Save previous block
            if !current_block.is_empty() {
                blocks.push((current_key.take(), current_block.clone()));
                current_block.clear();
            }
            // Extract key name (everything before the first colon)
            let key = trimmed.split(':').next().unwrap_or("").trim().to_string();
            current_key = Some(key);
        }

        current_block.push_str(line);
        current_block.push('\n');
    }
    // Push final block
    if !current_block.is_empty() {
        blocks.push((current_key, current_block));
    }

    // Find which keys are duplicated and keep only the last occurrence
    let mut key_last_index: HashMap<String, usize> = HashMap::new();
    for (i, (key, _)) in blocks.iter().enumerate() {
        if let Some(k) = key {
            key_last_index.insert(k.clone(), i);
        }
    }

    let mut result = String::new();
    let mut seen_keys: HashMap<String, usize> = HashMap::new();
    for (i, (key, block)) in blocks.iter().enumerate() {
        if let Some(k) = key {
            let count = seen_keys.entry(k.clone()).or_insert(0);
            *count += 1;
            // Skip if this is not the last occurrence
            if let Some(&last_idx) = key_last_index.get(k) {
                if i != last_idx {
                    continue;
                }
            }
        }
        result.push_str(block);
    }

    result
}


fn yaml_load_file(path: String) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    // Read entire file as UTF-8 (Ruby default behavior for SafeYAML.load_file is IO read in Ruby)
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return Err(Error::new(
                ruby.exception_runtime_error(),
                format!("failed to read YAML file {}: {}", path, e),
            ))
        }
    };

    match serde_yaml::from_str::<YamlValue>(&content) {
        Ok(value) => yaml_value_to_ruby(&ruby, value),
        Err(err) => {
            // Mirror parse_front_matter error mapping to Psych::SyntaxError
            let psych: RModule = ruby.class_object().const_get("Psych")?;
            let syntax_error: ExceptionClass = psych.const_get("SyntaxError")?;
            let message = format!("{} in {}", err.to_string(), path);
            let exception = syntax_error.new_instance((
                ruby.qnil().into_value_with(&ruby),
                0.into_value_with(&ruby),
                0.into_value_with(&ruby),
                0.into_value_with(&ruby),
                ruby.str_new(&message).into_value_with(&ruby),
                ruby.qnil().into_value_with(&ruby),
            ))?;
            Err(Error::from(exception))
        }
    }
}

fn json_load_file(path: String) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            return Err(Error::new(
                ruby.exception_runtime_error(),
                format!("failed to read JSON file {}: {}", path, e),
            ))
        }
    };

    match serde_json::from_str::<JsonValue>(&content) {
        Ok(value) => json_value_to_ruby(&ruby, value),
        Err(err) => Err(Error::new(
            ruby.exception_runtime_error(),
            format!("failed to parse JSON in {}: {}", path, err),
        )),
    }
}

fn yaml_value_to_ruby(ruby: &Ruby, value: YamlValue) -> Result<Value, Error> {
    match value {
        YamlValue::Null => Ok(ruby.qnil().into_value_with(ruby)),
        YamlValue::Bool(b) => Ok(b.into_value_with(ruby)),
        YamlValue::Number(num) => {
            if let Some(i) = num.as_i64() {
                Ok(i.into_value_with(ruby))
            } else if let Some(u) = num.as_u64() {
                if u <= i64::MAX as u64 {
                    Ok((u as i64).into_value_with(ruby))
                } else {
                    Ok((u as f64).into_value_with(ruby))
                }
            } else if let Some(f) = num.as_f64() {
                Ok(f.into_value_with(ruby))
            } else {
                Ok(ruby.qnil().into_value_with(ruby))
            }
        }
        YamlValue::String(s) => string_scalar_to_ruby(ruby, &s),
        YamlValue::Sequence(seq) => {
            let array = ruby.ary_new();
            for item in seq {
                let converted = yaml_value_to_ruby(ruby, item)?;
                array.push(converted)?;
            }
            Ok(array.into_value_with(ruby))
        }
        YamlValue::Mapping(map) => {
            let hash: RHash = ruby.hash_new();
            for (key, value) in map {
                let key_value = yaml_value_to_ruby(ruby, key)?;
                let value_value = yaml_value_to_ruby(ruby, value)?;
                hash.aset(key_value, value_value)?;
            }
            Ok(hash.into_value_with(ruby))
        }
        YamlValue::Tagged(boxed) => yaml_tagged_value_to_ruby(ruby, *boxed),
    }
}

fn json_value_to_ruby(ruby: &Ruby, value: JsonValue) -> Result<Value, Error> {
    match value {
        JsonValue::Null => Ok(ruby.qnil().into_value_with(ruby)),
        JsonValue::Bool(b) => Ok(b.into_value_with(ruby)),
        JsonValue::Number(num) => {
            if let Some(i) = num.as_i64() {
                Ok(i.into_value_with(ruby))
            } else if let Some(u) = num.as_u64() {
                if u <= i64::MAX as u64 {
                    Ok((u as i64).into_value_with(ruby))
                } else {
                    Ok((u as f64).into_value_with(ruby))
                }
            } else if let Some(f) = num.as_f64() {
                Ok(f.into_value_with(ruby))
            } else {
                Ok(ruby.qnil().into_value_with(ruby))
            }
        }
        JsonValue::String(s) => Ok(ruby.str_new(&s).into_value_with(ruby)),
        JsonValue::Array(arr) => {
            let out = ruby.ary_new();
            for item in arr {
                out.push(json_value_to_ruby(ruby, item)?)?;
            }
            Ok(out.into_value_with(ruby))
        }
        JsonValue::Object(map) => {
            let h = ruby.hash_new();
            for (k, v) in map {
                let key = ruby.str_new(&k);
                let val = json_value_to_ruby(ruby, v)?;
                h.aset(key, val)?;
            }
            Ok(h.into_value_with(ruby))
        }
    }
}

fn yaml_tagged_value_to_ruby(ruby: &Ruby, tagged: TaggedValue) -> Result<Value, Error> {
    let tag_string = tagged.tag.to_string();
    let normalized_tag = tag_string.trim_start_matches('!');

    if normalized_tag == "tag:yaml.org,2002:timestamp" || normalized_tag == "timestamp" {
        return match tagged.value {
            YamlValue::String(s) => string_scalar_to_ruby(ruby, &s),
            other => yaml_value_to_ruby(ruby, other),
        };
    }

    yaml_value_to_ruby(ruby, tagged.value)
}

fn string_scalar_to_ruby(ruby: &Ruby, input: &str) -> Result<Value, Error> {
    if let Some(parsed) = maybe_parse_time_string(ruby, input)? {
        return Ok(parsed);
    }

    Ok(ruby.str_new(input).into_value_with(ruby))
}

fn maybe_parse_time_string(ruby: &Ruby, input: &str) -> Result<Option<Value>, Error> {
    let candidate = input.trim();
    match time_utils::classify_time_string(candidate) {
        Some(TimeStringKind::DateTime) => time_utils::try_time_parse(ruby, candidate),
        Some(TimeStringKind::DateOnly) => {
            ensure_date_required(ruby)?;
            let date_class: Value = ruby.class_object().const_get("Date")?;

            match date_class.funcall::<_, _, Value>("parse", (ruby.str_new(candidate),)) {
                Ok(value) => Ok(Some(value)),
                Err(_) => {
                    let composed = format!("{} 00:00:00", candidate);
                    time_utils::try_time_parse(ruby, &composed)
                }
            }
        }
        None => Ok(None),
    }
}

fn ensure_date_required(ruby: &Ruby) -> Result<(), Error> {
    DATE_REQUIRED.get_or_try_init(|| {
        ruby.eval::<Value>("require 'date'")?;
        Ok::<(), Error>(())
    })?;
    Ok(())
}
