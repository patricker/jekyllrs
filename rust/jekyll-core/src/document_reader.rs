use magnus::{
    function, prelude::*, Error, ExceptionClass, IntoValue, KwArgs, RHash, RModule, Ruby, Value,
};
use once_cell::sync::OnceCell;
use regex::Regex;
use serde_yaml::{self, value::TaggedValue, Value as YamlValue};

use crate::{
    ruby_utils::ruby_handle,
    time_utils::{self, TimeStringKind},
};

static FRONT_MATTER_RE: OnceCell<Regex> = OnceCell::new();
static DATE_REQUIRED: OnceCell<()> = OnceCell::new();

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("document_read", function!(document_read, 2))?;
    Ok(())
}

fn document_read(path: String, file_opts: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    let file_class: Value = ruby.class_object().const_get("File")?;
    let args = ruby.str_new(&path);

    let content_value: Value = if let Some(hash) = RHash::from_value(file_opts) {
        if hash.len() > 0 {
            let kwargs = KwArgs(hash);
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
            let psych: RModule = ruby.class_object().const_get("Psych")?;
            let syntax_error: ExceptionClass = psych.const_get("SyntaxError")?;
            Err(Error::new(syntax_error, err.to_string()))
        }
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
