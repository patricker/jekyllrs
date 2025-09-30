use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::fmt;

use kstring::KString;
use liquid::model::{self as liquid_model, Value as LiquidValue};
use liquid::ParserBuilder;
use liquid_core::parser::{BlockReflection, ParseBlock, TagBlock, TagReflection, TagTokenIter};
use liquid_core::ParseTag;
use liquid_core::runtime::Renderable as LiquidRenderable;
use liquid_core::error::{Error as LiquidError, Result as LiquidResult};
use liquid_core::parser::{
    Filter, FilterArguments, FilterReflection, ParameterReflection, ParseFilter,
};
use liquid_core::runtime::{Expression, Runtime};
use magnus::symbol::Symbol;
use magnus::{
    prelude::*, Error, IntoValue, RArray, RClass, RHash, RModule, RString, Ruby, TryConvert, Value,
};

use crate::ruby_utils::ruby_handle;

const MAX_DEPTH: usize = 64;
const EMPTY_PARAMS: &[ParameterReflection] = &[];

#[derive(Copy, Clone)]
struct RubyFilterContext {
    context: Value,
}

thread_local! {
    static RUBY_FILTER_CONTEXT: RefCell<Option<RubyFilterContext>> = RefCell::new(None);
}

thread_local! {
    static TEMPLATE_CACHE: RefCell<HashMap<u64, liquid::Template>> = RefCell::new(HashMap::new());
}

/// Convert a `liquid::Value` back into a Ruby object.
pub fn liquid_value_to_ruby(ruby: &Ruby, value: &LiquidValue) -> Result<Value, Error> {
    match value {
        LiquidValue::Nil => Ok(ruby.qnil().into_value()),
        LiquidValue::State(state) => {
            let stringified = state.to_string();
            Ok(ruby.str_new(stringified.as_str()).into_value_with(ruby))
        }
        LiquidValue::Scalar(scalar) => scalar_to_ruby(ruby, scalar),
        LiquidValue::Array(array) => array_to_ruby(ruby, array),
        LiquidValue::Object(object) => object_to_ruby(ruby, object),
    }
}

fn scalar_to_ruby(ruby: &Ruby, scalar: &liquid_model::Scalar) -> Result<Value, Error> {
    if let Some(boolean) = scalar.to_bool() {
        return Ok(if boolean {
            ruby.qtrue().into_value()
        } else {
            ruby.qfalse().into_value()
        });
    }

    if let Some(integer) = scalar.to_integer() {
        let integer_value = ruby.integer_from_i64(integer);
        return Ok(integer_value.into_value_with(ruby));
    }

    if let Some(float) = scalar.to_float() {
        return Ok(ruby.float_from_f64(float).into_value_with(ruby));
    }

    let stringified = scalar.clone().into_string();
    Ok(ruby.str_new(stringified.as_str()).into_value_with(ruby))
}

fn array_to_ruby(ruby: &Ruby, array: &liquid_model::Array) -> Result<Value, Error> {
    let ruby_array = ruby.ary_new_capa(array.len());
    for item in array.iter() {
        let converted = liquid_value_to_ruby(ruby, item)?;
        ruby_array.push(converted)?;
    }
    Ok(ruby_array.into_value_with(ruby))
}

fn object_to_ruby(ruby: &Ruby, object: &liquid_model::Object) -> Result<Value, Error> {
    let ruby_hash = ruby.hash_new();
    for (key, value) in object.iter() {
        let converted_value = liquid_value_to_ruby(ruby, value)?;
        ruby_hash.aset(ruby.str_new(key.as_str()), converted_value)?;
    }
    Ok(ruby_hash.into_value_with(ruby))
}

/// Helper that converts Ruby objects into `liquid::Value` trees.
pub struct LiquidValueConverter<'ruby> {
    ruby: &'ruby Ruby,
    drop_class: RClass,
    visited: HashSet<i64>,
}

