use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use magnus::r_hash::ForEach;
use magnus::{
    function, prelude::*, Error, ExceptionClass, IntoValue, RArray, RClass, RHash, RModule, RString,
    Ruby, Value,
};

use once_cell::sync::Lazy;

use crate::ruby_utils::ruby_handle;

trait RustConverter: Sync {
    #[allow(dead_code)]
    fn name(&self) -> &'static str;
    fn priority(&self) -> i32;
    fn matches(
        &self,
        ctx: &RenderingContext,
        site: Value,
        ext: &str,
    ) -> Result<bool, Error>;
    fn convert(
        &self,
        ctx: &RenderingContext,
        site: Value,
        document: Value,
        content: Value,
    ) -> Result<Value, Error>;
    fn output_ext(
        &self,
        ctx: &RenderingContext,
        site: Value,
        original_ext: Value,
    ) -> Result<Option<Value>, Error>;
    fn highlighter_options(
        &self,
        _ctx: &RenderingContext,
        _site: Value,
    ) -> Result<Option<(Value, Value)>, Error> {
        Ok(None)
    }
}

#[derive(Clone, Copy)]
enum ConverterKind {
    Ruby(Value),
    Rust(&'static dyn RustConverter),
}

#[derive(Clone, Copy)]
struct ConverterEntry {
    priority: i32,
    order: usize,
    kind: ConverterKind,
}

struct IdentityConverter;

impl RustConverter for IdentityConverter {
    fn name(&self) -> &'static str {
        "Identity"
    }

    fn priority(&self) -> i32 {
        -100
    }

    fn matches(
        &self,
        _ctx: &RenderingContext,
        _site: Value,
        _ext: &str,
    ) -> Result<bool, Error> {
        Ok(true)
    }

    fn convert(
        &self,
        _ctx: &RenderingContext,
        _site: Value,
        _document: Value,
        content: Value,
    ) -> Result<Value, Error> {
        Ok(content)
    }

    fn output_ext(
        &self,
        _ctx: &RenderingContext,
        _site: Value,
        original_ext: Value,
    ) -> Result<Option<Value>, Error> {
        if original_ext.is_nil() {
            Ok(None)
        } else {
            Ok(Some(original_ext))
        }
    }
}

static IDENTITY_CONVERTER: IdentityConverter = IdentityConverter;
static RUST_CONVERTERS: Lazy<Mutex<Vec<&'static dyn RustConverter>>> = Lazy::new(|| {
    Mutex::new(vec![&IDENTITY_CONVERTER as &dyn RustConverter])
});

static KRAMDOWN_CONVERTER: KramdownConverter = KramdownConverter;
static RUST_MD_SHIM_CONVERTER: RustMdShimConverter = RustMdShimConverter;

fn rust_converters() -> Vec<&'static dyn RustConverter> {
    let guard = RUST_CONVERTERS
        .lock()
        .expect("rust converter registry poisoned");
    guard.clone()
}

#[allow(dead_code)]
fn register_rust_converter(converter: &'static dyn RustConverter) {
    let mut guard = RUST_CONVERTERS
        .lock()
        .expect("rust converter registry poisoned");
    guard.push(converter);
}

struct KramdownConverter;

impl KramdownConverter {
    fn default_extensions() -> &'static [&'static str] {
        &[".markdown", ".mkdown", ".mkd", ".md"]
    }

    fn extensions_from_config(
        &self,
        ctx: &RenderingContext,
        config: Value,
    ) -> Result<Vec<String>, Error> {
        let markdown_ext_key = ctx.str("markdown_ext");
        let ext_value: Value = config.funcall("[]", (markdown_ext_key,))?;
        if ext_value.is_nil() {
            return Ok(Self::default_extensions()
                .iter()
                .map(|ext| ext.to_string())
                .collect());
        }

        let ext_string = String::try_convert(ext_value)?;
        let extensions = ext_string
            .split(',')
            .map(|part| {
                let trimmed = part.trim();
                if trimmed.starts_with('.') {
                    trimmed.to_ascii_lowercase()
                } else {
                    format!(".{}", trimmed.to_ascii_lowercase())
                }
            })
            .collect();
        Ok(extensions)
    }

    fn is_kramdown_enabled(
        &self,
        ctx: &RenderingContext,
        config: Value,
    ) -> Result<bool, Error> {
        let markdown_key = ctx.str("markdown");
        let markdown_engine: Value = config.funcall("[]", (markdown_key,))?;
        if markdown_engine.is_nil() {
            return Ok(true);
        }
        let engine = String::try_convert(markdown_engine)?;
        Ok(engine.eq_ignore_ascii_case("kramdown"))
    }

    fn parser_class(&self, ctx: &RenderingContext) -> Option<RClass> {
        ctx.markdown_parser_class
    }
}

impl RustConverter for KramdownConverter {
    fn name(&self) -> &'static str {
        "Kramdown"
    }

    fn priority(&self) -> i32 {
        5
    }

    fn matches(
        &self,
        ctx: &RenderingContext,
        site: Value,
        ext: &str,
    ) -> Result<bool, Error> {
        let config: Value = site.funcall("config", ())?;
        if !self.is_kramdown_enabled(ctx, config)? {
            return Ok(false);
        }

        let extensions = self.extensions_from_config(ctx, config)?;
        Ok(extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(ext)))
    }

    fn convert(
        &self,
        ctx: &RenderingContext,
        site: Value,
        _document: Value,
        content: Value,
    ) -> Result<Value, Error> {
        let parser_class = match self.parser_class(ctx) {
            Some(class) => class,
            None => {
                return Err(Error::new(
                    ctx.ruby.exception_runtime_error(),
                    "Kramdown parser class not available",
                ))
            }
        };

        let config: Value = site.funcall("config", ())?;
        let config_dup: Value = config.funcall("dup", ())?;

        let parser_instance: Value = parser_class.funcall("new", (config_dup,))?;
        parser_instance.funcall("convert", (content,))
    }

    fn output_ext(
        &self,
        ctx: &RenderingContext,
        _site: Value,
        _original_ext: Value,
    ) -> Result<Option<Value>, Error> {
        Ok(Some(ctx.str(".html")))
    }

    fn highlighter_options(
        &self,
        ctx: &RenderingContext,
        _site: Value,
    ) -> Result<Option<(Value, Value)>, Error> {
        Ok(Some((ctx.str("\n"), ctx.str("\n"))))
    }
}

