use magnus::{function, prelude::*, Error, IntoValue, RArray, RModule, RString, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("theme_assets_list", function!(theme_assets_list, 1))?;
    Ok(())
}

fn theme_assets_list(root: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let root_str = root.to_string()?;
    let dir_module: Value = ruby.class_object().const_get("Dir")?;
    let file_class: Value = ruby.class_object().const_get("File")?;

    let pattern = format!("{}/**/*", root_str);

    let fnm_dotmatch: Value = file_class.funcall("const_get", (ruby.str_new("FNM_DOTMATCH"),))?;
    let glob_value: Value = dir_module.funcall("glob", (ruby.str_new(&pattern), fnm_dotmatch))?;
    let entries = RArray::from_value(glob_value).ok_or_else(|| {
        Error::new(
            ruby.exception_type_error(),
            "Dir.glob did not return an array",
        )
    })?;

    let array = ruby.ary_new();
    for entry in entries.each() {
        let entry = entry?;
        // Filter directories
        let is_dir: bool = file_class.funcall("directory?", (entry,))?;
        if is_dir {
            continue;
        }
        array.push(entry)?;
    }

    Ok(array.into_value_with(&ruby))
}