impl<'ruby> LiquidValueConverter<'ruby> {
    /// Create a new converter bound to the provided Ruby handle.
    pub fn new(ruby: &'ruby Ruby) -> Result<Self, Error> {
        let liquid: RModule = ruby.class_object().const_get("Liquid")?;
        let drop_class: RClass = liquid.const_get("Drop")?;
        Ok(Self {
            ruby,
            drop_class,
            visited: HashSet::new(),
        })
    }

    /// Convert a Ruby value into a Liquid value representation.
    pub fn convert(&mut self, value: Value) -> Result<LiquidValue, Error> {
        self.convert_inner(value, 0)
    }

    fn convert_inner(&mut self, value: Value, depth: usize) -> Result<LiquidValue, Error> {
        if depth > MAX_DEPTH {
            return Ok(LiquidValue::Nil);
        }

        if value.is_nil() {
            return Ok(LiquidValue::Nil);
        }
        if value.equal(self.ruby.qtrue())? {
            return Ok(LiquidValue::scalar(true));
        }
        if value.equal(self.ruby.qfalse())? {
            return Ok(LiquidValue::scalar(false));
        }

        if value.is_kind_of(self.ruby.class_integer()) {
            if let Ok(integer) = i64::try_convert(value) {
                return Ok(LiquidValue::scalar(integer));
            }
            let coerced = value.funcall::<_, _, RString>("to_s", ())?;
            return Ok(LiquidValue::scalar(coerced.to_string()?));
        }

        if value.is_kind_of(self.ruby.class_float()) {
            let float_value = f64::try_convert(value)?;
            return Ok(LiquidValue::scalar(float_value));
        }

        if let Some(symbol) = Symbol::from_value(value) {
            let name = symbol.name()?;
            return Ok(LiquidValue::scalar(name.to_string()));
        }

        if let Some(string) = RString::from_value(value) {
            return Ok(LiquidValue::scalar(string.to_string()?));
        }

        if let Some(array) = RArray::from_value(value) {
            if let Some(result) = self.with_guard(value, |this| this.convert_array(array, depth))? {
                return Ok(result);
            }
            return Ok(LiquidValue::Nil);
        }

        if let Some(hash) = RHash::from_value(value) {
            if let Some(result) = self.with_guard(value, |this| this.convert_hash(hash, depth))? {
                return Ok(result);
            }
            return Ok(LiquidValue::Nil);
        }

        if value.respond_to("to_liquid", false)? {
            let liquidified = value.funcall::<_, _, Value>("to_liquid", ())?;
            if !value.equal(liquidified)? {
                return self.convert_inner(liquidified, depth + 1);
            }
        }

        if value.is_kind_of(self.drop_class) {
            return self
                .with_guard(value, |this| this.convert_drop(value, depth))?
                .ok_or_else(|| Error::new(self.ruby.exception_runtime_error(), "cycle detected"));
        }

        if value.respond_to("to_hash", false)? {
            let hash_value = value.funcall::<_, _, Value>("to_hash", ())?;
            if let Some(hash) = RHash::from_value(hash_value) {
                if let Some(result) =
                    self.with_guard(value, |this| this.convert_hash(hash, depth))?
                {
                    return Ok(result);
                }
                return Ok(LiquidValue::Nil);
            }
        }

        if value.respond_to("to_h", false)? {
            let hash_value = value.funcall::<_, _, Value>("to_h", ())?;
            if let Some(hash) = RHash::from_value(hash_value) {
                if let Some(result) =
                    self.with_guard(value, |this| this.convert_hash(hash, depth))?
                {
                    return Ok(result);
                }
                return Ok(LiquidValue::Nil);
            }
        }

        if value.respond_to("to_a", false)? {
            let array_value = value.funcall::<_, _, Value>("to_a", ())?;
            if let Some(array) = RArray::from_value(array_value) {
                if let Some(result) =
                    self.with_guard(value, |this| this.convert_array(array, depth))?
                {
                    return Ok(result);
                }
                return Ok(LiquidValue::Nil);
            }
        }

        let message = value.funcall::<_, _, RString>("inspect", ())?.to_string()?;
        Err(Error::new(
            self.ruby.exception_runtime_error(),
            format!("unsupported Ruby object for Rust Liquid: {}", message),
        ))
    }

