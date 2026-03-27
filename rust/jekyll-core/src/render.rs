use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use magnus::r_hash::ForEach;
use magnus::{
    function, prelude::*, Error, ExceptionClass, IntoValue, RArray, RClass, RHash, RModule, RString,
    Ruby, Value,
};

use once_cell::sync::Lazy;
use comrak::Options as ComrakOptions;
use regex::Regex;

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
static RUST_MD_NATIVE_CONVERTER: RustMarkdownNativeConverter = RustMarkdownNativeConverter;

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

// Native Markdown converter using comrak. Config is mapped from Jekyll's
// kramdown settings for parity where practical.
struct RustMarkdownNativeConverter;

impl RustMarkdownNativeConverter {
    fn extensions_from_config(
        &self,
        ctx: &RenderingContext,
        config: Value,
    ) -> Result<Vec<String>, Error> {
        KRAMDOWN_CONVERTER.extensions_from_config(ctx, config)
    }

    fn is_kramdown_enabled(&self, ctx: &RenderingContext, config: Value) -> Result<bool, Error> {
        // Honor existing `markdown` setting; treat nil or 'kramdown' as enabled
        let markdown_key = ctx.str("markdown");
        let markdown_engine: Value = config.funcall("[]", (markdown_key,))?;
        if markdown_engine.is_nil() {
            return Ok(true);
        }
        let engine = String::try_convert(markdown_engine)?;
        Ok(engine.eq_ignore_ascii_case("kramdown"))
    }

    fn comrak_options_from_config(&self, ctx: &RenderingContext, site: Value) -> Result<ComrakOptions, Error> {
        let config: Value = site.funcall("config", ())?;
        let mut opts = ComrakOptions::default();
        // Allow raw HTML to match Kramdown
        opts.render.unsafe_ = true;
        // Enable footnotes by default to match Kramdown capabilities
        opts.extension.footnotes = true;
        if let Ok(kramdown) = config.funcall::<_, _, Value>("[]", (ctx.str("kramdown"),)) {
            if !kramdown.is_nil() {
                // hard_wrap -> hardbreaks
                if let Ok(hw) = kramdown.funcall::<_, _, Value>("[]", (ctx.str("hard_wrap"),)) {
                    if !hw.is_nil() && hw.to_bool() {
                        opts.render.hardbreaks = true;
                    }
                }
                // smart punctuation approximation
                if let Ok(sq) = kramdown.funcall::<_, _, Value>("[]", (ctx.str("smart_quotes"),)) {
                    if !sq.is_nil() { opts.parse.smart = true; }
                }
                // input: enable some GFM-like bits when requested
                if let Ok(input) = kramdown.funcall::<_, _, Value>("[]", (ctx.str("input"),)) {
                    if !input.is_nil() {
                        let s = String::try_convert(input)?;
                        if s.eq_ignore_ascii_case("GFM") || s.to_ascii_lowercase().contains("gfm") {
                            opts.extension.table = true;
                            opts.extension.tasklist = true;
                            opts.extension.strikethrough = true;
                            opts.extension.autolink = true;
                        }
                    }
                }
            }
        }
        // Enable a subset commonly used by sites
        opts.extension.table = true;
        opts.extension.strikethrough = true;
        Ok(opts)
    }
}

