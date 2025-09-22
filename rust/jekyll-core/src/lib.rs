mod cleaner;
mod cli_build;
mod cli_clean;
mod dates;
mod document;
mod document_reader;
mod engine;
mod entry_filter;
mod file_opts;
mod frontmatter;
mod liquid;
mod merge;
mod path_manager;
mod reader;
mod regenerator_io;
mod ruby_utils;
mod slugify;
mod static_file;
mod theme_reader;
mod time_utils;
mod url;
mod utils;
mod yaml_header;

use magnus::{prelude::*, Error, RModule, Ruby};

#[magnus::init]
fn init(ruby: &Ruby) -> Result<(), Error> {
    let jekyll: RModule = match ruby.class_object().const_get::<_, RModule>("Jekyll") {
        Ok(module) => module,
        Err(_) => ruby.define_module("Jekyll")?,
    };

    let rust_module: RModule = match jekyll.const_get::<_, RModule>("Rust") {
        Ok(module) => module,
        Err(_) => jekyll.define_module("Rust")?,
    };

    let bridge: RModule = match rust_module.const_get::<_, RModule>("Bridge") {
        Ok(module) => module,
        Err(_) => rust_module.define_module("Bridge")?,
    };

    slugify::define_into(&bridge)?;
    path_manager::define_into(&bridge)?;
    utils::define_into(&bridge)?;
    liquid::define_into(&bridge)?;
    merge::define_into(&bridge)?;
    file_opts::define_into(&bridge)?;
    dates::define_into(&bridge)?;
    yaml_header::define_into(&bridge)?;
    url::define_into(&bridge)?;
    entry_filter::define_into(&bridge)?;
    document::define_into(&bridge)?;
    document_reader::define_into(&bridge)?;
    time_utils::define_into(&bridge)?;
    static_file::define_into(&bridge)?;
    frontmatter::define_into(&bridge)?;
    cleaner::define_into(&bridge)?;
    theme_reader::define_into(&bridge)?;
    regenerator_io::define_into(&bridge)?;
    engine::define_into(&bridge)?;
    cli_build::define_into(&bridge)?;
    cli_clean::define_into(&bridge)?;
    reader::define_into(&bridge)?;

    Ok(())
}

extern "C" {
    #[link_name = "Init_jekyll_core"]
    fn init_jekyll_core_shim();
}

#[no_mangle]
pub extern "C" fn Init_libjekyll_core() {
    unsafe { init_jekyll_core_shim() }
}