// A shim that allows using Ruby's Kramdown parser when `markdown: rust` is set.
// This scaffolds a Rust-native Markdown converter slot without changing behavior yet.
struct RustMdShimConverter;

impl RustMdShimConverter {
    fn extensions_from_config(
        &self,
        ctx: &RenderingContext,
        config: Value,
    ) -> Result<Vec<String>, Error> {
        KRAMDOWN_CONVERTER.extensions_from_config(ctx, config)
    }

    fn is_rust_markdown_enabled(&self, ctx: &RenderingContext, config: Value) -> Result<bool, Error> {
        let markdown_key = ctx.str("markdown");
        let markdown_engine: Value = config.funcall("[]", (markdown_key,))?;
        if markdown_engine.is_nil() { return Ok(false); }
        let engine = String::try_convert(markdown_engine)?;
        Ok(engine.eq_ignore_ascii_case("rust"))
    }
}

impl RustConverter for RustMdShimConverter {
    fn name(&self) -> &'static str { "RustMarkdownShim" }
    fn priority(&self) -> i32 { 5 }
    fn matches(&self, ctx: &RenderingContext, site: Value, ext: &str) -> Result<bool, Error> {
        let config: Value = site.funcall("config", ())?;
        if !self.is_rust_markdown_enabled(ctx, config)? { return Ok(false); }
        let extensions = self.extensions_from_config(ctx, config)?;
        Ok(extensions.iter().any(|candidate| candidate.eq_ignore_ascii_case(ext)))
    }
    fn convert(&self, ctx: &RenderingContext, site: Value, _document: Value, content: Value) -> Result<Value, Error> {
        let parser_class = match KRAMDOWN_CONVERTER.parser_class(ctx) {
            Some(class) => class,
            None => {
                return Err(Error::new(
                    ctx.ruby.exception_runtime_error(),
                    "Kramdown parser class not available",
                ))
            }
        };
        let config: Value = site.funcall("config", ())?;
        let config_dup: Value = config.funcall("dup", ())?;
        let parser_instance: Value = parser_class.funcall("new", (config_dup,))?;
        parser_instance.funcall("convert", (content,))
    }
    fn output_ext(&self, ctx: &RenderingContext, _site: Value, _original_ext: Value) -> Result<Option<Value>, Error> {
        Ok(Some(ctx.str(".html")))
    }
    fn highlighter_options(&self, ctx: &RenderingContext, _site: Value) -> Result<Option<(Value, Value)>, Error> {
        Ok(Some((ctx.str("\n"), ctx.str("\n"))))
    }
}

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    register_rust_converter(&RUST_MD_SHIM_CONVERTER);
    register_rust_converter(&KRAMDOWN_CONVERTER);

    bridge.define_singleton_method("render_site", function!(render_site, 1))?;
    bridge.define_singleton_method("renderer_run", function!(renderer_run, 4))?;
    bridge.define_singleton_method("renderer_convert", function!(renderer_convert, 3))?;
    bridge.define_singleton_method("renderer_output_ext", function!(renderer_output_ext, 2))?;
    bridge.define_singleton_method(
        "renderer_render_liquid",
        function!(renderer_render_liquid, 6),
    )?;
    bridge.define_singleton_method(
        "renderer_place_in_layouts",
        function!(renderer_place_in_layouts, 6),
    )?;
    bridge.define_singleton_method("renderer_converters", function!(renderer_converters, 2))?;
    Ok(())
}

struct RenderingContext<'ruby> {
    ruby: &'ruby Ruby,
    logger: Value,
    liquid_renderer_class: Value,
    utils: Value,
    excerpt_class: RClass,
    plugin_priorities: Value,
    identity_converter_class: RClass,
    markdown_converter_class: Option<RClass>,
    markdown_parser_class: Option<RClass>,
}

impl<'ruby> RenderingContext<'ruby> {
    fn new(ruby: &'ruby Ruby) -> Result<Self, Error> {
        let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
        let logger: Value = jekyll.funcall::<_, _, Value>("logger", ())?;
        let liquid_renderer_class: Value = jekyll.const_get("LiquidRenderer")?;
        let utils: Value = jekyll.const_get("Utils")?;
        let excerpt_class: RClass = jekyll.const_get("Excerpt")?;
        let plugin_class: RClass = jekyll.const_get("Plugin")?;
        let plugin_priorities: Value = plugin_class.const_get("PRIORITIES")?;
        let converters_module: RModule = jekyll.const_get("Converters")?;
        let identity_converter_class: RClass = converters_module.const_get("Identity")?;
        let markdown_converter_class = converters_module.const_get::<_, RClass>("Markdown").ok();
        let markdown_parser_class = markdown_converter_class
            .as_ref()
            .and_then(|markdown_class| markdown_class.const_get::<_, RClass>("KramdownParser").ok());
        Ok(Self {
            ruby,
            logger,
            liquid_renderer_class,
            utils,
            excerpt_class,
            plugin_priorities,
            identity_converter_class,
            markdown_converter_class,
            markdown_parser_class,
        })
    }

    fn symbol(&self, name: &str) -> Value {
        self.ruby.sym_new(name).into_value_with(self.ruby)
    }

    fn str(&self, value: &str) -> Value {
        self.ruby.str_new(value).into_value_with(self.ruby)
    }
}

thread_local! {
    static CONVERTER_REGISTRY: RefCell<HashMap<i64, SiteConverterRegistry>> =
        RefCell::new(HashMap::new());
}

struct SiteConverterRegistry {
    array_object_id: i64,
    array_hash: i64,
    entries: Vec<ConverterEntry>,
    identity_converter: Option<Value>,
    per_extension: HashMap<String, Vec<usize>>,
}

impl SiteConverterRegistry {
    fn from_values(
        ctx: &RenderingContext,
        converters: &[Value],
        array_object_id: i64,
        array_hash: i64,
    ) -> Result<Self, Error> {
        let rust_converters = rust_converters();
        let mut entries = Vec::with_capacity(converters.len() + rust_converters.len());
        let mut identity_converter = None;
        let mut order = 0;

        for converter in converters {
            if converter.is_kind_of(ctx.identity_converter_class) {
                identity_converter = Some(*converter);
                continue;
            }

            let converter_class: Value = converter.funcall::<_, _, Value>("class", ())?;
            let priority_symbol: Value = converter_class.funcall::<_, _, Value>("priority", ())?;
            let priority_value: Value = ctx
                .plugin_priorities
                .funcall::<_, _, Value>("[]", (priority_symbol,))?;
            let priority = if priority_value.is_nil() {
                0
            } else {
                i64::try_convert(priority_value)? as i32
            };

            entries.push(ConverterEntry {
                priority,
                order,
                kind: ConverterKind::Ruby(*converter),
            });
            order += 1;
        }

        for converter in rust_converters {
            entries.push(ConverterEntry {
                priority: converter.priority(),
                order,
                kind: ConverterKind::Rust(converter),
            });
            order += 1;
        }

        Ok(Self {
            array_object_id,
            array_hash,
            entries,
            identity_converter,
            per_extension: HashMap::new(),
        })
    }