impl RustConverter for RustMarkdownNativeConverter {
    fn name(&self) -> &'static str { "RustMarkdownNative" }
    fn priority(&self) -> i32 { 5 }
    fn matches(&self, ctx: &RenderingContext, site: Value, ext: &str) -> Result<bool, Error> {
        let config: Value = site.funcall("config", ())?;
        if !self.is_kramdown_enabled(ctx, config)? { return Ok(false); }
        let extensions = self.extensions_from_config(ctx, config)?;
        Ok(extensions.iter().any(|candidate| candidate.eq_ignore_ascii_case(ext)))
    }
    fn convert(&self, ctx: &RenderingContext, site: Value, _document: Value, content: Value) -> Result<Value, Error> {
        let content_s = String::try_convert(content)?;
        let opts = self.comrak_options_from_config(ctx, site)?;

        // Kramdown treats `---` (dashes only) as a thematic break even after a
        // text line, while CommonMark/comrak interprets `text\n---` as a setext
        // h2.  Insert a blank line before dash-only thematic-break lines that
        // follow non-blank text so comrak sees them as breaks, not headings.
        let content_s = prevent_dash_setext_headings(&content_s);

        let mut html = comrak::markdown_to_html(&content_s, &opts);

        // Normalize whitespace between block-level elements to match Kramdown
        // output conventions (double newline between blocks).
        html = normalize_block_whitespace_kramdown_like(&html);

        // Approximate Kramdown's default guess_lang behavior for inline code
        // by tagging bare <code> elements when guess_lang is not explicitly false.
        let config: Value = site.funcall("config", ())?;
        let kd: Value = config.funcall("[]", (ctx.str("kramdown"),))?;
        let guess = if kd.is_nil() {
            true
        } else {
            match kd.funcall::<_, _, Value>("[]", (ctx.str("guess_lang"),)) {
                Ok(v) if !v.is_nil() => v.to_bool(),
                _ => true,
            }
        };
        if guess {
            html = html.replace(
                "<code>",
                "<code class=\"language-plaintext highlighter-rouge\">",
            );
        }

        // Inject heading IDs similar to Kramdown's auto_ids when enabled (default true)
        let auto_ids = if kd.is_nil() {
            true
        } else {
            match kd.funcall::<_, _, Value>("[]", (ctx.str("auto_ids"),)) {
                Ok(v) if !v.is_nil() => v.to_bool(),
                _ => true,
            }
        };
        if auto_ids {
            html = inject_heading_ids_like_kramdown(&html);
        }

        // Normalize footnote markup closer to Kramdown expectations
        html = normalize_footnotes_kramdown_like(&html);
        Ok(ctx.ruby.str_new(&html).into_value_with(ctx.ruby))
    }
    fn output_ext(&self, ctx: &RenderingContext, _site: Value, _original_ext: Value) -> Result<Option<Value>, Error> {
        Ok(Some(ctx.str(".html")))
    }
    fn highlighter_options(&self, ctx: &RenderingContext, _site: Value) -> Result<Option<(Value, Value)>, Error> {
        Ok(Some((ctx.str("\n"), ctx.str("\n"))))
    }
}

fn inject_heading_ids_like_kramdown(input: &str) -> String {
    static RE_SIMPLE_HEADER: once_cell::sync::Lazy<Regex> = once_cell::sync::Lazy::new(|| {
        // Avoid backreferences: capture opening and closing header levels separately
        Regex::new(r"(?i)<h([1-6])>([^<]+)</h([1-6])>").expect("valid header regex without backrefs")
    });
    RE_SIMPLE_HEADER
        .replace_all(input, |caps: &regex::Captures| {
            let open = &caps[1];
            let close = &caps[3];
            if open != close {
                return caps.get(0).map(|m| m.as_str()).unwrap_or("").to_string();
            }
            let text = caps[2].trim();
            let id = slugify_heading_kramdown_like(text);
            format!("<h{lvl} id=\"{id}\">{text}</h{lvl}>", lvl = open, id = id, text = text)
        })
        .into_owned()
}

