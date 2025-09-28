use std::collections::HashSet;

use magnus::r_hash::ForEach;
use magnus::{function, prelude::*, Error, IntoValue, RArray, RClass, RHash, RModule, Ruby, Value};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
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
    hooks: RModule,
    logger: Value,
    liquid_renderer_class: Value,
    utils: Value,
    excerpt_class: RClass,
}

impl<'ruby> RenderingContext<'ruby> {
    fn new(ruby: &'ruby Ruby) -> Result<Self, Error> {
        let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
        let hooks: RModule = jekyll.const_get("Hooks")?;
        let logger: Value = jekyll.funcall::<_, _, Value>("logger", ())?;
        let liquid_renderer_class: Value = jekyll.const_get("LiquidRenderer")?;
        let utils: Value = jekyll.const_get("Utils")?;
        let excerpt_class: RClass = jekyll.const_get("Excerpt")?;
        Ok(Self {
            ruby,
            hooks,
            logger,
            liquid_renderer_class,
            utils,
            excerpt_class,
        })
    }

    fn symbol(&self, name: &str) -> Value {
        self.ruby.sym_new(name).into_value_with(self.ruby)
    }

    fn str(&self, value: &str) -> Value {
        self.ruby.str_new(value).into_value_with(self.ruby)
    }
}

pub(crate) fn render_site(site: Value) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let ctx = RenderingContext::new(&ruby)?;

    site.funcall::<_, _, Value>("relative_permalinks_are_deprecated", ())?;

    let payload: Value = site.funcall::<_, _, Value>("site_payload", ())?;
    let layouts: Value = site.funcall::<_, _, Value>("layouts", ())?;

    let site_symbol = ctx.symbol("site");
    let pre_render_symbol = ctx.symbol("pre_render");
    let post_render_symbol = ctx.symbol("post_render");

    ctx.hooks
        .funcall::<_, _, Value>("trigger", (site_symbol, pre_render_symbol, site, payload))?;

    let regenerator: Value = site.funcall::<_, _, Value>("regenerator", ())?;

    render_collections(&ctx, site, payload, layouts, regenerator)?;
    render_pages(&ctx, site, payload, layouts, regenerator)?;

    ctx.hooks
        .funcall::<_, _, Value>("trigger", (site_symbol, post_render_symbol, site, payload))?;
    Ok(())
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
    convert_content(&ctx, document, &converters, content)
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
    Ok(converters.into_value_with(ctx.ruby))
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
                    document.funcall::<_, _, Value>("trigger_hooks", (post_render_symbol,))?;
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
            document.funcall::<_, _, Value>("trigger_hooks", (post_render_symbol,))?;
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
) -> Result<RArray, Error> {
    let extname: Value = document.funcall::<_, _, Value>("extname", ())?;
    let converters_value: Value = site.funcall::<_, _, Value>("converters", ())?;
    let Some(converters) = RArray::from_value(converters_value) else {
        let empty_array = ctx.ruby.ary_new();
        return Ok(empty_array);
    };

    let filtered = ctx.ruby.ary_new();
    for entry in converters.each() {
        let converter = entry?;
        let matched: Value = converter.funcall::<_, _, Value>("matches", (extname,))?;
        if matched.to_bool() {
            filtered.push(converter)?;
        }
    }

    filtered.funcall::<_, _, Value>("sort!", ())?;
    Ok(filtered)
}