    fn update_from_values(
        &mut self,
        ctx: &RenderingContext,
        converters: &[Value],
        array_object_id: i64,
        array_hash: i64,
    ) -> Result<(), Error> {
        let rust_converters = rust_converters();
        let mut entries = Vec::with_capacity(converters.len() + rust_converters.len());
        let mut identity_converter = None;
        let mut order = 0;

        for converter in converters {
            if converter.is_kind_of(ctx.identity_converter_class) {
                identity_converter = Some(*converter);
                continue;
            }

            let converter_class: Value = converter.funcall::<_, _, Value>("class", ())?;
            let priority_symbol: Value = converter_class.funcall::<_, _, Value>("priority", ())?;
            let priority_value: Value = ctx
                .plugin_priorities
                .funcall::<_, _, Value>("[]", (priority_symbol,))?;
            let priority = if priority_value.is_nil() {
                0
            } else {
                i64::try_convert(priority_value)? as i32
            };

            entries.push(ConverterEntry {
                priority,
                order,
                kind: ConverterKind::Ruby(*converter),
            });
            order += 1;
        }

        for converter in rust_converters {
            entries.push(ConverterEntry {
                priority: converter.priority(),
                order,
                kind: ConverterKind::Rust(converter),
            });
            order += 1;
        }

        self.array_object_id = array_object_id;
        self.array_hash = array_hash;
        self.entries = entries;
        self.identity_converter = identity_converter;
        self.per_extension.clear();
        Ok(())
    }

    fn converters_for(
        &mut self,
        ctx: &RenderingContext,
        site: Value,
        ext_value: Value,
        ext_string: &str,
    ) -> Result<Vec<ConverterKind>, Error> {
        if let Some(indices) = self.per_extension.get(ext_string) {
            let mut cached =
                Vec::with_capacity(indices.len() + self.identity_converter.map(|_| 1).unwrap_or(0));
            for &index in indices {
                cached.push(self.entries[index].kind);
            }
            if let Some(identity) = self.identity_converter {
                cached.push(ConverterKind::Ruby(identity));
            }
            return Ok(cached);
        }

        let mut matched_indices = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            let matches = match entry.kind {
                ConverterKind::Ruby(converter) => {
                    let matched: Value = converter.funcall("matches", (ext_value,))?;
                    matched.to_bool()
                }
                ConverterKind::Rust(converter) => {
                    let site_clone = site;
                    converter.matches(ctx, site_clone, ext_string)?
                }
            };

            if matches {
                matched_indices.push(index);
            }
        }

        matched_indices.sort_by(|a, b| {
            let a_entry = &self.entries[*a];
            let b_entry = &self.entries[*b];
            b_entry
                .priority
                .cmp(&a_entry.priority)
                .then_with(|| a_entry.order.cmp(&b_entry.order))
        });

        let mut converters = Vec::with_capacity(
            matched_indices.len() + self.identity_converter.map(|_| 1).unwrap_or(0),
        );
        for &index in matched_indices.iter() {
            converters.push(self.entries[index].kind);
        }

        if let Some(identity) = self.identity_converter {
            converters.push(ConverterKind::Ruby(identity));
        }

        self.per_extension
            .insert(ext_string.to_string(), matched_indices);

        Ok(converters)
    }
}

fn converter_chain_for_site(
    ctx: &RenderingContext,
    site: Value,
    converters_array: RArray,
    array_object_id: i64,
    array_hash: i64,
    ext_value: Value,
    ext_string: &str,
) -> Result<Vec<ConverterKind>, Error> {
    let site_object_id: i64 = i64::try_convert(site.funcall::<_, _, Value>("object_id", ())?)?;

    let initial_values = CONVERTER_REGISTRY.with(|cache| {
        let map = cache.borrow();
        match map.get(&site_object_id) {
            Some(registry) => {
                if registry.array_object_id != array_object_id || registry.array_hash != array_hash
                {
                    Some(
                        converters_array
                            .each()
                            .collect::<Result<Vec<Value>, Error>>(),
                    )
                } else {
                    None
                }
            }
            None => Some(
                converters_array
                    .each()
                    .collect::<Result<Vec<Value>, Error>>(),
            ),
        }
    });

    CONVERTER_REGISTRY.with(|cache| -> Result<Vec<ConverterKind>, Error> {
        let mut map = cache.borrow_mut();
        let mut converter_values_storage: Option<Vec<Value>> = initial_values.transpose()?;

        if !map.contains_key(&site_object_id) {
            if converter_values_storage.is_none() {
                converter_values_storage = Some(
                    converters_array
                        .each()
                        .collect::<Result<Vec<Value>, Error>>()?,
                );
            }

            let values = converter_values_storage.as_ref().ok_or_else(|| {
                Error::new(
                    ctx.ruby.exception_runtime_error(),
                    "converter registry missing entries",
                )
            })?;
            let registry = SiteConverterRegistry::from_values(
                ctx,
                values.as_slice(),
                array_object_id,
                array_hash,
            )?;
            map.insert(site_object_id, registry);
        }

        let registry = map
            .get_mut(&site_object_id)
            .expect("converter registry must exist");
        if registry.array_object_id != array_object_id || registry.array_hash != array_hash {
            if converter_values_storage.is_none() {
                converter_values_storage = Some(
                    converters_array
                        .each()
                        .collect::<Result<Vec<Value>, Error>>()?,
                );
            }

            let values = converter_values_storage.as_ref().ok_or_else(|| {
                Error::new(
                    ctx.ruby.exception_runtime_error(),
                    "converter registry missing entries",
                )
            })?;
            registry.update_from_values(ctx, values.as_slice(), array_object_id, array_hash)?;
        }

        registry.converters_for(ctx, site, ext_value, ext_string)
    })
}