    fn convert_array(&mut self, array: RArray, depth: usize) -> Result<LiquidValue, Error> {
        let mut values = Vec::with_capacity(array.len());
        for entry in array.each() {
            let element = entry?;
            values.push(self.convert_inner(element, depth + 1)?);
        }
        Ok(LiquidValue::array(values))
    }

    fn convert_hash(&mut self, hash: RHash, depth: usize) -> Result<LiquidValue, Error> {
        let pairs = hash.funcall::<_, _, Value>("to_a", ())?;
        let array = match RArray::from_value(pairs) {
            Some(arr) => arr,
            None => return Ok(LiquidValue::Nil),
        };

        let mut object = liquid_model::Object::new();
        for entry in array.each() {
            let pair = entry?;
            let pair_array = match RArray::from_value(pair) {
                Some(pair_array) => pair_array,
                None => continue,
            };
            let key_value: Value = match pair_array.entry(0) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let value_value: Value = match pair_array.entry(1) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let key = self.convert_key(key_value)?;
            let converted = self.convert_inner(value_value, depth + 1)?;
            object.insert(key, converted);
        }

        Ok(LiquidValue::Object(object))
    }

    fn convert_drop(&mut self, drop: Value, depth: usize) -> Result<LiquidValue, Error> {
        let methods_value: Value = drop.funcall::<_, _, Value>("liquid_methods", ())?;
        let mut object = liquid_model::Object::new();

        if let Some(methods) = RArray::from_value(methods_value) {
            for entry in methods.each() {
                let method_value = entry?;
                let method_string: Value = method_value.funcall::<_, _, Value>("to_s", ())?;
                let method_name = String::try_convert(method_string)?;

                let result = drop.funcall::<_, _, Value>(method_name.as_str(), ())?;
                let converted = self.convert_inner(result, depth + 1)?;
                object.insert(method_name.clone().into(), converted);
            }
        }

        Ok(LiquidValue::Object(object))
    }

    fn convert_key(&self, key: Value) -> Result<KString, Error> {
        if key.is_nil() {
            return Ok(KString::from_ref(""));
        }

        if let Some(symbol) = Symbol::from_value(key) {
            let name = symbol.name()?;
            return Ok(KString::from_string(name.to_string()));
        }

        if let Some(string) = RString::from_value(key) {
            return Ok(KString::from_string(string.to_string()?));
        }

        let fallback = key.funcall::<_, _, RString>("to_s", ())?;
        Ok(KString::from_string(fallback.to_string()?))
    }

    fn with_guard<F>(&mut self, value: Value, func: F) -> Result<Option<LiquidValue>, Error>
    where
        F: FnOnce(&mut Self) -> Result<LiquidValue, Error>,
    {
        let object_id_value = value.funcall::<_, _, Value>("object_id", ())?;
        let object_id = match i64::try_convert(object_id_value) {
            Ok(id) => id,
            Err(_) => return func(self).map(Some),
        };

        if !self.visited.insert(object_id) {
            return Ok(None);
        }

        let result = func(self);
        self.visited.remove(&object_id);
        result.map(Some)
    }
}

#[derive(Clone)]
struct RubyFilterParser {
    name: String,
}

impl RubyFilterParser {
    fn new(name: String) -> Self {
        Self { name }
    }
}

impl FilterReflection for RubyFilterParser {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        ""
    }

    fn positional_parameters(&self) -> &'static [ParameterReflection] {
        EMPTY_PARAMS
    }

    fn keyword_parameters(&self) -> &'static [ParameterReflection] {
        EMPTY_PARAMS
    }
}