/// In kramdown a line consisting solely of dashes (`---`, `----`, etc.) is
/// always parsed as a thematic break, even when it immediately follows a text
/// line.  CommonMark gives setext heading underlines (`---`) higher priority
/// than thematic breaks, so `text\n---` becomes `<h2>text</h2>` in comrak.
///
/// This function inserts a blank line before any dash-only line that directly
/// follows a non-blank line, forcing comrak to treat the dashes as a thematic
/// break instead of a setext heading underline.
fn prevent_dash_setext_headings(content: &str) -> String {
    static DASH_ONLY: once_cell::sync::Lazy<Regex> =
        once_cell::sync::Lazy::new(|| Regex::new(r"^[ ]{0,3}-{3,}\s*$").unwrap());

    let mut result = String::with_capacity(content.len() + 64);
    let mut prev_was_nonblank = false;

    for line in content.split('\n') {
        let is_dash_line = DASH_ONLY.is_match(line);
        if prev_was_nonblank && is_dash_line {
            // Insert blank line to prevent setext heading interpretation
            result.push('\n');
        }
        result.push_str(line);
        result.push('\n');
        prev_was_nonblank = !line.trim().is_empty();
    }

    // The split-and-push loop adds one trailing \n; remove it if the original
    // content did not end with a newline.
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

fn slugify_heading_kramdown_like(text: &str) -> String {
    // Transliterate to ASCII, downcase, replace non-alnum with dashes, collapse repeats
    let ascii = deunicode::deunicode(text);
    static NON_ALNUM: once_cell::sync::Lazy<Regex> =
        once_cell::sync::Lazy::new(|| Regex::new(r"[^A-Za-z0-9]+").unwrap());
    let mut slug = NON_ALNUM.replace_all(&ascii, "-").into_owned();
    slug.make_ascii_lowercase();
    slug.trim_matches('-').to_string()
}

fn normalize_footnotes_kramdown_like(input: &str) -> String {
    let mut out = input.to_string();

    // ── 1. Footnote references ──────────────────────────────────────────
    // Comrak 0.21 produces:
    //   <sup class="footnote-ref"><a href="#fn-1" id="fnref-1" data-footnote-ref>1</a></sup>
    // Kramdown expects:
    //   <sup id="fnref:1"><a href="#fn:1" class="footnote" rel="footnote" role="doc-noteref">1</a></sup>
    let re_sup = Regex::new(
        r##"<sup class="footnote-ref"><a href="#fn-(\d+)" id="fnref-(\d+)" data-footnote-ref>(\d+)</a></sup>"##
    ).expect("valid sup regex");
    out = re_sup
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            format!(
                "<sup id=\"fnref:{n}\"><a href=\"#fn:{n}\" class=\"footnote\" rel=\"footnote\" role=\"doc-noteref\">{n}</a></sup>",
                n = n
            )
        })
        .into_owned();

    // ── 2. Footnote definition list item ids ────────────────────────────
    // Comrak:  <li id="fn-1">
    // Kramdown: <li id="fn:1">
    let re_def = Regex::new(r##"<li id="fn-(\d+)">"##).expect("valid def id regex");
    out = re_def
        .replace_all(&out, |caps: &regex::Captures| {
            format!("<li id=\"fn:{}\">", &caps[1])
        })
        .into_owned();

    // ── 3. Backlinks ────────────────────────────────────────────────────
    // Comrak:
    //   <a href="#fnref-1" class="footnote-backref" ...>↩</a>
    // Kramdown:
    //   &nbsp;<a href="#fnref:1" class="reversefootnote" role="doc-backlink">&#8617;</a>
    let re_back = Regex::new(
        r##" ?<a href="#fnref-(\d+)" class="footnote-backref"[^>]*>.*?</a>"##
    ).expect("valid backref regex");
    out = re_back
        .replace_all(&out, |caps: &regex::Captures| {
            let n = &caps[1];
            format!(
                "\u{00A0}<a href=\"#fnref:{n}\" class=\"reversefootnote\" role=\"doc-backlink\">&#8617;</a>",
                n = n
            )
        })
        .into_owned();

    // ── 4. Unwrap <section class="footnotes"> wrapper ───────────────────
    // Comrak wraps footnotes in <section>, kramdown uses <div>
    out = out.replace(
        "<section class=\"footnotes\" data-footnotes>",
        "<div class=\"footnotes\" role=\"doc-endnotes\">",
    );
    out = out.replace("</section>", "</div>");

    out
}

