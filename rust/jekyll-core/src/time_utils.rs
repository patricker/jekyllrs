use magnus::{exception::ExceptionClass, function, prelude::*, Error, RModule, Ruby, Value};
use once_cell::sync::{Lazy, OnceCell};
use regex::Regex;

use crate::ruby_utils::ruby_handle;

static DATE_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").expect("valid date regex"));
static DATETIME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"^\d{4}-\d{2}-\d{2}(?:[ Tt]\d{2}:\d{2}:\d{2}(?:\.\d+)?)?(?:[ \t][A-Za-z0-9/_+:-]+)?$",
    )
    .expect("valid datetime regex")
});

static TIME_REQUIRED: OnceCell<()> = OnceCell::new();

#[derive(Debug)]
pub(crate) enum TimeStringKind {
    DateOnly,
    DateTime,
}

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("parse_time", function!(parse_time, 2))?;
    Ok(())
}

fn parse_time(input: Value, message: Option<Value>) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    ensure_time_required(&ruby)?;

    if input.is_nil() {
        return Err(invalid_date_error(&ruby, "nil", message)?);
    }

    if input.respond_to("to_time", false)? {
        match input.funcall::<_, _, Value>("to_time", ()) {
            Ok(time) => return Ok(time),
            Err(err) => return Err(err),
        }
    }

    let input_string: String = match input.respond_to("to_s", false)? {
        true => {
            let value = input.funcall::<_, _, Value>("to_s", ())?;
            String::try_convert(value)?
        }
        false => {
            return Err(invalid_date_error(
                &ruby,
                &format!("{}", input.inspect()),
                message,
            )?)
        }
    };

    if let Some(parsed) = try_time_parse(&ruby, &input_string)? {
        return Ok(parsed);
    }

    if DATE_RE.is_match(&input_string) {
        let composed = format!("{} 00:00:00", input_string);
        if let Some(parsed) = try_time_parse(&ruby, &composed)? {
            return Ok(parsed);
        }
    }

    if DATETIME_RE.is_match(&input_string) {
        if let Some(parsed) = try_time_parse(&ruby, &input_string)? {
            return Ok(parsed);
        }
    }

    Err(invalid_date_error(&ruby, &input_string, message)?)
}

pub(crate) fn try_time_parse(ruby: &Ruby, input: &str) -> Result<Option<Value>, Error> {
    ensure_time_required(ruby)?;

    let time_class: Value = ruby.class_object().const_get("Time")?;

    match time_class.funcall::<_, _, Value>("parse", (ruby.str_new(input),)) {
        Ok(parsed) => match parsed.funcall::<_, _, Value>("localtime", ()) {
            Ok(local) => Ok(Some(local)),
            Err(_) => Ok(None),
        },
        Err(_) => Ok(None),
    }
}

pub(crate) fn ensure_time_required(ruby: &Ruby) -> Result<(), Error> {
    TIME_REQUIRED.get_or_try_init(|| {
        ruby.eval::<Value>("require 'time'")?;
        Ok::<(), Error>(())
    })?;
    Ok(())
}

fn invalid_date_error(ruby: &Ruby, input: &str, message: Option<Value>) -> Result<Error, Error> {
    let jekyll: RModule = ruby.class_object().const_get::<_, RModule>("Jekyll")?;
    let errors: RModule = jekyll.const_get("Errors")?;
    let error_class: ExceptionClass = errors.const_get("InvalidDateError")?;

    let detail = match message {
        Some(msg) if !msg.is_nil() => {
            let text: String = String::try_convert(msg)?;
            text
        }
        _ => "Input could not be parsed.".to_string(),
    };

    Ok(Error::new(
        error_class,
        format!("Invalid date '{}': {}", input, detail),
    ))
}

pub(crate) fn classify_time_string(input: &str) -> Option<TimeStringKind> {
    if input.is_empty() {
        return None;
    }

    if DATE_RE.is_match(input) {
        Some(TimeStringKind::DateOnly)
    } else if DATETIME_RE.is_match(input) {
        Some(TimeStringKind::DateTime)
    } else {
        None
    }
}