impl ParseFilter for RubyFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        let keyword: Vec<(String, Expression)> = arguments
            .keyword
            .by_ref()
            .map(|(name, expr)| (name.to_string(), expr))
            .collect();

        Ok(Box::new(RubyFilter {
            name: self.name.clone(),
            positional,
            keyword,
        }))
    }

    fn reflection(&self) -> &dyn FilterReflection {
        self
    }
}

struct RubyFilter {
    name: String,
    positional: Vec<Expression>,
    keyword: Vec<(String, Expression)>,
}

impl fmt::Debug for RubyFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RubyFilter")
            .field("name", &self.name)
            .finish()
    }
}

impl fmt::Display for RubyFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl Filter for RubyFilter {
    fn evaluate(
        &self,
        input: &dyn liquid::model::ValueView,
        runtime: &dyn Runtime,
    ) -> LiquidResult<LiquidValue> {
        let ruby = ruby_handle().map_err(|err| LiquidError::with_msg(err.to_string()))?;

        let input_value = liquid_value_to_ruby(&ruby, &input.to_value())
            .map_err(|err| LiquidError::with_msg(err.to_string()))?;

        let mut positional_values = Vec::with_capacity(self.positional.len());
        for expr in &self.positional {
            let value = expr.evaluate(runtime)?.into_owned();
            let ruby_value = liquid_value_to_ruby(&ruby, &value)
                .map_err(|err| LiquidError::with_msg(err.to_string()))?;
            positional_values.push(ruby_value);
        }

        let positional_array = ruby.ary_new_capa(positional_values.len());
        for value in positional_values {
            positional_array
                .push(value)
                .map_err(|err| LiquidError::with_msg(err.to_string()))?;
        }
        let positional_value = positional_array.into_value_with(&ruby);

        let keyword_hash = ruby.hash_new();
        for (name, expr) in &self.keyword {
            let value = expr.evaluate(runtime)?.into_owned();
            let ruby_value = liquid_value_to_ruby(&ruby, &value)
                .map_err(|err| LiquidError::with_msg(err.to_string()))?;
            keyword_hash
                .aset(ruby.str_new(name), ruby_value)
                .map_err(|err| LiquidError::with_msg(err.to_string()))?;
        }
        let keyword_value = keyword_hash.into_value_with(&ruby);

        let context = RUBY_FILTER_CONTEXT.with(|cell| cell.borrow().clone());
        let Some(ctx) = context else {
            return Err(LiquidError::with_msg("Ruby filter context unavailable"));
        };

        let rust_module =
            rust_bridge_module(&ruby).map_err(|err| LiquidError::with_msg(err.to_string()))?;

        let result_value: Value = rust_module
            .funcall(
                "apply_liquid_filter",
                (
                    ctx.context,
                    self.name.as_str(),
                    input_value,
                    positional_value,
                    keyword_value,
                ),
            )
            .map_err(|err| LiquidError::with_msg(err.to_string()))?;

        let mut converter = LiquidValueConverter::new(&ruby)
            .map_err(|err| LiquidError::with_msg(err.to_string()))?;
        converter
            .convert(result_value)
            .map_err(|err| LiquidError::with_msg(err.to_string()))
    }
}

fn rust_bridge_module(ruby: &Ruby) -> Result<RModule, Error> {
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    jekyll.const_get("Rust")
}

fn fetch_filter_names(_ruby: &Ruby, rust_module: RModule) -> Result<Vec<String>, Error> {
    let names_value: Value = rust_module.funcall("liquid_filter_names", ())?;

    let mut names = Vec::new();
    if let Some(array) = RArray::from_value(names_value) {
        for entry in array.each() {
            let value = entry?;
            names.push(String::try_convert(value)?);
        }
    }
    Ok(names)
}

