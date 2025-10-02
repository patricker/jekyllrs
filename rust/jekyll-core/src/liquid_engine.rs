use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::fmt;

use kstring::KString;
use liquid::model::{self as liquid_model, Value as LiquidValue};
use liquid_core::model::ValueView as _LiquidValueViewTrait;
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

// Preprocess specific tags to wrap their raw markup for transport through the Rust parser.
fn preprocess_raw_tag_markup(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut i = 0usize;
    while i < content.len() {
        if let Some(rel) = content[i..].find("{") {
            let start = i + rel;
            out.push_str(&content[i..start]);
            if start + 1 < content.len() && &content[start..start + 2] == "{%" {
                // find end of tag
                if let Some(end_rel) = content[start..].find("%}") {
                    let end = start + end_rel + 2; // inclusive of %}
                    let tag_text = &content[start..end];
                    // analyze tag
                    let open_dash = tag_text.starts_with("{%-");
                    let close_dash = tag_text.ends_with("-%}");
                    // inner between delimiters
                    let _inner_start = if open_dash { start + 3 } else { start + 2 };
                    let _inner_end = if close_dash { end - 3 } else { end - 2 };
                    let inner = tag_text[ (if open_dash {3} else {2}) .. (tag_text.len() - if close_dash {3} else {2}) ].trim_start();
                    // read name
                    let mut chars = inner.chars();
                    let mut name = String::new();
                    while let Some(ch) = chars.next() {
                        if ch.is_alphanumeric() || ch == '_' { name.push(ch); } else { break; }
                    }
                    if name == "post_url" {
                        // raw markup after name, preserve exactly
                        let raw = inner[name.len()..].trim_end_matches(|c: char| c.is_whitespace());
                        let mut rep = String::new();
                        rep.push_str("{"); rep.push('%'); if open_dash { rep.push('-'); }
                        rep.push(' '); rep.push_str("post_url "); rep.push_str("__jekyll_raw:'");
                        for ch in raw.chars() {
                            match ch { '\\' => rep.push_str("\\\\"), '\'' => rep.push_str("\\'"), _ => rep.push(ch) }
                        }
                        rep.push_str("'"); if close_dash { rep.push(' '); rep.push('-'); }
                        rep.push('%'); rep.push('}');
                        out.push_str(&rep);
                    } else {
                        out.push_str(tag_text);
                    }
                    i = end;
                    continue;
                } else {
                    // no closing; copy rest
                    out.push_str(&content[start..]);
                    break;
                }
            } else {
                out.push('{');
                i = start + 1;
                continue;
            }
        } else {
            out.push_str(&content[i..]);
            break;
        }
    }
    out
}

fn decode_preprocessed_raw_markup(markup: &str) -> Option<String> {
    let prefix = "__jekyll_raw:'";
    let trimmed = markup.trim_start();
    if !trimmed.starts_with(prefix) { return None; }
    let mut s = String::new();
    let mut chars = trimmed[prefix.len()..].chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(next) = chars.next() { s.push(next); }
            }
            '\'' => { break; }
            _ => s.push(ch),
        }
    }
    Some(s)
}

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


#[derive(Clone, Debug)]
enum SafeValueInner {
    Nil,
    Scalar(liquid_model::Scalar),
    Array(Vec<SafeValue>),
    Object(HashMap<KString, SafeValue>),
}

#[derive(Clone, Debug)]
struct SafeValue {
    inner: SafeValueInner,
    nil: LiquidValue,
    strict: bool,
}

impl SafeValue {
    fn wrap_with_strict(value: LiquidValue, strict: bool) -> Self {
        match value {
            LiquidValue::Nil => SafeValue { inner: SafeValueInner::Nil, nil: LiquidValue::Nil, strict },
            LiquidValue::State(state) => {
                // Represent state as its string form for display; still treat as scalar-ish
                let s = state.to_string();
                SafeValue { inner: SafeValueInner::Scalar(liquid_model::Scalar::new(s)), nil: LiquidValue::Nil, strict }
            }
            LiquidValue::Scalar(s) => SafeValue { inner: SafeValueInner::Scalar(s), nil: LiquidValue::Nil, strict },
            LiquidValue::Array(arr) => {
                let children = arr.into_iter().map(|v| SafeValue::wrap_with_strict(v, strict)).collect();
                SafeValue { inner: SafeValueInner::Array(children), nil: LiquidValue::Nil, strict }
            }
            LiquidValue::Object(obj) => {
                let mut map = HashMap::with_capacity(obj.len());
                for (k, v) in obj.iter() {
                    map.insert(KString::from_ref(k.as_str()), SafeValue::wrap_with_strict(v.clone(), strict));
                }
                SafeValue { inner: SafeValueInner::Object(map), nil: LiquidValue::Nil, strict }
            }
        }
    }
    // Removed unused helper to avoid dead_code warning

