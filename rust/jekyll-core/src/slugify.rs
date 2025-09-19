use magnus::{function, prelude::*, Error, RModule, RString, Value};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::ruby_utils::ruby_handle;

static SLUGIFY_RAW_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").expect("valid raw regex"));
static SLUGIFY_DEFAULT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^\p{M}\p{L}\p{Nd}]+").expect("valid default regex"));
static SLUGIFY_PRETTY_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^\p{M}\p{L}\p{Nd}._~!$&'()+,;=@]+").expect("valid pretty regex"));
static SLUGIFY_ASCII_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^A-Za-z0-9]+").expect("valid ascii regex"));

const SLUGIFY_MODES: &[&str] = &["raw", "default", "pretty", "ascii", "latin"];

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("slugify", function!(slugify, 3))?;
    Ok(())
}

fn slugify(
    input: Option<RString>,
    mode: Option<Value>,
    cased: bool,
) -> Result<Option<String>, Error> {
    let Some(input) = input else {
        return Ok(None);
    };

    let original = input.to_string()?;

    let mut mode_str = match mode {
        Some(value) if !value.is_nil() => format!("{}", value),
        _ => "default".to_owned(),
    };
    if mode_str.is_empty() {
        mode_str = "default".to_owned();
    }

    if !SLUGIFY_MODES.contains(&mode_str.as_str()) {
        let result = if cased {
            original
        } else {
            original.to_lowercase()
        };
        return Ok(Some(result));
    }

    let prepared = if mode_str == "latin" {
        let ruby = ruby_handle()?;
        let object = ruby.class_object();
        let i18n: Value = object.const_get("I18n")?;
        let config: Value = i18n.funcall("config", ())?;
        let locales: Value = config.funcall("available_locales", ())?;
        let is_empty: bool = locales.funcall("empty?", ())?;
        if is_empty {
            let _: Value = config.funcall("available_locales=", (ruby.to_symbol("en"),))?;
        }

        let transliterated: RString = i18n.funcall("transliterate", (input,))?;
        transliterated.to_string()?
    } else {
        original.clone()
    };

    let replaced = replace_character_sequence_with_hyphen(&prepared, &mode_str);
    let mut slug = replaced.trim_matches('-').to_string();

    if !cased {
        slug = slug.to_lowercase();
    }

    if slug.is_empty() {
        emit_empty_slug_warning(&original)?;
    }

    Ok(Some(slug))
}

fn replace_character_sequence_with_hyphen(input: &str, mode: &str) -> String {
    match mode {
        "raw" => SLUGIFY_RAW_RE.replace_all(input, "-").into_owned(),
        "pretty" => SLUGIFY_PRETTY_RE.replace_all(input, "-").into_owned(),
        "ascii" => SLUGIFY_ASCII_RE.replace_all(input, "-").into_owned(),
        _ => SLUGIFY_DEFAULT_RE.replace_all(input, "-").into_owned(),
    }
}

fn emit_empty_slug_warning(original: &str) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let jekyll: RModule = match ruby.class_object().const_get::<_, RModule>("Jekyll") {
        Ok(module) => module,
        Err(_) => ruby.define_module("Jekyll")?,
    };
    let logger: Value = jekyll.funcall("logger", ())?;
    let message = format!("Empty `slug` generated for '{}'.", original);
    let _: Value = logger.funcall("warn", ("Warning:", message))?;
    Ok(())
}