fn prepare_filter_context(
    _ruby: &Ruby,
    rust_module: RModule,
    payload: Value,
    info: Value,
) -> Result<Value, Error> {
    rust_module.funcall("prepare_liquid_filter_context", (payload, info))
}

fn fetch_tag_kinds(_ruby: &Ruby, rust_module: RModule) -> Result<(Vec<String>, Vec<String>), Error> {
    let value: Value = rust_module.funcall("liquid_tag_kinds", ())?;
    let mut tags = Vec::new();
    let mut blocks = Vec::new();

    if let Some(hash) = RHash::from_value(value) {
        if let Some(arr) = RArray::from_value(hash.aref("tags")?) {
            for entry in arr.each() {
                let name = String::try_convert(entry?)?;
                tags.push(name);
            }
        }
        if let Some(arr) = RArray::from_value(hash.aref("blocks")?) {
            for entry in arr.each() {
                let name = String::try_convert(entry?)?;
                blocks.push(name);
            }
        }
    }
    Ok((tags, blocks))
}

#[derive(Clone)]
struct RubyTagParser {
    name: String,
}

impl RubyTagParser {
    fn new(name: String) -> Self {
        Self { name }
    }
}

impl TagReflection for RubyTagParser {
    fn tag(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        ""
    }
}

#[derive(Clone)]
struct RubyBlockParser {
    name: String,
}

impl RubyBlockParser {
    fn new(name: String) -> Self {
        Self { name }
    }
}

impl BlockReflection for RubyBlockParser {
    fn start_tag(&self) -> &str {
        &self.name
    }

    fn end_tag(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        ""
    }
}

impl ParseTag for RubyTagParser {
    fn parse(&self, mut arguments: TagTokenIter, _options: &liquid_core::parser::Language) -> LiquidResult<Box<dyn LiquidRenderable>> {
        // Reconstruct markup from tokens
        let mut parts: Vec<String> = Vec::new();
        while let Some(tok) = arguments.next() {
            parts.push(tok.as_str().to_string());
        }
        let markup = parts.join(" ");
        Ok(Box::new(RubyTagRenderable { name: self.name.clone(), markup, body: None }))
    }

    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}

impl ParseBlock for RubyBlockParser {
    fn parse(
        &self,
        mut arguments: TagTokenIter,
        mut block: TagBlock,
        _options: &liquid_core::parser::Language,
    ) -> LiquidResult<Box<dyn LiquidRenderable>> {
        let mut parts: Vec<String> = Vec::new();
        while let Some(tok) = arguments.next() {
            parts.push(tok.as_str().to_string());
        }
        let markup = parts.join(" ");
        // Capture raw body preserving nested blocks of same name
        let body = block.escape_liquid(true).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        // Ensure the iterator is properly closed
        block.assert_empty();
        Ok(Box::new(RubyTagRenderable { name: self.name.clone(), markup, body: Some(body.to_string()) }))
    }

    fn reflection(&self) -> &dyn BlockReflection {
        self
    }
}

#[derive(Debug, Clone)]
struct RubyTagRenderable {
    name: String,
    markup: String,
    body: Option<String>,
}

impl LiquidRenderable for RubyTagRenderable {
    fn render_to(&self, writer: &mut dyn std::io::Write, _runtime: &dyn liquid_core::runtime::Runtime) -> LiquidResult<()> {
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let context = RUBY_FILTER_CONTEXT.with(|cell| cell.borrow().clone());
        let Some(ctx) = context else {
            return Err(LiquidError::with_msg("Ruby tag context unavailable"));
        };
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let body_value = match &self.body {
            Some(s) => ruby.str_new(s).into_value_with(&ruby),
            None => ruby.qnil().into_value_with(&ruby),
        };
        let out: Value = rust_module
            .funcall(
                "apply_liquid_tag",
                (ctx.context, self.name.as_str(), ruby.str_new(&self.markup), body_value),
            )
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let s: RString = out
            .funcall("to_s", ())
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let bytes = s.to_string().map_err(|e| LiquidError::with_msg(e.to_string()))?.into_bytes();
        writer
            .write_all(&bytes)
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        Ok(())
    }
}