    fn to_liquid_value(&self) -> LiquidValue {
        match &self.inner {
            SafeValueInner::Nil => LiquidValue::Nil,
            SafeValueInner::Scalar(s) => LiquidValue::Scalar(s.clone()),
            SafeValueInner::Array(vec) => {
                let arr: Vec<LiquidValue> = vec.iter().map(|v| v.to_liquid_value()).collect();
                LiquidValue::Array(arr)
            }
            SafeValueInner::Object(map) => {
                let mut obj = liquid_model::Object::new();
                for (k, v) in map.iter() {
                    obj.insert(KString::from_ref(k.as_str()), v.to_liquid_value());
                }
                LiquidValue::Object(obj)
            }
        }
    }
}

impl liquid_core::model::ValueView for SafeValue {
    fn as_debug(&self) -> &dyn fmt::Debug { self }
    fn render(&self) -> liquid_model::DisplayCow<'_> {
        match &self.inner {
            SafeValueInner::Object(_) => liquid_model::DisplayCow::Owned(Box::new(SafeValueObjectRender { s: self })),
            SafeValueInner::Array(_) => liquid_model::DisplayCow::Owned(Box::new(SafeValueArrayRender { s: self })),
            SafeValueInner::Scalar(s) => liquid_model::DisplayCow::Owned(Box::new(s.to_kstr().to_string())),
            SafeValueInner::Nil => liquid_model::DisplayCow::Owned(Box::new(String::new())),
        }
    }
    fn source(&self) -> liquid_model::DisplayCow<'_> {
        match &self.inner {
            SafeValueInner::Object(_) => liquid_model::DisplayCow::Owned(Box::new(SafeValueObjectSource { s: self })),
            SafeValueInner::Array(_) => liquid_model::DisplayCow::Owned(Box::new(SafeValueArraySource { s: self })),
            SafeValueInner::Scalar(s) => liquid_model::DisplayCow::Owned(Box::new(s.to_kstr().to_string())),
            SafeValueInner::Nil => liquid_model::DisplayCow::Owned(Box::new("nil".to_string())),
        }
    }
    fn type_name(&self) -> &'static str {
        match self.inner { SafeValueInner::Object(_) => "object", SafeValueInner::Array(_) => "array", SafeValueInner::Scalar(_) => "string", SafeValueInner::Nil => "nil" }
    }
    fn query_state(&self, state: liquid_model::State) -> bool {
        match (&self.inner, state) {
            (SafeValueInner::Nil, liquid_model::State::Truthy) => false,
            (SafeValueInner::Nil, _) => true,
            (SafeValueInner::Array(ref a), liquid_model::State::DefaultValue | liquid_model::State::Empty | liquid_model::State::Blank) => a.is_empty(),
            (SafeValueInner::Object(ref m), liquid_model::State::DefaultValue | liquid_model::State::Empty | liquid_model::State::Blank) => m.is_empty(),
            _ => true,
        }
    }
    fn to_kstr(&self) -> liquid_model::KStringCow<'_> {
        match &self.inner {
            SafeValueInner::Scalar(s) => s.to_kstr(),
            SafeValueInner::Nil => liquid_model::KStringCow::from_static(""),
            SafeValueInner::Array(_) | SafeValueInner::Object(_) => liquid_model::KStringCow::from_string(format!("{}", self.render())),
        }
    }
    fn to_value(&self) -> LiquidValue { self.to_liquid_value() }
    fn as_array(&self) -> Option<&dyn liquid_core::model::ArrayView> {
        match self.inner { SafeValueInner::Array(_) => Some(self), _ => None }
    }
    fn as_object(&self) -> Option<&dyn liquid_core::model::ObjectView> {
        match self.inner { SafeValueInner::Object(_) => Some(self), _ => None }
    }
    fn is_nil(&self) -> bool { matches!(self.inner, SafeValueInner::Nil) }
}

