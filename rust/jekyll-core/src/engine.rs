use magnus::{function, prelude::*, Error, RModule, Ruby, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("engine_build_site", function!(engine_build_site, 1))?;
    Ok(())
}

fn engine_build_site(site: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;

    // If profile mode is enabled, delegate to site.profiler.profile_process
    let config: Value = site.funcall("config", ())?;
    let profile_key = ruby.str_new("profile");
    let profile_val: Value = config.funcall("[]", (profile_key,))?;
    let profile_enabled = !profile_val.is_nil() && profile_val.to_bool();

    if profile_enabled {
        let profiler: Value = site.funcall("profiler", ())?;
        let _ = profiler.funcall::<_, _, Value>("profile_process", ())?;
        return Ok(());
    }

    // Otherwise, orchestrate the standard phases
    let _ = site.funcall::<_, _, Value>("reset", ())?;
    let _ = site.funcall::<_, _, Value>("read", ())?;
    let _ = site.funcall::<_, _, Value>("generate", ())?;
    let _ = site.funcall::<_, _, Value>("render", ())?;
    let _ = site.funcall::<_, _, Value>("cleanup", ())?;
    let _ = site.funcall::<_, _, Value>("write", ())?;

    Ok(())
}