pub(crate) fn render_site(site: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;

    // Validate markdown processor configuration to mirror Ruby behavior
    validate_markdown_processor(&ctx, site)?;

    site.funcall::<_, _, Value>("relative_permalinks_are_deprecated", ())?;

    let payload: Value = site.funcall::<_, _, Value>("site_payload", ())?;
    let layouts: Value = site.funcall::<_, _, Value>("layouts", ())?;

    let pre_render_symbol = ctx.symbol("pre_render");
    let post_render_symbol = ctx.symbol("post_render");

    // Centralized site-level hooks via Bridge
    let jekyll: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;
    let _ = bridge.funcall::<_, _, Value>("hook_trigger_site", (site, pre_render_symbol, payload))?;

    let regenerator: Value = site.funcall::<_, _, Value>("regenerator", ())?;

    render_collections(&ctx, site, payload, layouts, regenerator)?;
    render_pages(&ctx, site, payload, layouts, regenerator)?;

    let _ = bridge.funcall::<_, _, Value>("hook_trigger_site", (site, post_render_symbol, payload))?;
    Ok(())
}

fn validate_markdown_processor(ctx: &RenderingContext, site: Value) -> Result<(), Error> {
    let config: Value = site.funcall("config", ())?;
    let markdown_key = ctx.str("markdown");
    let markdown_val: Value = config.funcall("[]", (markdown_key,))?;
    if markdown_val.is_nil() {
        return Ok(());
    }

    let name = String::try_convert(markdown_val)?;
    // default supported engine
    if name.eq_ignore_ascii_case("kramdown") {
        return Ok(());
    }

    // Only allow custom class names with [A-Za-z0-9_]+ and that are defined under
    // Jekyll::Converters::Markdown
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
    let jekyll: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
    let errors: RModule = jekyll.const_get("Errors")?;
    let fatal: ExceptionClass = errors.const_get("FatalException")?;
        return Err(Error::new(
            fatal,
            format!("Invalid Markdown processor given: {}", name),
        ));
    }

    let jekyll: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
    let converters: RModule = jekyll.const_get("Converters")?;
    let markdown_class: RClass = converters.const_get("Markdown")?;
    let defined: Value = markdown_class.funcall("const_defined?", (ctx.ruby.str_new(&name),))?;
    if defined.to_bool() {
        return Ok(());
    }

    let errors: RModule = jekyll.const_get("Errors")?;
    let fatal: ExceptionClass = errors.const_get("FatalException")?;
    Err(Error::new(
        fatal,
        format!("Invalid Markdown processor given: {}", name),
    ))
}

fn renderer_run(
    site: Value,
    document: Value,
    payload: Option<Value>,
    layouts: Option<Value>,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;

    let payload = payload.unwrap_or(site.funcall::<_, _, Value>("site_payload", ())?);
    let layouts = layouts.unwrap_or(site.funcall::<_, _, Value>("layouts", ())?);

    render_document(&ctx, site, document, payload, layouts)
}

fn renderer_convert(site: Value, document: Value, content: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;
    let converters = collect_converters(&ctx, site, document)?;
    convert_content(&ctx, site, document, &converters, content)
}

fn renderer_output_ext(site: Value, document: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;
    if let Some(ext) = permalink_extension(&ctx, document)? {
        return Ok(ext);
    }
    converter_output_extension(&ctx, site, document)
}

fn renderer_render_liquid(
    site: Value,
    document: Value,
    content: Value,
    payload: Value,
    info: Value,
    path: Value,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;
    let path_option = if path.is_nil() { None } else { Some(path) };
    render_liquid_template(&ctx, site, document, payload, info, content, path_option)
}

fn renderer_place_in_layouts(
    site: Value,
    document: Value,
    content: Value,
    payload: Value,
    info: Value,
    layouts: Value,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;
    let layout_value = if layouts.is_nil() {
        site.funcall::<_, _, Value>("layouts", ())?
    } else {
        layouts
    };
    let site_layouts: Value = site.funcall::<_, _, Value>("layouts", ())?;
    place_in_layouts(
        &ctx,
        site,
        document,
        payload,
        layout_value,
        site_layouts,
        info,
        content,
    )
}

fn renderer_converters(site: Value, document: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;
    let converters = collect_converters(&ctx, site, document)?;
    let mut count = 0;
    for converter in &converters {
        if matches!(converter, ConverterKind::Ruby(_)) {
            count += 1;
        }
    }

    let array = ctx.ruby.ary_new_capa(count);
    for converter in converters {
        if let ConverterKind::Ruby(value) = converter {
            array.push(value)?;
        }
    }
    Ok(array.into_value_with(ctx.ruby))
}

fn render_collections(
    ctx: &RenderingContext,
    site: Value,
    payload: Value,
    layouts: Value,
    regenerator: Value,
) -> Result<(), Error> {
    let collections_value: Value = site.funcall::<_, _, Value>("collections", ())?;
    let Some(collections) = RHash::from_value(collections_value) else {
        return Ok(());
    };

    let post_render_symbol = ctx.symbol("post_render");

    collections.foreach(|_: Value, collection: Value| {
        let docs_value: Value = collection.funcall::<_, _, Value>("docs", ())?;
        if let Some(docs) = RArray::from_value(docs_value) {
            for entry in docs.each() {
                let document = entry?;
                if should_render(&regenerator, document)? {
                    let output = render_document(ctx, site, document, payload, layouts)?;
                    document.funcall::<_, _, Value>("output=", (output,))?;
                    let jekyll: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
                    let rust: RModule = jekyll.const_get("Rust")?;
                    let bridge: RModule = rust.const_get("Bridge")?;
                    let _ = bridge.funcall::<_, _, Value>("hook_trigger_document", (document, post_render_symbol, ctx.ruby.qnil().into_value_with(ctx.ruby)))?;
                }
            }
        }
        Ok(ForEach::Continue)
    })?;

    Ok(())
}

fn render_pages(
    ctx: &RenderingContext,
    site: Value,
    payload: Value,
    layouts: Value,
    regenerator: Value,
) -> Result<(), Error> {
    let pages_value: Value = site.funcall::<_, _, Value>("pages", ())?;
    let Some(pages) = RArray::from_value(pages_value) else {
        return Ok(());
    };

    let post_render_symbol = ctx.symbol("post_render");

    for entry in pages.each() {
        let document = entry?;
        if should_render(&regenerator, document)? {
            let output = render_document(ctx, site, document, payload, layouts)?;
            document.funcall::<_, _, Value>("output=", (output,))?;
            let jekyll: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
            let rust: RModule = jekyll.const_get("Rust")?;
            let bridge: RModule = rust.const_get("Bridge")?;
            let _ = bridge.funcall::<_, _, Value>("hook_trigger_document", (document, post_render_symbol, ctx.ruby.qnil().into_value_with(ctx.ruby)))?;
        }
    }

    Ok(())
}