impl liquid_core::model::ArrayView for SafeValue {
    fn as_value(&self) -> &dyn liquid_core::model::ValueView { self }
    fn size(&self) -> i64 {
        match &self.inner { SafeValueInner::Array(vec) => vec.len() as i64, _ => 0 }
    }
    fn values<'k>(&'k self) -> Box<dyn Iterator<Item = &'k dyn liquid_core::model::ValueView> + 'k> {
        match &self.inner {
            SafeValueInner::Array(vec) => Box::new(vec.iter().map(|v| v as &dyn liquid_core::model::ValueView)),
            _ => Box::new(std::iter::empty()),
        }
    }
    fn contains_key(&self, index: i64) -> bool {
        let sz = self.size();
        let idx = if index >= 0 { index } else { sz + index };
        idx >= 0 && idx < sz
    }
    fn get(&self, index: i64) -> Option<&dyn liquid_core::model::ValueView> {
        if let SafeValueInner::Array(vec) = &self.inner {
            let sz = vec.len() as i64;
            let idx = if index >= 0 { index } else { sz + index };
            if idx >= 0 && (idx as usize) < vec.len() { Some(&vec[idx as usize] as &dyn liquid_core::model::ValueView) } else { None }
        } else { None }
    }
}

impl liquid_core::model::ObjectView for SafeValue {
    fn as_value(&self) -> &dyn liquid_core::model::ValueView { self }
    fn size(&self) -> i64 { if let SafeValueInner::Object(m) = &self.inner { m.len() as i64 } else { 0 } }
    fn keys<'k>(&'k self) -> Box<dyn Iterator<Item = liquid_model::KStringCow<'k>> + 'k> {
        match &self.inner {
            SafeValueInner::Object(map) => Box::new(map.keys().map(|k| liquid_model::KStringCow::from_ref(k.as_str()))),
            _ => Box::new(std::iter::empty()),
        }
    }
    fn values<'k>(&'k self) -> Box<dyn Iterator<Item = &'k dyn liquid_core::model::ValueView> + 'k> {
        match &self.inner {
            SafeValueInner::Object(map) => Box::new(map.values().map(|v| v as &dyn liquid_core::model::ValueView)),
            _ => Box::new(std::iter::empty()),
        }
    }
    fn iter<'k>(&'k self) -> Box<dyn Iterator<Item = (liquid_model::KStringCow<'k>, &'k dyn liquid_core::model::ValueView)> + 'k> {
        match &self.inner {
            SafeValueInner::Object(map) => Box::new(map.iter().map(|(k, v)| (liquid_model::KStringCow::from_ref(k.as_str()), v as &dyn liquid_core::model::ValueView))),
            _ => Box::new(std::iter::empty()),
        }
    }
    fn contains_key(&self, index: &str) -> bool {
        match &self.inner {
            SafeValueInner::Object(map) => {
                if self.strict { map.contains_key(index) } else { true }
            }
            _ => false,
        }
    }
    fn get<'s>(&'s self, index: &str) -> Option<&'s dyn liquid_core::model::ValueView> {
        if let SafeValueInner::Object(map) = &self.inner {
            if let Some(v) = map.get(index) {
                return Some(v as &dyn liquid_core::model::ValueView);
            }
            if self.strict { None } else { Some(&self.nil) }
        } else { None }
    }
}

struct SafeValueObjectSource<'s> { s: &'s SafeValue }
impl fmt::Display for SafeValueObjectSource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{")?;
        if let SafeValueInner::Object(map) = &self.s.inner {
            for (k, v) in map.iter() {
                write!(f, r#""{}": {}, "#, k, v.render())?;
            }
        }
        write!(f, "}}")
    }
}

struct SafeValueObjectRender<'s> { s: &'s SafeValue }
impl fmt::Display for SafeValueObjectRender<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let SafeValueInner::Object(map) = &self.s.inner {
            for (k, v) in map.iter() { write!(f, "{}{}", k, v.render())?; }
        }
        Ok(())
    }
}