fn assign_highlighter_options(
    ctx: &RenderingContext,
    payload: Value,
    converters: &RArray,
) -> Result<(), Error> {
    if converters.len() == 0 {
        return Ok(());
    }

    let first: Value = converters.entry(0)?;
    let prefix_key = ctx.str("highlighter_prefix");
    let suffix_key = ctx.str("highlighter_suffix");

    let prefix: Value = first.funcall("highlighter_prefix", ())?;
    let suffix: Value = first.funcall("highlighter_suffix", ())?;

    payload.funcall::<_, _, Value>("[]=", (prefix_key, prefix))?;
    payload.funcall::<_, _, Value>("[]=", (suffix_key, suffix))?;
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
    site: Value,
    document: Value,
    payload: Value,
    info: Value,
    content: Value,
    path: Option<Value>,
) -> Result<Value, Error> {
    let liquid_renderer: Value = site.funcall::<_, _, Value>("liquid_renderer", ())?;
    let path_value = match path {
        Some(ref value) => value.clone(),
        None => ctx.ruby.qnil().into_value_with(ctx.ruby),
    };
    let error_path = match path {
        Some(value) => value,
        None => document.funcall::<_, _, Value>("relative_path", ())?,
    };
    let file = liquid_renderer.funcall::<_, _, Value>("file", (path_value,))?;
    let template = file.funcall::<_, _, Value>("parse", (content,))?;

    if let Some(warnings) = RArray::from_value(template.funcall::<_, _, Value>("warnings", ())?) {
        for entry in warnings.each() {
            let warning = entry?;
            let formatted: Value = ctx
                .liquid_renderer_class
                .funcall::<_, _, Value>("format_error", (warning, error_path.clone()))?;
            ctx.logger
                .funcall::<_, _, Value>("warn", (ctx.str("Liquid Warning:"), formatted))?;
        }
    }

    match template.funcall::<_, _, Value>("render!", (payload, info)) {
        Ok(output) => Ok(output),
        Err(err) => {
            if let Some(exception) = err.value() {
                let formatted: Value = ctx
                    .liquid_renderer_class
                    .funcall::<_, _, Value>("format_error", (exception, error_path.clone()))?;
                ctx.logger
                    .funcall::<_, _, Value>("error", (ctx.str("Liquid Exception:"), formatted))?;
            }
            Err(err)
        }
    }
}

fn convert_content(
    ctx: &RenderingContext,
    document: Value,
    converters: &RArray,
    mut content: Value,
) -> Result<Value, Error> {
    for entry in converters.each() {
        let converter = entry?;
        match converter.funcall::<_, _, Value>("convert", (content,)) {
            Ok(result) => {
                content = result;
            }
            Err(err) => {
                let converter_class: Value = converter.funcall::<_, _, Value>("class", ())?;
                let converter_name: Value = converter_class.funcall::<_, _, Value>("to_s", ())?;
                let converter_name = String::try_convert(converter_name)?;
                let relative_path: Value = document.funcall::<_, _, Value>("relative_path", ())?;
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
                    let exception_message: Value = exception.funcall::<_, _, Value>("to_s", ())?;
                    ctx.logger
                        .funcall::<_, _, Value>("error", (ctx.str(""), exception_message))?;
                }

                return Err(err);
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
    let converters = collect_converters(ctx, site, document)?;
    let mut exts: Vec<Value> = Vec::new();
    for entry in converters.each() {
        let converter = entry?;
        let value: Value = converter.funcall::<_, _, Value>("output_ext", (extname,))?;
        if !value.is_nil() {
            exts.push(value);
        }
    }

    if exts.is_empty() {
        return Ok(ctx.ruby.qnil().into_value_with(ctx.ruby));
    }

    let selected = if exts.len() == 1 {
        *exts.last().unwrap()
    } else {
        exts[exts.len() - 2]
    };
    Ok(selected)
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
    assign_highlighter_options(ctx, payload, &converters)?;
    assign_layout_data(ctx, document, payload, layouts)?;

    log_debug(ctx, "Pre-Render Hooks:", relative_path)?;
    let pre_render_symbol = ctx.symbol("pre_render");
    document.funcall::<_, _, Value>("trigger_hooks", (pre_render_symbol, payload))?;

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
    output = convert_content(ctx, document, &converters, output)?;
    document.funcall::<_, _, Value>("content=", (output,))?;

    log_debug(ctx, "Post-Convert Hooks:", relative_path)?;
    let post_convert_symbol = ctx.symbol("post_convert");
    document.funcall::<_, _, Value>("trigger_hooks", (post_convert_symbol,))?;

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
