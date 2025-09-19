use magnus::{
    exception::ExceptionClass, function, prelude::*, Error, RModule, RString, Ruby, Value,
};
use once_cell::sync::OnceCell;

use crate::ruby_utils::ruby_handle;

static TIME_REQUIRED: OnceCell<()> = OnceCell::new();

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("parse_date", function!(parse_date, 2))?;
    Ok(())
}

fn parse_date(input: RString, message: Option<RString>) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    ensure_time_required(&ruby)?;

    let input_str = input.to_string()?;
    let message_str = match message {
        Some(msg) => msg.to_string()?,
        None => "Input could not be parsed.".to_string(),
    };

    let time_class = ruby.class_object().const_get::<_, Value>("Time")?;

    let parsed = match time_class.funcall::<_, _, Value>("parse", (input,)) {
        Ok(value) => value,
        Err(_) => return Err(invalid_date_error(&ruby, &input_str, &message_str)?),
    };

    match parsed.funcall::<_, _, Value>("localtime", ()) {
        Ok(value) => Ok(value),
        Err(_) => Err(invalid_date_error(&ruby, &input_str, &message_str)?),
    }
}

fn ensure_time_required(ruby: &Ruby) -> Result<(), Error> {
    TIME_REQUIRED.get_or_try_init(|| {
        ruby.eval::<Value>("require 'time'")?;
        Ok::<(), Error>(())
    })?;
    Ok(())
}

fn invalid_date_error(ruby: &Ruby, input: &str, message: &str) -> Result<Error, Error> {
    let jekyll: RModule = ruby.class_object().const_get::<_, RModule>("Jekyll")?;
    let errors: RModule = jekyll.const_get("Errors")?;
    let error_class: ExceptionClass = errors.const_get("InvalidDateError")?;

    Ok(Error::new(
        error_class,
        format!("Invalid date '{}': {}", input, message),
    ))
}