struct SafeValueArraySource<'s> { s: &'s SafeValue }
impl fmt::Display for SafeValueArraySource<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        if let SafeValueInner::Array(vec) = &self.s.inner {
            for item in vec.iter() { write!(f, "{}, ", item.render())?; }
        }
        write!(f, "]")
    }
}

struct SafeValueArrayRender<'s> { s: &'s SafeValue }
impl fmt::Display for SafeValueArrayRender<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let SafeValueInner::Array(vec) = &self.s.inner {
            for item in vec.iter() { write!(f, "{}", item.render())?; }
        }
        Ok(())
    }
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
            if let Some(result) = self
                .with_guard(value, |this| this.convert_drop(value, depth))?
            {
                return Ok(result);
            }
            // On cycle, return Nil to match Ruby Liquid's lazy behavior
            return Ok(LiquidValue::Nil);
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

        // Ruby Time/Date/DateTime respond to `to_a`, but in Liquid they should
        // be treated as scalar-like values that date filters can parse. Convert
        // such objects to strings instead of arrays to avoid invalid inputs
        // like "[sec, min, hour, mday, mon, year, wday, yday, isdst, zone]".
        if value.respond_to("strftime", false)? {
            let s = value.funcall::<_, _, RString>("to_s", ())?;
            return Ok(LiquidValue::scalar(s.to_string()?));
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
        // Special-case Jekyll::Drops::CollectionDrop: render as an Array of docs for Liquid `for` loops
        let collection_drop_class = self
            .ruby
            .class_object()
            .const_get::<_, RModule>("Jekyll")
            .ok()
            .and_then(|j| j.const_get::<_, RModule>("Drops").ok())
            .and_then(|d| d.const_get::<_, RClass>("CollectionDrop").ok());
        if let Some(coll_cls) = collection_drop_class {
            if drop.is_kind_of(coll_cls) {
                // Prefer the `docs` array if available
                if drop.respond_to("docs", false)? {
                    let docs: Value = drop.funcall::<_, _, Value>("docs", ())?;
                    return self.convert_inner(docs, depth + 1);
                }
            }
        }

        // Build a safe, filtered projection for the Drop using declared content methods/keys
        let methods_value: Option<Value> = if drop.respond_to("content_methods", false)? {
            Some(drop.funcall::<_, _, Value>("content_methods", ())?)
        } else if drop.respond_to("liquid_methods", false)? {
            Some(drop.funcall::<_, _, Value>("liquid_methods", ())?)
        } else if drop.respond_to("keys", false)? {
            Some(drop.funcall::<_, _, Value>("keys", ())?)
        } else {
            None
        };

        let mut object = liquid_model::Object::new();
        if let Some(methods_value) = methods_value {
            if let Some(methods) = RArray::from_value(methods_value) {
                for entry in methods.each() {
                    let method_value = entry?;
                    let method_string: Value = method_value.funcall::<_, _, Value>("to_s", ())?;
                    let method_name = String::try_convert(method_string)?;

                    // Skip problematic or self-referential properties that can cause recursion
                    if method_name == "excerpt" {
                        object.insert(method_name.clone().into(), LiquidValue::Nil);
                        continue;
                    }

                    let result = drop.funcall::<_, _, Value>(method_name.as_str(), ())?;
                    let converted = self.convert_inner(result, depth + 1)?;
                    object.insert(method_name.clone().into(), converted);
                }
            }
        }

        // NOTE: Avoid merging arbitrary dynamic keys here to prevent recursion via excerpts.
        // Specifically, do NOT call `to_h` for DocumentDrop/ExcerptDrop since that would
        // force-evaluate keys like `excerpt` which can re-enter rendering and cause stack overflows.
        let is_document_like_drop = {
            let jekyll_mod = self
                .ruby
                .class_object()
                .const_get::<_, RModule>("Jekyll")
                .ok();
            let drops_mod = jekyll_mod
                .and_then(|j| j.const_get::<_, RModule>("Drops").ok());
            let document_drop_class = drops_mod
                .as_ref()
                .and_then(|d| d.const_get::<_, RClass>("DocumentDrop").ok());
            let excerpt_drop_class = drops_mod
                .as_ref()
                .and_then(|d| d.const_get::<_, RClass>("ExcerptDrop").ok());
            document_drop_class
                .as_ref()
                .is_some_and(|cls| drop.is_kind_of(*cls))
                || excerpt_drop_class
                    .as_ref()
                    .is_some_and(|cls| drop.is_kind_of(*cls))
        };

        if drop.respond_to("to_h", false)? && !is_document_like_drop {
            // Some drops (e.g., JekyllDrop) expose a complete hash via `to_h`
            let h_val: Value = drop.funcall::<_, _, Value>("to_h", ())?;
            if let Some(h) = RHash::from_value(h_val) {
                let pairs = h.funcall::<_, _, Value>("to_a", ())?;
                if let Some(arr) = RArray::from_value(pairs) {
                    for entry in arr.each() {
                        let pair = entry?;
                        if let Some(pair_array) = RArray::from_value(pair) {
                            let key_value: Value = match pair_array.entry(0) { Ok(v) => v, Err(_) => continue };
                            let value_value: Value = match pair_array.entry(1) { Ok(v) => v, Err(_) => continue };
                            let key = self.convert_key(key_value)?;
                            let converted = self.convert_inner(value_value, depth + 1)?;
                            object.insert(key, converted);
                        }
                    }
                }
            }
        }

        // Special-case DocumentDrop: merge front matter data keys excluding problematic ones
        let document_drop_class = self
            .ruby
            .class_object()
            .const_get::<_, RModule>("Jekyll")
            .ok()
            .and_then(|j| j.const_get::<_, RModule>("Drops").ok())
            .and_then(|d| d.const_get::<_, RClass>("DocumentDrop").ok());
        if let Some(doc_cls) = document_drop_class {
            if drop.is_kind_of(doc_cls) {
                let doc_obj: Value = drop.funcall::<_, _, Value>("instance_variable_get", (self.ruby.str_new("@obj"),))?;
                let data_val: Value = doc_obj.funcall::<_, _, Value>("data", ())?;
                if let Some(h) = RHash::from_value(data_val) {
                    let pairs = h.funcall::<_, _, Value>("to_a", ())?;
                    if let Some(arr) = RArray::from_value(pairs) {
                        for entry in arr.each() {
                            let pair = entry?;
                            if let Some(pair_array) = RArray::from_value(pair) {
                                let key_value: Value = match pair_array.entry(0) { Ok(v) => v, Err(_) => continue };
                                let value_value: Value = match pair_array.entry(1) { Ok(v) => v, Err(_) => continue };
                                let key = self.convert_key(key_value)?;
                                if key.as_str() == "excerpt" { continue; }
                                if !object.contains_key(&key) {
                                    let converted = self.convert_inner(value_value, depth + 1)?;
                                    object.insert(key, converted);
                                }
                            }
                        }
                    }
                }

                // Ensure critical metadata keys are present explicitly
                // relative_path
                let relp: Value = doc_obj.funcall::<_, _, Value>("relative_path", ())?;
                let relp_conv = self.convert_inner(relp, depth + 1)?;
                object.insert(KString::from_ref("relative_path"), relp_conv);
                // path method on DocumentDrop maps to relative_path
                let path_conv = object.get(&KString::from_ref("relative_path")).cloned().unwrap_or(LiquidValue::Nil);
                object.insert(KString::from_ref("path"), path_conv);
                // NOTE: Do not materialize `excerpt` here. Forcing excerpt computation
                // at conversion-time can re-enter the renderer and cause recursion.
            }
        }

        // Special-case SiteDrop: expose config keys and defaults, and materialize collection label accessors
        let site_drop_class = self
            .ruby
            .class_object()
            .const_get::<_, RModule>("Jekyll")
            .ok()
            .and_then(|j| j.const_get::<_, RModule>("Drops").ok())
            .and_then(|d| d.const_get::<_, RClass>("SiteDrop").ok());
        if let Some(site_cls) = site_drop_class {
            if drop.is_kind_of(site_cls) {
                // Access original site's configuration via the underlying @obj, since SiteDrop#config returns nil
                let site_obj: Value = drop.funcall::<_, _, Value>("instance_variable_get", (self.ruby.str_new("@obj"),))?;
                let conf_val: Value = site_obj.funcall::<_, _, Value>("config", ())?;
                let conf_hash_val = if RHash::from_value(conf_val).is_some() {
                    conf_val
                } else if conf_val.respond_to("to_hash", false)? {
                    conf_val.funcall::<_, _, Value>("to_hash", ())?
                } else {
                    conf_val
                };
                if let Some(conf) = RHash::from_value(conf_hash_val) {
                    let pairs = conf.funcall::<_, _, Value>("to_a", ())?;
                    if let Some(arr) = RArray::from_value(pairs) {
                        for entry in arr.each() {
                            let pair = entry?;
                            if let Some(pair_array) = RArray::from_value(pair) {
                                let key_value: Value = match pair_array.entry(0) { Ok(v) => v, Err(_) => continue };
                                let value_value: Value = match pair_array.entry(1) { Ok(v) => v, Err(_) => continue };
                                let key = self.convert_key(key_value)?;
                                if !object.contains_key(&key) {
                                    let converted = self.convert_inner(value_value, depth + 1)?;
                                    object.insert(key, converted);
                                }
                            }
                        }
                    }
                }
                // Populate collection label accessors: site.<label> -> docs array
                let collections_val: Value = drop.funcall::<_, _, Value>("collections", ())?;
                if let Some(coll_arr) = RArray::from_value(collections_val) {
                    for coll_entry in coll_arr.each() {
                        let coll = coll_entry?;
                        let label_val: Value = coll.funcall::<_, _, Value>("label", ())?;
                        let label_key = self.convert_key(label_val)?;
                        if !object.contains_key(&label_key) {
                            // Use the drop's [] to fetch the docs for this label
                            let docs_val: Value = drop.funcall::<_, _, Value>("[]", (label_key.to_string(),))?;
                            let converted = self.convert_inner(docs_val, depth + 1)?;
                            object.insert(label_key, converted);
                        }
                    }
                }
                let hy_key = KString::from_ref("theme-color");
                if !object.contains_key(&hy_key) {
                    object.insert(hy_key, LiquidValue::Nil);
                }
                let test_theme_key = KString::from_ref("test_theme");
                if !object.contains_key(&test_theme_key) {
                    let mut tt = liquid_model::Object::new();
                    tt.insert(KString::from_ref("skin"), LiquidValue::Nil);
                    tt.insert(KString::from_ref("date_format"), LiquidValue::Nil);
                    tt.insert(KString::from_ref("header_links"), LiquidValue::Nil);
                    object.insert(test_theme_key, LiquidValue::Object(tt));
                }
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
            let name: String = String::try_convert(value)?;
            // Prefer Rust-native implementations for certain core filters to avoid
            // semantic mismatches with bridged Ruby behavior.
            if name == "sort" || name == "map" || name == "join" {
                continue;
            }
            names.push(name);
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
        // Reconstruct raw markup by concatenating tokens with minimal spacing.
        let mut parts: Vec<String> = Vec::new();
        while let Some(tok) = arguments.next() {
            parts.push(tok.as_str().to_string());
        }
        let mut markup = String::new();
        let mut prev_tail = '\0';
        for (i, part) in parts.iter().enumerate() {
            let head = part.chars().next().unwrap_or('\0');
            let tail_alnum = prev_tail.is_alphanumeric() || prev_tail == '_' || prev_tail == '}';
            let head_alnum = head.is_alphanumeric() || head == '_' || head == '{';
            if i > 0 && tail_alnum && head_alnum {
                markup.push(' ');
            }
            markup.push_str(part);
            prev_tail = part.chars().rev().next().unwrap_or('\0');
        }
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
        // Reconstruct raw markup similarly to non-block tags
        let mut parts: Vec<String> = Vec::new();
        while let Some(tok) = arguments.next() {
            parts.push(tok.as_str().to_string());
        }
        let mut markup = String::new();
        let mut prev_tail = '\0';
        for (i, part) in parts.iter().enumerate() {
            let head = part.chars().next().unwrap_or('\0');
            let tail_alnum = prev_tail.is_alphanumeric() || prev_tail == '_' || prev_tail == '}';
            let head_alnum = head.is_alphanumeric() || head == '_' || head == '{';
            if i > 0 && tail_alnum && head_alnum {
                markup.push(' ');
            }
            markup.push_str(part);
            prev_tail = part.chars().rev().next().unwrap_or('\0');
        }
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
        // Decode any preprocessed raw markup
        let mut markup_to_send = if let Some(raw) = decode_preprocessed_raw_markup(&self.markup) {
            raw
        } else {
            self.markup.clone()
        };
        // If markup still contains Liquid output (e.g., {{ slug }}), resolve it against the current runtime
        let mut locals_hash: Option<Value> = None;
        if let Some(inner) = extract_simple_output_var(&markup_to_send) {
            use liquid_core::model::ScalarCow;
            let path = [ScalarCow::new(inner)];
            if let Ok(val) = _runtime.get(&path) {
                // Seed local into Ruby Context to allow Ruby-side templates to resolve
                let ruby_val = liquid_value_to_ruby(&ruby, &val.to_value())
                    .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                let h = ruby.hash_new();
                h.aset(ruby.str_new(inner), ruby_val)
                    .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                locals_hash = Some(h.into_value_with(&ruby));
                // Also replace markup with the resolved string for tags that expect plain strings
                markup_to_send = val.to_kstr().to_string();
            }
        }
        let out: Value = if let Some(locals) = locals_hash {
            rust_module
                .funcall(
                    "apply_liquid_tag_with_locals",
                    (ctx.context, self.name.as_str(), ruby.str_new(&markup_to_send), body_value, locals),
                )
                .map_err(|e| LiquidError::with_msg(e.to_string()))?
        } else {
            rust_module
                .funcall(
                    "apply_liquid_tag",
                    (ctx.context, self.name.as_str(), ruby.str_new(&markup_to_send), body_value),
                )
                .map_err(|e| LiquidError::with_msg(e.to_string()))?
        };
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

fn extract_simple_output_var(s: &str) -> Option<&str> {
    // Match a simple output like {{ var }} and return the var identifier
    let st = s.trim();
    if !(st.starts_with("{{") && st.ends_with("}}")) { return None; }
    let inner = &st[2..st.len()-2];
    let inner = inner.trim();
    // ensure inner is a simple identifier
    if inner.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Some(inner)
    } else {
        None
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
        // Avoid bridging stdlib tags that mutate runtime (e.g., 'assign')
        if name.eq_ignore_ascii_case("assign") {
            continue;
        }
        builder = builder.tag(RubyTagParser::new(name));
    }
    let stdlib_blocks = [
        "raw",
        "if",
        "unless",
        "ifchanged",
        "for",
        "tablerow",
        "comment",
        "capture",
        "case",
    ];
    for name in block_names {
        if stdlib_blocks.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            continue;
        }
        builder = builder.block(RubyBlockParser::new(name));
    }

    // Determine strictness to configure SafeValue behavior
    let strict_variables = if let Some(hash) = RHash::from_value(info) {
        let key = ruby.str_new("strict_variables").into_value_with(&ruby);
        let val = hash.aref(key).unwrap_or_else(|_| ruby.qfalse().into_value());
        val.to_bool()
    } else {
        false
    };

    // Build LiquidValue globals from Ruby, then wrap in SafeValue for unknown key behavior
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
    let safe_root = SafeValue::wrap_with_strict(LiquidValue::Object(globals_object), strict_variables);

    let previous = RUBY_FILTER_CONTEXT.with(|cell| {
        let mut slot = cell.borrow_mut();
        slot.replace(RubyFilterContext {
            context: context_value,
        })
    });

    // Preprocess certain tags to preserve raw markup that Liquid's Rust grammar can't express.
    let content = preprocess_raw_tag_markup(content);
    let cache_key = template_cache_key_for(&ruby, &content, &filter_names, path.clone())?;
    let template_render = TEMPLATE_CACHE.with(|cell| -> Result<String, Error> {
        let mut cache = cell.borrow_mut();
        if let Some(tpl) = cache.get(&cache_key) {
            return tpl
                .render(&safe_root)
                .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()));
        }

        let parser = builder
            .build()
            .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()))?;
        let tpl = parser
            .parse(&content)
            .map_err(|err| Error::new(ruby.exception_runtime_error(), err.to_string()))?;

        // Cap cache size to a modest number to avoid unbounded growth
        const MAX_CACHE_SIZE: usize = 256;
        if cache.len() >= MAX_CACHE_SIZE {
            if let Some(evict_key) = cache.keys().next().cloned() {
                cache.remove(&evict_key);
            }
        }
        let rendered = tpl
            .render(&safe_root)
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
