use magnus::{function, prelude::*, Error, IntoValue, RModule, Ruby, TryConvert, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("merged_file_read_opts", function!(merged_file_read_opts, 2))?;
    Ok(())
}

fn merged_file_read_opts(site: Option<Value>, opts: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    let site_opts = match site {
        Some(value) if !value.is_nil() => value.funcall::<_, _, Value>("file_read_opts", ())?,
        _ => ruby.hash_new().into_value_with(&ruby),
    };

    let merged = site_opts.funcall::<_, _, Value>("merge", (opts,))?;

    let symbol_key = ruby.to_symbol("encoding").into_value_with(&ruby);
    let string_key = ruby.str_new("encoding").into_value_with(&ruby);
    normalize_encoding(&ruby, merged.clone(), symbol_key)?;
    normalize_encoding(&ruby, merged.clone(), string_key)?;

    Ok(merged)
}

fn normalize_encoding(ruby: &Ruby, hash: Value, key: Value) -> Result<(), Error> {
    let value = hash.funcall::<_, _, Value>("[]", (key,))?;
    if value.is_nil() {
        return Ok(());
    }

    let encoding = String::try_convert(value)?;
    if !encoding.to_lowercase().starts_with("utf-") {
        return Ok(());
    }

    let new_value = format!("bom|{}", encoding);
    hash.funcall::<_, _, Value>("[]=", (key, ruby.str_new(&new_value)))?;
    Ok(())
}