/// Normalize whitespace between block-level elements to match Kramdown output.
///
/// Kramdown emits a blank line (`\n\n`) between adjacent block-level elements
/// (e.g. `</p>\n\n<p>`, `</p>\n\n<div>`). Comrak only emits a single `\n` (or
/// nothing for raw HTML blocks). This pass inserts the extra newline where
/// needed so rendered output matches test expectations.
fn normalize_block_whitespace_kramdown_like(input: &str) -> String {
    // Block-level tags that Kramdown separates with a blank line.
    // We handle two cases:
    //  1. Comrak already placed a single \n between blocks (Markdown paragraphs) →
    //     upgrade to \n\n to match Kramdown.
    //  2. No whitespace at all between adjacent block elements (raw HTML in
    //     Markdown) → insert a single \n to match Kramdown.
    static BLOCK_TAGS: &str = r"p|div|h[1-6]|ul|ol|li|blockquote|pre|hr|table|thead|tbody|tfoot|tr|td|th|dl|dt|dd|section|article|aside|header|footer|nav|main|figure|figcaption|details|summary|fieldset|form";

    // Case 1: closing block tag + exactly one \n + opening block tag → \n\n
    static RE_SINGLE_NL: Lazy<Regex> = Lazy::new(|| {
        let pat = format!(
            r"(?i)(</(?:{tags})>)\n(<(?:{tags})[\s>])",
            tags = BLOCK_TAGS
        );
        Regex::new(&pat).expect("valid single-nl block-gap regex")
    });

    // Case 2: closing block tag + NO newline + opening block tag → insert \n
    static RE_NO_NL: Lazy<Regex> = Lazy::new(|| {
        let pat = format!(
            r"(?i)(</(?:{tags})>)(<(?:{tags})[\s>])",
            tags = BLOCK_TAGS
        );
        Regex::new(&pat).expect("valid no-nl block-gap regex")
    });

    let out = RE_SINGLE_NL.replace_all(input, "$1\n\n$2");
    RE_NO_NL.replace_all(&out, "$1\n$2").into_owned()
}

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    // Register native Markdown converter (comrak-based). Ruby Kramdown class is
    // still referenced for config/extension parsing but is not registered here.
    register_rust_converter(&RUST_MD_NATIVE_CONVERTER);

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
    // Pre-cached Ruby strings for frequently used hash keys (avoids repeated allocation).
    s_content: Value,
    s_layout: Value,
    s_page: Value,
    s_site: Value,
    s_markdown: Value,
    s_liquid: Value,
    s_strict_filters: Value,
    s_strict_variables: Value,
    s_config: Value,
    s_kramdown: Value,
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

        // Pre-cache frequently used Ruby strings (frozen by Ruby VM)
        let s = |v: &str| -> Value { ruby.str_new(v).into_value_with(ruby) };

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
            s_content: s("content"),
            s_layout: s("layout"),
            s_page: s("page"),
            s_site: s("site"),
            s_markdown: s("markdown"),
            s_liquid: s("liquid"),
            s_strict_filters: s("strict_filters"),
            s_strict_variables: s("strict_variables"),
            s_config: s("config"),
            s_kramdown: s("kramdown"),
        })
    }

    fn symbol(&self, name: &str) -> Value {
        self.ruby.sym_new(name).into_value_with(self.ruby)
    }

    fn str(&self, value: &str) -> Value {
        self.ruby.str_new(value).into_value_with(self.ruby)
    }

    fn env_true(&self, key: &str) -> bool {
        if let Ok(env) = self.ruby.class_object().const_get::<_, RHash>("ENV") {
            let k = self.ruby.str_new(key).into_value_with(self.ruby);
            if let Ok(v) = env.aref(k) {
                if let Some(s) = RString::from_value(v) {
                    if let Ok(st) = s.to_string() {
                        return !st.is_empty() && st != "0" && st.to_ascii_lowercase() != "false";
                    }
                }
            }
        }
        false
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
    crate::liquid_engine::clear_liquid_cache();
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
    crate::liquid_engine::dump_render_stats();
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
    // default supported engines
    if name.eq_ignore_ascii_case("kramdown") || name.eq_ignore_ascii_case("rust") {
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

    let jekyll_mod: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
    let rust_mod: RModule = jekyll_mod.const_get("Rust")?;
    let bridge_mod: RModule = rust_mod.const_get("Bridge")?;

    collections.foreach(|_label: Value, collection: Value| {
        let label_str: String = collection.funcall::<_, _, String>("label", ())?;
        let docs_value: Value = collection.funcall::<_, _, Value>("docs", ())?;
        if let Some(docs) = RArray::from_value(docs_value) {
            let total = docs.len();
            let mut rendered = 0usize;
            for entry in docs.each() {
                let document = entry?;
                if should_render(&regenerator, document)? {
                    let output = render_document(ctx, site, document, payload, layouts)?;
                    document.funcall::<_, _, Value>("output=", (output,))?;
                    let _ = bridge_mod.funcall::<_, _, Value>("hook_trigger_document", (document, post_render_symbol, ctx.ruby.qnil().into_value_with(ctx.ruby)))?;
                    rendered += 1;
                    if rendered % 100 == 0 {
                        let msg = format!("Rendered {}/{} docs in '{}'", rendered, total, label_str);
                        let _ = ctx.logger.funcall::<_, _, Value>("info", (ctx.str("  Rust:"), ctx.ruby.str_new(&msg)));
                    }
                }
            }
            if rendered > 0 {
                let msg = format!("Rendered {}/{} docs in '{}'", rendered, total, label_str);
                let _ = ctx.logger.funcall::<_, _, Value>("info", (ctx.str("  Rust:"), ctx.ruby.str_new(&msg)));
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

    let jekyll_mod: RModule = ctx.ruby.class_object().const_get("Jekyll")?;
    let rust_mod: RModule = jekyll_mod.const_get("Rust")?;
    let bridge_mod: RModule = rust_mod.const_get("Bridge")?;
    let total = pages.len();
    let mut rendered = 0usize;

    for entry in pages.each() {
        let document = entry?;
        if should_render(&regenerator, document)? {
            let output = render_document(ctx, site, document, payload, layouts)?;
            document.funcall::<_, _, Value>("output=", (output,))?;
            let _ = bridge_mod.funcall::<_, _, Value>("hook_trigger_document", (document, post_render_symbol, ctx.ruby.qnil().into_value_with(ctx.ruby)))?;
            rendered += 1;
            if rendered % 100 == 0 {
                let msg = format!("Rendered {}/{} pages", rendered, total);
                let _ = ctx.logger.funcall::<_, _, Value>("info", (ctx.str("  Rust:"), ctx.ruby.str_new(&msg)));
            }
        }
    }
    if rendered > 0 {
        let msg = format!("Rendered {}/{} pages total", rendered, total);
        let _ = ctx.logger.funcall::<_, _, Value>("info", (ctx.str("  Rust:"), ctx.ruby.str_new(&msg)));
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
    // Use string keys so render_liquid_template can read them back with string lookups.
    let strict_filters_key = ctx.str("strict_filters");
    let strict_variables_key = ctx.str("strict_variables");

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
    info.funcall::<_, _, Value>("[]=", (strict_filters_key, strict_filters))?;
    info.funcall::<_, _, Value>("[]=", (strict_variables_key, strict_variables))?;
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

    // Update LiquidRenderer stats for this template when a path is known (profile output).
    // IMPORTANT: Only do this when profiling is explicitly enabled; otherwise this
    // double-parses every template through Ruby Liquid just for stats, which is
    // catastrophically slow on large sites (10K+ pages).
    if let Some(ref p) = path {
        let cfg: Value = _site.funcall::<_, _, Value>("config", ())?;
        let profile_val: Value = cfg.funcall::<_, _, Value>("[]", (ctx.str("profile"),))?;
        if profile_val.to_bool() {
            let liquid_renderer: Value = _site.funcall::<_, _, Value>("liquid_renderer", ())?;
            let file = liquid_renderer.funcall::<_, _, Value>("file", (p.clone(),))?;
            let _ = file.funcall::<_, _, Value>("parse", (content,))?;
        }
    }

    // Render using Rust Liquid engine (no Ruby Liquid fallback)

    match crate::liquid_engine::render_template(&content_string, payload, info, path) {
        Ok(rendered) => {
            let rendered_value = ctx
                .ruby
                .str_new(rendered.as_str())
                .into_value_with(ctx.ruby);
            Ok(rendered_value)
        }
        Err(err) => {

            // Excerpt recursion guard: if an excerpt render overflows, log and continue with raw content
            if let Some(ref pval) = path {
                if let Ok(ps) = RString::try_convert(pval.funcall::<_, _, Value>("to_s", ())?) {
                    if ps.to_string()?.contains("#excerpt") {
                        let msg_s = err.to_string();
                        if msg_s.contains("stack level too deep") || msg_s.contains("cycle detected") {
                            let warn_label = ctx.ruby.str_new("Liquid Exception (excerpt):");
                            let warn_msg = ctx.ruby.str_new(msg_s.as_str());
                            let _ = ctx.logger.funcall::<_, _, Value>("warn", (warn_label, warn_msg));
                            return Ok(content);
                        }
                    }
                }
            }

            let msg_s = err.to_string();

            // In non-strict mode, silently ignore unknown index/filter errors.
            // Ruby Liquid's lax mode renders unknown variables as empty strings.
            // Return an empty Ruby string — never return raw Liquid template source.
            if !strict_variables && msg_s.contains("Unknown index") {

                return Ok(ctx.ruby.str_new("").into_value_with(ctx.ruby));
            }
            if !strict_filters && msg_s.contains("Unknown filter") {
                return Ok(ctx.ruby.str_new("").into_value_with(ctx.ruby));
            }

            // Reformat strict mode errors to match Ruby Liquid's message format.
            // Ruby Liquid produces: "Liquid error (line N): undefined variable X"
            // Rust Liquid produces: "Unknown index\n  with:\n    variable=page\n    requested index=X\n ..."
            let mut cleaned = msg_s.replace("liquid: ", "");
            if let Some(s) = cleaned.strip_prefix("RuntimeError: ") { cleaned = s.to_string(); }

            if cleaned.contains("Unknown index") {
                // Extract the requested index name: "requested index=NAME"
                let var_name = cleaned.find("requested index=")
                    .map(|pos| {
                        let rest = &cleaned[pos + 16..];
                        let end = rest.find(|c: char| c == ' ' || c == '\n').unwrap_or(rest.len());
                        rest[..end].to_string()
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                // Compute line number by finding the variable expression in the content
                let line = find_line_of_expression(&content_string, &var_name);
                cleaned = format!("Liquid error (line {}): undefined variable {}", line, var_name);
            } else if cleaned.contains("Unknown filter") {
                // Extract filter name: "requested filter=FILTERNAME"
                let filter_name = cleaned.find("requested filter=")
                    .map(|pos| {
                        let rest = &cleaned[pos + 17..];
                        let end = rest.find(|c: char| c == ' ' || c == '\n').unwrap_or(rest.len());
                        rest[..end].to_string()
                    })
                    .unwrap_or_else(|| "unknown".to_string());
                let line = find_line_of_expression(&content_string, &filter_name);
                cleaned = format!("Liquid error (line {}): undefined filter {}", line, filter_name);
            }

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

/// Find the 1-based line number of a Liquid expression containing `name` in `content`.
/// Searches for {{ ... name ... }} or | name patterns. Returns 1 if not found.
fn find_line_of_expression(content: &str, name: &str) -> usize {
    // Strip leading newline to match Ruby Liquid's line numbering (content
    // after front matter typically starts with \n which Ruby doesn't count).
    let content = content.strip_prefix('\n').unwrap_or(content);
    // Search each line for the expression name inside {{ }}, {% %}, or after |
    for (idx, line) in content.lines().enumerate() {
        if line.contains(name) {
            // Check if it's inside a Liquid expression context
            if line.contains("{{") || line.contains("{%") || line.contains("|") {
                return idx + 1;
            }
        }
    }
    // Fallback: just find the first line containing the name
    for (idx, line) in content.lines().enumerate() {
        if line.contains(name) {
            return idx + 1;
        }
    }
    1
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
    let cfg: Value = site.funcall::<_, _, Value>("config", ())?;
    let profile_val: Value = cfg.funcall::<_, _, Value>("[]", (ctx.str("profile"),))?;
    if profile_val.to_bool() {
        let liquid_renderer: Value = site.funcall::<_, _, Value>("liquid_renderer", ())?;
        let file = liquid_renderer.funcall::<_, _, Value>("file", (layout_path,))?;
        let _ = file.funcall::<_, _, Value>("parse", (layout_content,))?;
    }
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
    if ctx.env_true("JEKYLL_RS_DEBUG_HANG") {
        let _ = ctx
            .logger
            .funcall::<_, _, Value>("info", (ctx.str("Render start:"), relative_path.clone()))?;
    }
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
    // Excerpts must never be placed in layouts. When front-matter defaults
    // assign a layout to all paths (path: ""), excerpt rendering would
    // re-enter the layout/render pipeline recursively, causing a stack
    // overflow.  Skip layout placement entirely for excerpts.
    let is_excerpt = document.is_kind_of(ctx.excerpt_class);
    if place_in_layout && !is_excerpt {
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

    if ctx.env_true("JEKYLL_RS_DEBUG_HANG") {
        let path: Value = document.funcall::<_, _, Value>("relative_path", ())?;
        let _ = ctx
            .logger
            .funcall::<_, _, Value>("info", (ctx.str("Render done:"), path))?;
    }
    Ok(output)
}