pub fn render_template(
    content: &str,
    payload: Value,
    info: Value,
    path: Option<Value>,
) -> Result<String, Error> {
    let ruby = ruby_handle()?;
    let rust_module = rust_bridge_module(&ruby)?;
    let filter_names = fetch_filter_names(&ruby, rust_module)?;
    let context_value = prepare_filter_context(&ruby, rust_module, payload, info)?;

    let mut builder = ParserBuilder::with_stdlib();
    for name in &filter_names {
        builder = builder.filter(RubyFilterParser::new(name.clone()));
    }
    let (tag_names, block_names) = fetch_tag_kinds(&ruby, rust_module)?;
    for name in tag_names {
        builder = builder.tag(RubyTagParser::new(name));
    }
    for name in block_names {
        builder = builder.block(RubyBlockParser::new(name));
    }

    let mut converter = LiquidValueConverter::new(&ruby)?;
    let globals_value = converter.convert(payload)?;

    let globals_object = match globals_value {
        LiquidValue::Object(obj) => obj,
        other => {
            let mut object = liquid_model::Object::new();
            object.insert("page".into(), other);
            object
        }
    };

    let previous = RUBY_FILTER_CONTEXT.with(|cell| {
        let mut slot = cell.borrow_mut();
        slot.replace(RubyFilterContext {
            context: context_value,
        })
    });

    let cache_key = template_cache_key_for(&ruby, content, &filter_names, path.clone())?;
    let template_render = TEMPLATE_CACHE.with(|cell| -> Result<String, Error> {
        let mut cache = cell.borrow_mut();
        if let Some(tpl) = cache.get(&cache_key) {
            return tpl
                .render(&globals_object)
                .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()));
        }

        let parser = builder
            .build()
            .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()))?;
        let tpl = parser
            .parse(content)
            .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()))?;

        // Cap cache size to a modest number to avoid unbounded growth
        const MAX_CACHE_SIZE: usize = 256;
        if cache.len() >= MAX_CACHE_SIZE {
            if let Some(evict_key) = cache.keys().next().cloned() {
                cache.remove(&evict_key);
            }
        }
        let rendered = tpl
            .render(&globals_object)
            .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()))?;
        cache.insert(cache_key, tpl);
        Ok(rendered)
    })?;

    let result = Ok(template_render);

    RUBY_FILTER_CONTEXT.with(|cell| {
        *cell.borrow_mut() = previous;
    });

    result
}

fn template_cache_key(content: &str, filters: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    for f in filters {
        f.hash(&mut hasher);
    }
    hasher.finish()
}

fn template_cache_key_for(
    ruby: &Ruby,
    content: &str,
    filters: &[String],
    path: Option<Value>,
) -> Result<u64, Error> {
    if let Some(p) = path {
        // Key: path + integer mtime + filters; fall back to content if mtime fails.
        let file: Value = ruby.class_object().const_get("File")?;
        let path_s: RString = p.funcall("to_s", ())?;
        let exists: Value = file.funcall("exist?", (path_s.clone(),))?;
        if exists.to_bool() {
            let mtime: Value = file.funcall("mtime", (path_s.clone(),))?;
            let mtime_i: Value = mtime.funcall("to_i", ())?;
            let mtime_int = i64::try_convert(mtime_i)?;
            let mut hasher = DefaultHasher::new();
            path_s.to_string()?.hash(&mut hasher);
            mtime_int.hash(&mut hasher);
            for f in filters {
                f.hash(&mut hasher);
            }
            return Ok(hasher.finish());
        }
    }
    Ok(template_cache_key(content, filters))
}

pub fn clear_template_cache() {
    TEMPLATE_CACHE.with(|cell| cell.borrow_mut().clear());
}