fn should_render(regenerator: &Value, document: Value) -> Result<bool, Error> {
    let result: Value = regenerator.funcall::<_, _, Value>("regenerate?", (document,))?;
    Ok(result.to_bool())
}

fn log_debug(ctx: &RenderingContext, label: &str, target: Value) -> Result<(), Error> {
    ctx.logger
        .funcall::<_, _, Value>("debug", (ctx.str(label), target))?;
    Ok(())
}

fn assign_page_payload(
    ctx: &RenderingContext,
    document: Value,
    payload: Value,
) -> Result<(), Error> {
    let page_key = ctx.str("page");
    let liquid_value: Value = document.funcall::<_, _, Value>("to_liquid", ())?;
    payload.funcall::<_, _, Value>("[]=", (page_key, liquid_value))?;

    let paginator_key = ctx.str("paginator");
    let responds = document.respond_to("pager", true)?;
    let paginator_value = if responds {
        let pager: Value = document.funcall::<_, _, Value>("pager", ())?;
        if pager.is_nil() {
            ctx.ruby.qnil().into_value_with(ctx.ruby)
        } else {
            pager.funcall::<_, _, Value>("to_liquid", ())?
        }
    } else {
        ctx.ruby.qnil().into_value_with(ctx.ruby)
    };
    payload.funcall::<_, _, Value>("[]=", (paginator_key, paginator_value))?;
    Ok(())
}

fn assign_current_document(
    ctx: &RenderingContext,
    document: Value,
    payload: Value,
) -> Result<(), Error> {
    let site_key = ctx.str("site");
    let site_drop: Value = payload.funcall::<_, _, Value>("[]", (site_key,))?;
    if !site_drop.is_nil() {
        site_drop.funcall::<_, _, Value>("current_document=", (document,))?;
    }
    Ok(())
}

fn collect_converters(
    ctx: &RenderingContext,
    site: Value,
    document: Value,
) -> Result<Vec<ConverterKind>, Error> {
    let raw_extname: Value = document.funcall::<_, _, Value>("extname", ())?;
    let ext_string = if raw_extname.is_nil() {
        String::new()
    } else if let Some(ext) = RString::from_value(raw_extname) {
        ext.to_string()?
    } else {
        let coerced: RString = raw_extname.funcall("to_s", ())?;
        coerced.to_string()?
    };

    let ext_value = ctx
        .ruby
        .str_new(ext_string.as_str())
        .into_value_with(ctx.ruby);

    let converters_value: Value = site.funcall::<_, _, Value>("converters", ())?;
    let Some(converters_array) = RArray::from_value(converters_value) else {
        return Ok(Vec::new());
    };

    let array_object_id: i64 = i64::try_convert(converters_value.funcall("object_id", ())?)?;
    let array_hash: i64 = i64::try_convert(converters_value.funcall("hash", ())?)?;

    converter_chain_for_site(
        ctx,
        site,
        converters_array,
        array_object_id,
        array_hash,
        ext_value,
        &ext_string,
    )
}

fn assign_highlighter_options(
    ctx: &RenderingContext,
    site: Value,
    payload: Value,
    converters: &[ConverterKind],
) -> Result<(), Error> {
    let Some(first) = converters.first() else {
        return Ok(());
    };

    let prefix_key = ctx.str("highlighter_prefix");
    let suffix_key = ctx.str("highlighter_suffix");

    match first {
        ConverterKind::Ruby(converter) => {
            let prefix: Value = converter.funcall("highlighter_prefix", ())?;
            let suffix: Value = converter.funcall("highlighter_suffix", ())?;

            payload.funcall::<_, _, Value>("[]=", (prefix_key, prefix))?;
            payload.funcall::<_, _, Value>("[]=", (suffix_key, suffix))?;
        }
        ConverterKind::Rust(converter) => {
            if let Some((prefix, suffix)) = converter.highlighter_options(ctx, site)? {
                payload.funcall::<_, _, Value>("[]=", (prefix_key, prefix))?;
                payload.funcall::<_, _, Value>("[]=", (suffix_key, suffix))?;
            }
        }
    }

    Ok(())
}

fn assign_layout_data(
    ctx: &RenderingContext,
    document: Value,
    payload: Value,
    layouts: Value,
) -> Result<(), Error> {
    let data: Value = document.funcall::<_, _, Value>("data", ())?;
    let layout_key = ctx.str("layout");
    let layout_name: Value = data.funcall("[]", (layout_key,))?;
    if layout_name.is_nil() {
        return Ok(());
    }

    let layout_key: Value = layout_name.funcall::<_, _, Value>("to_s", ())?;
    let layout: Value = layouts.funcall::<_, _, Value>("[]", (layout_key,))?;
    if layout.is_nil() {
        return Ok(());
    }

    let layout_data: Value = layout.funcall::<_, _, Value>("data", ())?;
    let existing: Value = payload.funcall::<_, _, Value>("[]", (layout_key,))?;
    let merged = if existing.is_nil() {
        let empty_hash = ctx.ruby.hash_new();
        ctx.utils
            .funcall::<_, _, Value>("deep_merge_hashes", (layout_data, empty_hash))?
    } else {
        ctx.utils
            .funcall::<_, _, Value>("deep_merge_hashes", (layout_data, existing))?
    };
    payload.funcall::<_, _, Value>("[]=", (layout_key, merged))?;
    Ok(())
}

