use magnus::r_hash::ForEach;
use magnus::{function, prelude::*, Error, RHash, RModule, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("deep_merge_hashes", function!(deep_merge_hashes, 2))?;
    bridge.define_singleton_method(
        "deep_merge_hashes_bang",
        function!(deep_merge_hashes_bang, 2),
    )?;
    Ok(())
}

fn deep_merge_hashes(master: Value, other: Value) -> Result<Value, Error> {
    let dup = master.funcall::<_, _, Value>("dup", ())?;
    deep_merge_hashes_bang_internal(dup, other)?;
    Ok(dup)
}

fn deep_merge_hashes_bang(target: Value, other: Value) -> Result<Value, Error> {
    deep_merge_hashes_bang_internal(target, other)?;
    Ok(target)
}

fn deep_merge_hashes_bang_internal(target: Value, other: Value) -> Result<(), Error> {
    merge_values(target, other)?;
    merge_default_proc(target, other)?;
    duplicate_frozen_values(target)?;
    Ok(())
}

fn merge_values(target: Value, other: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let block = ruby.proc_from_fn(move |args: &[Value], _| -> Result<Value, Error> {
        let ruby = ruby_handle()?;
        let nil = ruby.qnil().as_value();
        let old_val = args.get(1).copied().unwrap_or(nil);
        let new_val = args.get(2).copied().unwrap_or(nil);

        if new_val.is_nil() {
            return Ok(old_val);
        }

        if is_mergable(old_val)? && is_mergable(new_val)? {
            return deep_merge_hashes(old_val, new_val);
        }

        Ok(new_val)
    });

    target.funcall_with_block::<_, _, Value>("merge!", (other,), block)?;
    Ok(())
}

fn merge_default_proc(target: Value, other: Value) -> Result<(), Error> {
    if RHash::from_value(target).is_none() || RHash::from_value(other).is_none() {
        return Ok(());
    }

    let target_default: Value = target.funcall("default_proc", ())?;
    if !target_default.is_nil() {
        return Ok(());
    }

    let overwrite_default: Value = other.funcall("default_proc", ())?;
    if overwrite_default.is_nil() {
        return Ok(());
    }

    target.funcall::<_, _, Value>("default_proc=", (overwrite_default,))?;
    Ok(())
}

fn duplicate_frozen_values(target: Value) -> Result<(), Error> {
    let hash = match RHash::from_value(target) {
        Some(hash) => hash,
        None => return Ok(()),
    };

    hash.foreach(|key: Value, value: Value| {
        let frozen: bool = value.funcall("frozen?", ())?;
        if frozen && is_duplicable(value)? {
            let duplicated = value.funcall::<_, _, Value>("dup", ())?;
            target.funcall::<_, _, Value>("[]=", (key, duplicated))?;
        }
        Ok(ForEach::Continue)
    })?;

    Ok(())
}

fn is_mergable(value: Value) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    Ok(value.is_kind_of(ruby.class_hash()))
}

fn is_duplicable(value: Value) -> Result<bool, Error> {
    if value.is_nil() {
        return Ok(false);
    }

    let ruby = ruby_handle()?;
    if !value.to_bool() {
        return Ok(false);
    }

    if value.is_kind_of(ruby.class_symbol()) {
        return Ok(false);
    }

    if value.is_kind_of(ruby.class_numeric()) {
        return Ok(false);
    }

    Ok(true)
}
