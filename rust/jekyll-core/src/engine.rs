use magnus::{function, prelude::*, Error, IntoValue, RModule, RString, Ruby, Value};

use crate::ruby_utils::ruby_handle;

const PHASES: [&str; 6] = ["reset", "read", "generate", "render", "cleanup", "write"];

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("engine_build_site", function!(engine_build_site, 1))?;
    bridge.define_singleton_method("engine_generate", function!(engine_generate, 1))?;
    Ok(())
}

pub fn run_site_phases(
    site: Value,
    profile_enabled: bool,
) -> Result<Option<Vec<(String, f64)>>, Error> {
    if profile_enabled {
        let profiler: Value = site.funcall("profiler", ())?;
        let _ = profiler.funcall::<_, _, Value>("profile_process", ())?;
        return Ok(None);
    }

    let mut timings = Vec::with_capacity(PHASES.len());
    for phase in PHASES.iter() {
        let start = std::time::Instant::now();
        let _: Value = site.funcall(phase, ())?;
        let elapsed = start.elapsed().as_secs_f64();
        timings.push((phase.to_uppercase(), elapsed));
    }

    Ok(Some(timings))
}

pub fn emit_build_summary(site: Value, timings: &[(String, f64)]) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let logger: Value = jekyll.funcall("logger", ())?;
    let profiler: Value = jekyll.const_get("Profiler")?;

    let rows = ruby.ary_new_capa(timings.len() + 1);
    let header = ruby.ary_new_capa(2);
    header.push(ruby.str_new("PHASE"))?;
    header.push(ruby.str_new("TIME"))?;
    rows.push(header.into_value_with(&ruby))?;

    for (phase, secs) in timings {
        let row = ruby.ary_new_capa(2);
        row.push(ruby.str_new(phase.as_str()))?;
        row.push(ruby.str_new(&format!("{:.4}", secs)))?;
        rows.push(row.into_value_with(&ruby))?;
    }

    let summary: Value = profiler.funcall("tabulate", (rows,))?;
    let _: Value = logger.funcall("info", (ruby.str_new("\nBuild Process Summary:"),))?;
    let _: Value = logger.funcall("info", (summary,))?;
    let _: Value = logger.funcall("info", (ruby.str_new("\nSite Render Stats:"),))?;
    let _ = site.funcall::<_, _, Value>("print_stats", ())?;
    Ok(())
}

fn engine_build_site(site: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let config: Value = site.funcall("config", ())?;
    let profile_key = ruby.str_new("profile");
    let profile_val: Value = config.funcall("[]", (profile_key,))?;
    let profile_enabled = !profile_val.is_nil() && profile_val.to_bool();

    let timings = run_site_phases(site, profile_enabled)?;
    if let Some(ref timings) = timings {
        emit_build_summary(site, timings)?;
    }

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
            let msg = format!(
                "{} finished in {} seconds.",
                String::try_convert(klass_s)?,
                elapsed
            );
            let _: Value = logger.funcall("debug", ("Generating:", msg))?;
        }
    }
    Ok(())
}
