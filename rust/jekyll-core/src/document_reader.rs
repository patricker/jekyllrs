use magnus::{function, prelude::*, Error, IntoValue, KwArgs, RHash, RModule, Ruby, Value};
use once_cell::sync::OnceCell;
use regex::Regex;

use crate::ruby_utils::ruby_handle;

static SAFE_YAML_REQUIRED: OnceCell<()> = OnceCell::new();
static FRONT_MATTER_RE: OnceCell<Regex> = OnceCell::new();

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("document_read", function!(document_read, 2))?;
    Ok(())
}

fn document_read(path: String, file_opts: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    ensure_safe_yaml(&ruby)?;

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

        let safe_yaml: Value = ruby.class_object().const_get("SafeYAML")?;
        let data: Value = safe_yaml.funcall("load", (ruby.str_new(front_matter),))?;

        result.aset(ruby.str_new("content"), ruby.str_new(body))?;
        result.aset(ruby.str_new("data"), data)?;
    } else {
        result.aset(ruby.str_new("content"), ruby.str_new(&content_string))?;
        result.aset(ruby.str_new("data"), ruby.qnil())?;
    }

    Ok(result.into_value_with(&ruby))
}

fn ensure_safe_yaml(ruby: &Ruby) -> Result<(), Error> {
    SAFE_YAML_REQUIRED.get_or_try_init(|| {
        ruby.eval::<Value>("require 'safe_yaml'")?;
        Ok::<(), Error>(())
    })?;
    Ok(())
}
