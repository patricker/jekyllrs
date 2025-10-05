use magnus::{function, prelude::*, Error, IntoValue, RModule, RString, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("hook_trigger_document", function!(hook_trigger_document, 3))?;
    bridge.define_singleton_method("hook_trigger_site", function!(hook_trigger_site, 3))?;
    Ok(())
}

/// Trigger document-related hooks via the centralized Ruby path while preserving
/// collection-specific owner and :documents owner semantics.
/// event may be a Symbol or String; arg is optional (may be nil).
fn hook_trigger_document(document: Value, event: Value, arg: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;

    let event_sym = if event.respond_to("to_sym", false)? {
        event.funcall::<_, _, Value>("to_sym", ())?
    } else {
        event
    };

    // Prefer collection-specific owner if object responds_to?(:collection) (Jekyll::Document)
    if document.respond_to("collection", false)? {
        let collection: Value = document.funcall("collection", ())?;
        if !collection.is_nil() {
            let label_value: Value = collection.funcall("label", ())?;
            let label_s: RString = label_value.funcall("to_s", ())?;
            let owner = ruby.to_symbol(&label_s.to_string()?);
            if arg.is_nil() {
                let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner, event_sym, document))?;
            } else {
                let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner, event_sym, document, arg))?;
            }
        }

        // And always trigger :documents owner for documents
        let owner = ruby.to_symbol("documents");
        if arg.is_nil() {
            let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner, event_sym, document))?;
        } else {
            let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner, event_sym, document, arg))?;
        }
        return Ok(ruby.qnil().into_value_with(&ruby));
    }

    // Otherwise, if the object exposes a hook_owner (e.g., Jekyll::Page via Convertible), use it.
    if document.respond_to("hook_owner", false)? {
        let owner_val: Value = document.funcall("hook_owner", ())?;
        if !owner_val.is_nil() {
            if arg.is_nil() {
                let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner_val, event_sym, document))?;
            } else {
                let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner_val, event_sym, document, arg))?;
            }
        }
        return Ok(ruby.qnil().into_value_with(&ruby));
    }

    // Fallback: no known owner; do nothing.
    
    Ok(ruby.qnil().into_value_with(&ruby))
}

/// Trigger site-level hooks via the centralized Ruby path.
/// Owner is always :site. Event may be Symbol or String. Payload is optional (nil allowed).
fn hook_trigger_site(site: Value, event: Value, payload: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;

    let event_sym = if event.respond_to("to_sym", false)? {
        event.funcall::<_, _, Value>("to_sym", ())?
    } else {
        event
    };

    let owner = ruby.to_symbol("site");
    if payload.is_nil() {
        let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner, event_sym, site))?;
    } else {
        let _ = rust.funcall::<_, _, Value>("hooks_trigger", (owner, event_sym, site, payload))?;
    }

    Ok(ruby.qnil().into_value_with(&ruby))
}