fn build_render_info(ctx: &RenderingContext, site: Value, payload: Value) -> Result<Value, Error> {
    let info = ctx.ruby.hash_new();
    let registers = ctx.ruby.hash_new();

    let site_symbol = ctx.symbol("site");
    let page_symbol = ctx.symbol("page");
    let registers_symbol = ctx.symbol("registers");
    let strict_filters_symbol = ctx.symbol("strict_filters");
    let strict_variables_symbol = ctx.symbol("strict_variables");

    registers.funcall::<_, _, Value>("[]=", (site_symbol, site))?;
    let page_value: Value = payload.funcall::<_, _, Value>("[]", (ctx.str("page"),))?;
    registers.funcall::<_, _, Value>("[]=", (page_symbol, page_value))?;

    let config: Value = site.funcall::<_, _, Value>("config", ())?;
    let liquid_options: Value = config.funcall::<_, _, Value>("[]", (ctx.str("liquid"),))?;
    let strict_filters: Value =
        liquid_options.funcall::<_, _, Value>("[]", (ctx.str("strict_filters"),))?;
    let strict_variables: Value =
        liquid_options.funcall::<_, _, Value>("[]", (ctx.str("strict_variables"),))?;

    let registers_value = registers.into_value_with(ctx.ruby);
    info.funcall::<_, _, Value>("[]=", (registers_symbol, registers_value))?;
    info.funcall::<_, _, Value>("[]=", (strict_filters_symbol, strict_filters))?;
    info.funcall::<_, _, Value>("[]=", (strict_variables_symbol, strict_variables))?;
    Ok(info.into_value_with(ctx.ruby))
}

fn render_liquid_template(
    ctx: &RenderingContext,
    _site: Value,
    document: Value,
    payload: Value,
    info: Value,
    content: Value,
    path: Option<Value>,
) -> Result<Value, Error> {
    let error_path = match path {
        Some(value) => value,
        None => document.funcall::<_, _, Value>("relative_path", ())?,
    };

    // Coerce content to String
    let content_string = match String::try_convert(content) {
        Ok(s) => s,
        Err(_) => {
            let s: RString = content.funcall("to_s", ())?;
            s.to_string()?
        }
    };

    // Read strictness flags from info
    let strict_filters_val: Value = info.funcall::<_, _, Value>("[]", (ctx.str("strict_filters"),))?;
    let strict_variables_val: Value = info.funcall::<_, _, Value>("[]", (ctx.str("strict_variables"),))?;
    let mut strict_filters = strict_filters_val.to_bool();
    let mut strict_variables = strict_variables_val.to_bool();
    if !(strict_filters || strict_variables) {
        // Fallback: read directly from site.config.liquid
        let cfg: Value = _site.funcall::<_, _, Value>("config", ())?;
        let liq: Value = cfg.funcall::<_, _, Value>("[]", (ctx.str("liquid"),))?;
        if !liq.is_nil() {
            let sf: Value = liq.funcall::<_, _, Value>("[]", (ctx.str("strict_filters"),))?;
            let sv: Value = liq.funcall::<_, _, Value>("[]", (ctx.str("strict_variables"),))?;
            strict_filters = strict_filters || sf.to_bool();
            strict_variables = strict_variables || sv.to_bool();
        }
    }

    // Render using Rust Liquid engine

    // In strict modes, delegate rendering to Ruby Liquid to match error semantics exactly
    if strict_filters || strict_variables {
        let liquid_renderer: Value = _site.funcall::<_, _, Value>("liquid_renderer", ())?;
        let path_value = match path {
            Some(ref value) => value.clone(),
            None => ctx.ruby.qnil().into_value_with(ctx.ruby),
        };
        let file = liquid_renderer.funcall::<_, _, Value>("file", (path_value,))?;
        let template = file.funcall::<_, _, Value>("parse", (content,))?;
        match template.funcall::<_, _, Value>("render!", (payload, info)) {
            Ok(v) => return Ok(v),
            Err(rb_err) => {
                if let Some(exc) = rb_err.value() {
                    let formatted: Value = ctx
                        .liquid_renderer_class
                        .funcall::<_, _, Value>("format_error", (exc, error_path))?;
                    ctx.logger
                        .funcall::<_, _, Value>("error", (ctx.str("Liquid Exception:"), formatted))?;
                }
                return Err(rb_err);
            }
        }
    }

    match crate::liquid_engine::render_template(&content_string, payload, info, path) {
        Ok(rendered) => {
            let rendered_value = ctx
                .ruby
                .str_new(rendered.as_str())
                .into_value_with(ctx.ruby);
            Ok(rendered_value)
        }
        Err(err) => {
            // No proactive delegation: keep Rust engine path
            // Excerpt recursion guard: if an excerpt render overflows, log and continue with raw content
            if let Some(ref pval) = path {
                if let Ok(ps) = RString::try_convert(pval.funcall::<_, _, Value>("to_s", ())?) {
                    // Safe conversion to String using to_string; lossy requires unsafe
                    if ps.to_string()?.contains("#excerpt") {
                        let msg_s = err.to_string();
                        if msg_s.contains("stack level too deep") || msg_s.contains("cycle detected") {
                            // Downgrade to a warning and return the unrendered content to avoid build failure
                            let warn_label = ctx.ruby.str_new("Liquid Exception (excerpt):");
                            let warn_msg = ctx.ruby.str_new(msg_s.as_str());
                            let _ = ctx.logger.funcall::<_, _, Value>("warn", (warn_label, warn_msg));
                            return Ok(content);
                        }
                    }
                }
            }

            let msg_s = err.to_string();
            // Gracefully handle non-strict unknown index lookups by delegating to Ruby Liquid
            if msg_s.contains("Unknown index") && !strict_variables {
                let liquid_renderer: Value = _site.funcall::<_, _, Value>("liquid_renderer", ())?;
                let path_value = match path {
                    Some(ref value) => value.clone(),
                    None => ctx.ruby.qnil().into_value_with(ctx.ruby),
                };
                let file = liquid_renderer.funcall::<_, _, Value>("file", (path_value,))?;
                let template = file.funcall::<_, _, Value>("parse", (content,))?;
                return template.funcall::<_, _, Value>("render!", (payload, info));
            }
            if msg_s.contains("Unknown filter") {
                // In non-strict mode, let Ruby Liquid handle unknown filters without failing the build.
                let liquid_renderer: Value = _site.funcall::<_, _, Value>("liquid_renderer", ())?;
                let path_value = match path {
                    Some(ref value) => value.clone(),
                    None => ctx.ruby.qnil().into_value_with(ctx.ruby),
                };
                let file = liquid_renderer.funcall::<_, _, Value>("file", (path_value,))?;
                let template = file.funcall::<_, _, Value>("parse", (content,))?;
                return template.funcall::<_, _, Value>("render!", (payload, info));
            }
            // No further delegation to Ruby Liquid in non-strict modes

            // Otherwise, format and log the Liquid error using Ruby's formatter
            let mut cleaned = msg_s.replace("liquid: ", "");
            if let Some(s) = cleaned.strip_prefix("RuntimeError: ") { cleaned = s.to_string(); }
            let msg = ctx.ruby.str_new(&cleaned);
            let exception_obj = ctx
                .ruby
                .exception_runtime_error()
                .new_instance((msg,))?;
            let formatted: Value = ctx
                .liquid_renderer_class
                .funcall::<_, _, Value>("format_error", (exception_obj, error_path))?;
            ctx.logger
                .funcall::<_, _, Value>("error", (ctx.str("Liquid Exception:"), formatted))?;
            Err(err)
        }
    }
}

