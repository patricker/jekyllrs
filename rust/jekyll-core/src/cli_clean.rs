use magnus::{function, prelude::*, Error, RModule, Ruby, Value, IntoValue};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("engine_clean_process", function!(engine_clean_process, 1))?;
    Ok(())
}

fn rb_join(a: Value, b: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let file: Value = ruby.class_object().const_get("File")?;
    file.funcall("join", (a, b))
}

fn engine_clean_process(options: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let logger: Value = jekyll.funcall("logger", ())?;

    // Resolve configuration from options
    let command: Value = jekyll.const_get("Command")?;
    let config: Value = command.funcall("configuration_from_options", (options,))?;

    // Dest and metadata/cache paths
    let destination: Value = config.funcall("[]", (ruby.str_new("destination"),))?;
    let source: Value = config.funcall("[]", (ruby.str_new("source"),))?;
    let cache_dir_name: Value = config.funcall("[]", (ruby.str_new("cache_dir"),))?;

    let metadata_file = rb_join(source, ruby.str_new(".jekyll-metadata").into_value_with(&ruby))?;
    let cache_dir = rb_join(source, cache_dir_name.funcall::<_,_,Value>("to_s", ())?)?;

    // Remove helper mirrors Ruby Clean.remove
    fn remove(path: Value, checker: &str) -> Result<(), Error> {
        let ruby = ruby_handle()?;
        let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
        let logger: Value = jekyll.funcall("logger", ())?;
        let file: Value = ruby.class_object().const_get("File")?;
        let check: bool = file.funcall(checker, (path,))?;
        if check {
            let msg = format!("Removing {}...", path.to_r_string()?.to_string()?);
            let _: Value = logger.funcall("info", ("Cleaner:", msg.as_str()))?;
            let fu: Value = ruby.class_object().const_get("FileUtils")?;
            let _: Value = fu.funcall("rm_rf", (path,))?;
        } else {
            let msg = format!("Nothing to do for {}.", path.to_r_string()?.to_string()?);
            let _: Value = logger.funcall("info", ("Cleaner:", msg.as_str()))?;
        }
        Ok(())
    }

    remove(destination, "directory?")?;
    remove(metadata_file, "file?")?;
    remove(cache_dir, "directory?")?;
    // Sass cache relative to cwd
    let sass_cache: Value = ruby.str_new(".sass-cache").into_value_with(&ruby);
    remove(sass_cache, "directory?")?;

    Ok(())
}
