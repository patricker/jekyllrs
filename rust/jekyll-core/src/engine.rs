use magnus::{function, prelude::*, Error, RModule, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("engine_build_site", function!(engine_build_site, 1))?;
    bridge.define_singleton_method("engine_generate", function!(engine_generate, 1))?;
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

fn engine_generate(site: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let logger: Value = jekyll.funcall("logger", ())?;
    let generators: Value = site.funcall("generators", ())?;
    if let Some(arr) = magnus::RArray::from_value(generators) {
        for item in arr.each() {
            let gen = item?;
            // Time the generator
            let start = std::time::Instant::now();
            let _: Value = gen.funcall("generate", (site,))?;
            let elapsed = start.elapsed().as_secs_f64();
            let klass: Value = gen.funcall("class", ())?;
            let klass_s: Value = klass.funcall("to_s", ())?;
            let msg = format!("{} finished in {} seconds.", String::try_convert(klass_s)?, elapsed);
            let _: Value = logger.funcall("debug", ("Generating:", msg))?;
        }
    }
    Ok(())
}