fn convert_content(
    ctx: &RenderingContext,
    site: Value,
    document: Value,
    converters: &[ConverterKind],
    mut content: Value,
) -> Result<Value, Error> {
    for converter in converters {
        match converter {
            ConverterKind::Ruby(value) => {
                if value.is_kind_of(ctx.identity_converter_class)
                    || ctx
                        .markdown_converter_class
                        .map(|class| value.is_kind_of(class))
                        .unwrap_or(false)
                {
                    continue;
                }

                match value.funcall::<_, _, Value>("convert", (content,)) {
                    Ok(result) => {
                        content = result;
                    }
                    Err(err) => {
                        let converter_class: Value = value.funcall::<_, _, Value>("class", ())?;
                        let converter_name: Value =
                            converter_class.funcall::<_, _, Value>("to_s", ())?;
                        let converter_name = String::try_convert(converter_name)?;
                        let relative_path: Value =
                            document.funcall::<_, _, Value>("relative_path", ())?;
                        let relative_path_str = String::try_convert(relative_path)?;
                        let message = format!(
                            "{} encountered an error while converting '{}':",
                            converter_name, relative_path_str
                        );
                        ctx.logger.funcall::<_, _, Value>(
                            "error",
                            (ctx.str("Conversion error:"), ctx.str(&message)),
                        )?;

                        if let Some(exception) = err.value() {
                            let exception_message: Value =
                                exception.funcall::<_, _, Value>("to_s", ())?;
                            ctx.logger.funcall::<_, _, Value>(
                                "error",
                                (ctx.str(""), exception_message),
                            )?;
                        }

                        return Err(err);
                    }
                }
            }
            ConverterKind::Rust(converter) => {
                content = converter.convert(ctx, site, document, content)?;
            }
        }
    }

    Ok(content)
}

fn permalink_extension(ctx: &RenderingContext, document: Value) -> Result<Option<Value>, Error> {
    let permalink: Value = document.funcall::<_, _, Value>("permalink", ())?;
    if permalink.is_nil() {
        return Ok(None);
    }

    let permalink_str = String::try_convert(permalink)?;
    if permalink_str.ends_with('/') {
        return Ok(None);
    }

    if let Some(dot_idx) = permalink_str.rfind('.') {
        if let Some(slash_idx) = permalink_str.rfind('/') {
            if slash_idx > dot_idx {
                return Ok(None);
            }
        }
        let ext = &permalink_str[dot_idx..];
        if !ext.is_empty() {
            return Ok(Some(ctx.str(ext)));
        }
    }

    Ok(None)
}

fn converter_output_extension(
    ctx: &RenderingContext,
    site: Value,
    document: Value,
) -> Result<Value, Error> {
    let extname: Value = document.funcall::<_, _, Value>("extname", ())?;
    // If this looks like a Markdown document and kramdown is enabled, prefer .html
    if let Some(ext_str) = RString::from_value(extname).and_then(|s| s.to_string().ok()) {
        let config: Value = site.funcall("config", ())?;
        if KRAMDOWN_CONVERTER.is_kramdown_enabled(ctx, config)? {
            let exts = KRAMDOWN_CONVERTER.extensions_from_config(ctx, config)?;
            if exts
                .iter()
                .any(|e| e.eq_ignore_ascii_case(ext_str.as_str()))
            {
                return Ok(ctx.str(".html"));
            }
        }
    }
    let converters = collect_converters(ctx, site, document)?;
    let mut exts: Vec<Value> = Vec::new();
    for converter in &converters {
        match converter {
            ConverterKind::Ruby(value) => {
                if value.is_kind_of(ctx.identity_converter_class) {
                    if !extname.is_nil() {
                        exts.push(extname);
                    }
                    continue;
                }
                let out_ext: Value = value.funcall::<_, _, Value>("output_ext", (extname,))?;
                if !out_ext.is_nil() {
                    exts.push(out_ext);
                }
            }
            ConverterKind::Rust(converter) => {
                if let Some(out_ext) = converter.output_ext(ctx, site, extname)? {
                    exts.push(out_ext);
                }
            }
        }
    }

    if exts.is_empty() {
        return if extname.is_nil() {
            Ok(ctx.ruby.qnil().into_value_with(ctx.ruby))
        } else {
            Ok(extname)
        };
    }

    // Prefer the last extension that differs from the original extname.
    let orig = extname;
    for ext in exts.iter().rev() {
        if !ext.equal(orig)? {
            return Ok(*ext);
        }
    }
    // Fallback to the original extname if no converter changed it.
    Ok(orig)
}

fn validate_layout(
    ctx: &RenderingContext,
    document: Value,
    layout_name: Value,
    layout: Value,
) -> Result<(), Error> {
    if layout_name.is_nil() || !layout.is_nil() {
        return Ok(());
    }

    let is_excerpt = document.is_kind_of(ctx.excerpt_class);
    if is_excerpt {
        return Ok(());
    }

    let layout_label: Value = layout_name.funcall::<_, _, Value>("to_s", ())?;
    let layout_label = String::try_convert(layout_label)?;
    let relative_path: Value = document.funcall::<_, _, Value>("relative_path", ())?;
    let relative_path = String::try_convert(relative_path)?;
    let message = format!(
        "Layout '{}' requested in {} does not exist.",
        layout_label, relative_path
    );
    ctx.logger
        .funcall::<_, _, Value>("warn", (ctx.str("Build Warning:"), ctx.str(&message)))?;
    Ok(())
}

fn add_regenerator_dependencies(
    _ctx: &RenderingContext,
    site: Value,
    document: Value,
    layout: Value,
) -> Result<(), Error> {
    let should_write: Value = document.funcall::<_, _, Value>("write?", ())?;
    if !should_write.to_bool() {
        return Ok(());
    }

    let document_path: Value = document.funcall::<_, _, Value>("path", ())?;
    let source_path: Value = site.funcall::<_, _, Value>("in_source_dir", (document_path,))?;
    let layout_path: Value = layout.funcall::<_, _, Value>("path", ())?;
    let regenerator: Value = site.funcall::<_, _, Value>("regenerator", ())?;
    regenerator.funcall::<_, _, Value>("add_dependency", (source_path, layout_path))?;
    Ok(())
}

fn render_layout(
    ctx: &RenderingContext,
    site: Value,
    payload: Value,
    layout: Value,
    info: Value,
    content: Value,
) -> Result<Value, Error> {
    let content_key = ctx.str("content");
    let layout_key = ctx.str("layout");
    payload.funcall::<_, _, Value>("[]=", (content_key, content))?;

    let layout_data: Value = layout.funcall::<_, _, Value>("data", ())?;
    let existing: Value = payload.funcall::<_, _, Value>("[]", (layout_key,))?;
    let merged = if existing.is_nil() {
        let empty_hash = ctx.ruby.hash_new();
        ctx.utils
            .funcall::<_, _, Value>("deep_merge_hashes", (layout_data, empty_hash))?
    } else {
        ctx.utils
            .funcall::<_, _, Value>("deep_merge_hashes", (layout_data, existing))?
    };
    payload.funcall::<_, _, Value>("[]=", (layout_key, merged))?;

    let layout_content: Value = layout.funcall::<_, _, Value>("content", ())?;
    let layout_path: Value = layout.funcall::<_, _, Value>("path", ())?;
    // Update Ruby LiquidRenderer stats table for the layout path to preserve CLI output parity
    let liquid_renderer: Value = site.funcall::<_, _, Value>("liquid_renderer", ())?;
    let file = liquid_renderer.funcall::<_, _, Value>("file", (layout_path,))?;
    let _ = file.funcall::<_, _, Value>("parse", (layout_content,))?;
    render_liquid_template(
        ctx,
        site,
        layout,
        payload,
        info,
        layout_content,
        Some(layout_path),
    )
}

fn place_in_layouts(
    ctx: &RenderingContext,
    site: Value,
    document: Value,
    payload: Value,
    layouts: Value,
    site_layouts: Value,
    info: Value,
    mut output: Value,
) -> Result<Value, Error> {
    let data: Value = document.funcall::<_, _, Value>("data", ())?;
    let layout_key = ctx.str("layout");
    let layout_name: Value = data.funcall::<_, _, Value>("[]", (layout_key,))?;
    if layout_name.is_nil() {
        return Ok(output);
    }

    let layout_lookup: Value = layout_name.funcall::<_, _, Value>("to_s", ())?;
    let mut layout: Value = layouts.funcall::<_, _, Value>("[]", (layout_lookup,))?;
    validate_layout(ctx, document, layout_name, layout)?;
    if layout.is_nil() {
        return Ok(output);
    }

    let relative_path: Value = document.funcall::<_, _, Value>("relative_path", ())?;
    let mut seen = HashSet::new();
    payload.funcall::<_, _, Value>(
        "[]=",
        (layout_key, ctx.ruby.qnil().into_value_with(ctx.ruby)),
    )?;

    loop {
        log_debug(ctx, "Rendering Layout:", relative_path)?;
        let layout_id_value: Value = layout.funcall::<_, _, Value>("object_id", ())?;
        let layout_id: i64 = i64::try_convert(layout_id_value)?;
        if !seen.insert(layout_id) {
            break;
        }

        output = render_layout(ctx, site, payload, layout, info, output)?;
        add_regenerator_dependencies(ctx, site, document, layout)?;

        let next_name: Value = layout
            .funcall::<_, _, Value>("data", ())?
            .funcall::<_, _, Value>("[]", (layout_key,))?;
        if next_name.is_nil() {
            break;
        }

        let next_key: Value = next_name.funcall::<_, _, Value>("to_s", ())?;
        let next_layout: Value = site_layouts.funcall::<_, _, Value>("[]", (next_key,))?;
        if next_layout.is_nil() {
            break;
        }

        layout = next_layout;
    }

    Ok(output)
}

fn render_document(
    ctx: &RenderingContext,
    site: Value,
    document: Value,
    payload: Value,
    layouts: Value,
) -> Result<Value, Error> {
    let relative_path: Value = document.funcall::<_, _, Value>("relative_path", ())?;
    log_debug(ctx, "Rendering:", relative_path)?;

    assign_page_payload(ctx, document, payload)?;
    assign_current_document(ctx, document, payload)?;

    let converters = collect_converters(ctx, site, document)?;
    assign_highlighter_options(ctx, site, payload, &converters)?;
    assign_layout_data(ctx, document, payload, layouts)?;

    log_debug(ctx, "Pre-Render Hooks:", relative_path)?;
    let pre_render_symbol = ctx.symbol("pre_render");
    // Centralized hook firing via Bridge
    let jekyll: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
    let rust: RModule = jekyll.const_get("Rust")?;
    let bridge: RModule = rust.const_get("Bridge")?;
    let _ = bridge.funcall::<_, _, Value>("hook_trigger_document", (document, pre_render_symbol, payload))?;

    let info = build_render_info(ctx, site, payload)?;

    let mut output: Value = document.funcall::<_, _, Value>("content", ())?;
    let render_liquid = document
        .funcall::<_, _, Value>("render_with_liquid?", ())?
        .to_bool();
    if render_liquid {
        log_debug(ctx, "Rendering Liquid:", relative_path)?;
        let document_path: Value = document.funcall::<_, _, Value>("path", ())?;
        output = render_liquid_template(
            ctx,
            site,
            document,
            payload,
            info,
            output,
            Some(document_path),
        )?;
    }

    log_debug(ctx, "Rendering Markup:", relative_path)?;
    output = convert_content(ctx, site, document, &converters, output)?;
    document.funcall::<_, _, Value>("content=", (output,))?;

    log_debug(ctx, "Post-Convert Hooks:", relative_path)?;
    let post_convert_symbol = ctx.symbol("post_convert");
    let _ = bridge.funcall::<_, _, Value>("hook_trigger_document", (document, post_convert_symbol, ctx.ruby.qnil().into_value_with(ctx.ruby)))?;

    output = document.funcall::<_, _, Value>("content", ())?;

    let place_in_layout = document
        .funcall::<_, _, Value>("place_in_layout?", ())?
        .to_bool();
    if place_in_layout {
        let site_layouts: Value = site.funcall::<_, _, Value>("layouts", ())?;
        output = place_in_layouts(
            ctx,
            site,
            document,
            payload,
            layouts,
            site_layouts,
            info,
            output,
        )?;
    }

    Ok(output)
}
