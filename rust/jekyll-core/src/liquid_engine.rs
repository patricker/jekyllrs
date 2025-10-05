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
                    // Tags whose markup should be preserved as raw and passed to Ruby intact
                    // Use a hex wrapper to keep the Rust parser happy even with quotes/spaces.
                    let needs_raw = matches!(name.as_str(), "post_url" | "include" | "include_relative" | "link");
                    if needs_raw {
                        // raw markup after name, preserve exactly
                        let raw = inner[name.len()..].trim_end_matches(|c: char| c.is_whitespace());
                        let mut rep = String::new();
                        rep.push_str("{"); rep.push('%'); if open_dash { rep.push('-'); }
                        rep.push(' ');
                        rep.push_str(&name);
                        rep.push(' ');
                        rep.push_str("__jekyll_raw_hex:'");
                        rep.push_str(&encode_hex(raw.as_bytes()));
                        rep.push_str("'"); if close_dash { rep.push(' '); rep.push('-'); }
                        rep.push('%'); rep.push('}');
                        out.push_str(&rep);
                    } else if name == "highlight" {
                        // Transform block into a single synthetic tag carrying both markup and body
                        // Find the end of the opening tag
                        let open_end = end;
                        // Naive search for endhighlight (no nested highlight blocks expected)
                        let mut search_pos = open_end;
                        let mut body_end = None;
                        while let Some(tag_pos_rel) = content[search_pos..].find("{%") {
                            let tag_pos = search_pos + tag_pos_rel;
                            // check if this is endhighlight
                            let after = &content[tag_pos + 2..];
                            let after_trim = after.trim_start();
                            if after_trim.starts_with("endhighlight") {
                                // find end of this tag
                                if let Some(close_rel) = content[tag_pos..].find("%}") {
                                    let close = tag_pos + close_rel + 2;
                                    body_end = Some((tag_pos, close));
                                    break;
                                }
                            }
                            search_pos = tag_pos + 2;
                        }
                        if let Some((close_start, close_end)) = body_end {
                            let raw = inner[name.len()..].trim_end_matches(|c: char| c.is_whitespace());
                            let body = &content[open_end..close_start];
                            let payload = format!("m:{}|b:{}", encode_hex(raw.as_bytes()), encode_hex(body.as_bytes()));
                            let mut rep = String::new();
                            rep.push_str("{"); rep.push('%'); if open_dash { rep.push('-'); }
                            rep.push_str(" jekyll_highlight_block ");
                            rep.push_str("__jekyll_raw_hex:'");
                            rep.push_str(&encode_hex(payload.as_bytes()));
                            rep.push_str("'"); if close_dash { rep.push(' '); rep.push('-'); }
                            rep.push('%'); rep.push('}');
                            out.push_str(&rep);
                            i = close_end;
                            continue;
                        } else {
                            // Fallback: copy as-is; parser will error if unmatched
                            out.push_str(tag_text);
                        }
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
    let trimmed = markup.trim_start();
    // Hex-encoded raw wrapper (preferred)
    let prefix_hex = "__jekyll_raw_hex:'";
    if trimmed.starts_with(prefix_hex) {
        let mut bytes: Vec<u8> = Vec::new();
        let mut it = trimmed[prefix_hex.len()..].chars();
        while let (Some(h), Some(l)) = (it.next(), it.next()) {
            if l == '\'' { break; }
            if h == '\'' { break; }
            let hv = h.to_digit(16)? as u8;
            let lv = l.to_digit(16)? as u8;
            bytes.push((hv << 4) | lv);
        }
        let s = String::from_utf8_lossy(&bytes).to_string();
        return Some(s);
    }
    // Legacy single-quoted raw wrapper with backslash escapes
    let prefix = "__jekyll_raw:'";
    if !trimmed.starts_with(prefix) { return None; }
    let mut s = String::new();
    let mut chars = trimmed[prefix.len()..].chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => if let Some(next) = chars.next() { s.push(next); },
            '\'' => break,
            _ => s.push(ch),
        }
    }
    Some(s)
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

fn decode_hex(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut chars = s.chars();
    while let (Some(h), Some(l)) = (chars.next(), chars.next()) {
        let hv = h.to_digit(16).unwrap_or(0) as u8;
        let lv = l.to_digit(16).unwrap_or(0) as u8;
        out.push((hv << 4) | lv);
    }
    out
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
        LiquidValue::Nil => Ok(ruby.qnil().into_value_with(ruby)),
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
            ruby.qtrue().into_value_with(ruby)
        } else {
            ruby.qfalse().into_value_with(ruby)
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
        // Do not collapse CollectionDrop to its docs array. Preserve it as an object so
        // property lookups like site.collections[0]["label"] behave correctly.

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

        // Detect document-like drops to avoid materializing expensive properties (e.g., output/content)
        let is_document_like_drop_method_phase = {
            let jekyll_mod = self
                .ruby
                .class_object()
                .const_get::<_, RModule>("Jekyll")
                .ok();
            let drops_mod = jekyll_mod
                .as_ref()
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

        let mut object = liquid_model::Object::new();
        if let Some(methods_value) = methods_value {
            if let Some(methods) = RArray::from_value(methods_value) {
                for entry in methods.each() {
                    let method_value = entry?;
                    let method_string: Value = method_value.funcall::<_, _, Value>("to_s", ())?;
                    let method_name = String::try_convert(method_string)?;

                    // Avoid expensive materialization on document-like drops. Allow 'output'
                    // (already computed during render), but skip 'content' and 'to_s'.
                    if is_document_like_drop_method_phase {
                        if method_name == "content" || method_name == "to_s" {
                            continue;
                        }
                    }

                    // Handle `excerpt` specially. Only materialize for posts to avoid
                    // expensive nested rendering on arbitrary collection documents.
                    if method_name == "excerpt" {
                        // Determine if this drop wraps a post (collection label == "posts")
                        let is_post = (|| -> Result<bool, Error> {
                            let obj: Value = drop.funcall("instance_variable_get", (self.ruby.str_new("@obj"),))?;
                            let coll: Value = obj.funcall("collection", ())?;
                            if coll.is_nil() { return Ok(false); }
                            let label_val: Value = coll.funcall("label", ())?;
                            if let Some(s) = RString::from_value(label_val) { return Ok(s.to_string()?.eq_ignore_ascii_case("posts")); }
                            Ok(false)
                        })().unwrap_or(false);

                        if is_post {
                            match drop.funcall::<_, _, Value>(method_name.as_str(), ()) {
                                Ok(ex) => {
                                    if let Ok(s) = ex.funcall::<_, _, RString>("to_s", ()) {
                                        if let Ok(st) = s.to_string() {
                                            object.insert(method_name.clone().into(), LiquidValue::scalar(st));
                                            continue;
                                        }
                                    }
                                    object.insert(method_name.clone().into(), LiquidValue::Nil);
                                    continue;
                                }
                                Err(_) => {
                                    object.insert(method_name.clone().into(), LiquidValue::Nil);
                                    continue;
                                }
                            }
                        } else {
                            // Do not compute excerpts for non-post documents during payload projection
                            object.insert(method_name.clone().into(), LiquidValue::Nil);
                            continue;
                        }
                    }

                    let result = drop.funcall::<_, _, Value>(method_name.as_str(), ())?;
                    let converted = self.convert_inner(result, depth + 1)?;
                    object.insert(method_name.clone().into(), converted);
                }
            }
        }

        // For DocumentDrop, ensure commonly used attributes like `relative_path` are present
        let jekyll_mod = self
            .ruby
            .class_object()
            .const_get::<_, RModule>("Jekyll")
            .ok();
        let drops_mod = jekyll_mod
            .as_ref()
            .and_then(|j| j.const_get::<_, RModule>("Drops").ok());
        let document_drop_class = drops_mod
            .as_ref()
            .and_then(|d| d.const_get::<_, RClass>("DocumentDrop").ok());
        if let Some(doc_cls) = document_drop_class {
            if drop.is_kind_of(doc_cls) {
                let rel_key = KString::from_ref("relative_path");
                if !object.contains_key(&rel_key) {
                    let rel_val = drop.funcall::<_, _, Value>("instance_variable_get", (self.ruby.str_new("@obj"),))
                        .ok()
                        .and_then(|obj| if obj.respond_to("relative_path", false).ok()? { obj.funcall("relative_path", ()).ok() } else { None })
                        .or_else(|| if drop.respond_to("relative_path", false).ok()? { drop.funcall("relative_path", ()).ok() } else { None });
                    if let Some(rv) = rel_val {
                        let converted = self.convert_inner(rv, depth + 1)?;
                        object.insert(rel_key, converted);
                    }
                }
            }
        }

        // For any other Drop types, if a `relative_path` method exists (on drop or @obj), surface it.
        let rel_key = KString::from_ref("relative_path");
        if !object.contains_key(&rel_key) {
            let rel_val = if drop.respond_to("relative_path", false)? {
                Some(drop.funcall::<_, _, Value>("relative_path", ())?)
            } else {
                let obj = drop.funcall::<_, _, Value>("instance_variable_get", (self.ruby.str_new("@obj"),))?;
                if obj.respond_to("relative_path", false)? {
                    Some(obj.funcall::<_, _, Value>("relative_path", ())?)
                } else {
                    None
                }
            };
            if let Some(rv) = rel_val {
                let converted = self.convert_inner(rv, depth + 1)?;
                object.insert(rel_key, converted);
            }
        }

        // Avoid calling generic `to_h` on document-like drops to prevent recursion/huge graphs.
        // For other drops (e.g., JekyllDrop), `to_h` exposes a stable, small set of keys.
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

        // Provide a generic alias: if an object exposes `path` but not `relative_path`,
        // mirror `path` as `relative_path` for Liquid templates expecting it.
        {
            let pkey = KString::from_ref("path");
            let rkey = KString::from_ref("relative_path");
            if object.contains_key(&pkey) && !object.contains_key(&rkey) {
                if let Some(pval) = object.get(&pkey).cloned() {
                    object.insert(rkey, pval);
                }
            }
        }

        // Special-case SiteDrop: expose config keys and defaults
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
                // Populate collection label accessors: site.<label> -> docs array, and ensure
                // `site["label"]` access works in Liquid by projecting these into the object.
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

// Native filters that bridge to Rust fast paths on the Jekyll::Rust module

#[derive(Clone)]
struct WhereFilterParser;
impl FilterReflection for WhereFilterParser {
    fn name(&self) -> &str { "where" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for WhereFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        Ok(Box::new(WhereFilter { positional }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct WhereFilter { positional: Vec<Expression> }
impl fmt::Display for WhereFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "where") } }
impl Filter for WhereFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if self.positional.len() < 2 { return Ok(LiquidValue::Nil); }
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;

        let input_value = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let prop_val = self.positional[0].evaluate(runtime)?.into_owned();
        let prop_str = prop_val.to_kstr().to_string();
        let prop_r = ruby.str_new(&prop_str).into_value_with(&ruby);
        let target_val = self.positional[1].evaluate(runtime)?.into_owned();
        let target_r = liquid_value_to_ruby(&ruby, &target_val).map_err(|e| LiquidError::with_msg(e.to_string()))?;

        let result = rust_module
            .funcall::<_, _, Value>("where_filter_fast", (input_value, prop_r, target_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;

        if result.is_nil() {
            // Fallback to Ruby Liquid's where for unsupported cases
            let ctx_opt = RUBY_FILTER_CONTEXT.with(|cell| cell.borrow().clone());
            if let Some(ctx) = ctx_opt {
                let positional_value = {
                    let arr = ruby.ary_new_capa(2);
                    arr.push(ruby.str_new(&prop_str)).map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    arr.push(target_r).map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    arr.into_value_with(&ruby)
                };
                let keyword_value = ruby.hash_new().into_value_with(&ruby);
                let result_value: Value = rust_module
                    .funcall(
                        "apply_liquid_filter",
                        (ctx.context, "where", input_value, positional_value, keyword_value),
                    )
                    .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
                return conv.convert(result_value).map_err(|e| LiquidError::with_msg(e.to_string()));
            }
        }

        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct WhereExpFilterParser;
impl FilterReflection for WhereExpFilterParser {
    fn name(&self) -> &str { "where_exp" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for WhereExpFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        Ok(Box::new(WhereExpFilter { positional }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct WhereExpFilter { positional: Vec<Expression> }
impl fmt::Display for WhereExpFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "where_exp") } }
impl Filter for WhereExpFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if self.positional.len() < 2 { return Ok(LiquidValue::Nil); }
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let var = self.positional[0].evaluate(runtime)?.into_owned().to_kstr().to_string();
        let var_r = ruby.str_new(&var).into_value_with(&ruby);
        let expr = self.positional[1].evaluate(runtime)?.into_owned().to_kstr().to_string();
        let expr_r = ruby.str_new(&expr).into_value_with(&ruby);
        let result = rust_module
            .funcall::<_, _, Value>("where_exp_fast", (input_r, var_r, expr_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;

        if result.is_nil() {
            // Fallback to Ruby Liquid's where_exp for complex expressions
            let ctx_opt = RUBY_FILTER_CONTEXT.with(|cell| cell.borrow().clone());
            if let Some(ctx) = ctx_opt {
                let positional_value = {
                    let arr = ruby.ary_new_capa(2);
                    arr.push(ruby.str_new(&var)).map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    arr.push(ruby.str_new(&expr)).map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    arr.into_value_with(&ruby)
                };
                let keyword_value = ruby.hash_new().into_value_with(&ruby);
                let result_value: Value = rust_module
                    .funcall(
                        "apply_liquid_filter",
                        (ctx.context, "where_exp", input_r, positional_value, keyword_value),
                    )
                    .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
                return conv.convert(result_value).map_err(|e| LiquidError::with_msg(e.to_string()));
            }
        }

        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct SortFilterParser;
impl FilterReflection for SortFilterParser {
    fn name(&self) -> &str { "sort" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for SortFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        let keyword: Vec<(String, Expression)> = arguments.keyword.by_ref().map(|(n,e)| (n.to_string(), e)).collect();
        Ok(Box::new(SortFilter { positional, keyword }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct SortFilter { positional: Vec<Expression>, keyword: Vec<(String, Expression)> }
impl fmt::Display for SortFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "sort") } }
impl Filter for SortFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let prop = if let Some(expr) = self.positional.get(0) {
            expr.evaluate(runtime)?.into_owned().to_kstr().to_string()
        } else { String::new() };
        let prop_r = ruby.str_new(&prop).into_value_with(&ruby);
        let mut nils = if let Some(expr) = self.positional.get(1) {
            expr.evaluate(runtime)?.into_owned().to_kstr().to_string()
        } else {
            "first".to_string()
        };
        for (k, v) in &self.keyword {
            if k.eq_ignore_ascii_case("nils") {
                nils = v.evaluate(runtime)?.into_owned().to_kstr().to_string();
            }
        }
        let nils_r = ruby.str_new(&nils).into_value_with(&ruby);
        let result = rust_module
            .funcall::<_, _, Value>("sort_filter_fast", (input_r, prop_r, nils_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct GroupByFilterParser;
impl FilterReflection for GroupByFilterParser {
    fn name(&self) -> &str { "group_by" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for GroupByFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        Ok(Box::new(GroupByFilter { positional }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct GroupByFilter { positional: Vec<Expression> }
impl fmt::Display for GroupByFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "group_by") } }
impl Filter for GroupByFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if self.positional.is_empty() { return Ok(LiquidValue::Nil); }
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let prop = self.positional[0].evaluate(runtime)?.into_owned().to_kstr().to_string();
        let prop_r = ruby.str_new(&prop).into_value_with(&ruby);
        let result = rust_module
            .funcall::<_, _, Value>("group_by_fast", (input_r, prop_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct MapFilterParser;
impl FilterReflection for MapFilterParser {
    fn name(&self) -> &str { "map" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for MapFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        Ok(Box::new(MapFilter { positional }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct MapFilter { positional: Vec<Expression> }
impl fmt::Display for MapFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "map") } }
impl Filter for MapFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if self.positional.is_empty() { return Ok(LiquidValue::Nil); }
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let prop = self.positional[0].evaluate(runtime)?.into_owned().to_kstr().to_string();
        let prop_r = ruby.str_new(&prop).into_value_with(&ruby);
        let result = rust_module
            .funcall::<_, _, Value>("map_filter_fast", (input_r, prop_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct JoinFilterParser;
impl FilterReflection for JoinFilterParser {
    fn name(&self) -> &str { "join" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for JoinFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        Ok(Box::new(JoinFilter { positional }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct JoinFilter { positional: Vec<Expression> }
impl fmt::Display for JoinFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "join") } }
impl Filter for JoinFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        let delim = if let Some(expr) = self.positional.get(0) {
            expr.evaluate(runtime)?.into_owned().to_kstr().to_string()
        } else {
            " ".to_string()
        };
        if let Some(arr) = input.as_array() {
            let mut parts: Vec<String> = Vec::with_capacity(arr.size() as usize);
            for i in 0..arr.size() {
                if let Some(v) = arr.get(i) {
                    parts.push(v.to_kstr().to_string());
                }
            }
            return Ok(LiquidValue::scalar(parts.join(&delim)));
        }
        Ok(LiquidValue::Nil)
    }
}

#[derive(Clone)]
struct FindFilterParser;
impl FilterReflection for FindFilterParser {
    fn name(&self) -> &str { "find" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for FindFilterParser {
    fn parse(&self, mut arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        let positional: Vec<Expression> = arguments.positional.by_ref().collect();
        Ok(Box::new(FindFilter { positional }))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct FindFilter { positional: Vec<Expression> }
impl fmt::Display for FindFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "find") } }
impl Filter for FindFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if self.positional.len() < 2 { return Ok(LiquidValue::Nil); }
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let prop = self.positional[0].evaluate(runtime)?.into_owned().to_kstr().to_string();
        let prop_r = ruby.str_new(&prop).into_value_with(&ruby);
        let target_val = self.positional[1].evaluate(runtime)?.into_owned();
        let target_r = liquid_value_to_ruby(&ruby, &target_val).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let result = rust_module
            .funcall::<_, _, Value>("find_filter_fast", (input_r, prop_r, target_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct AbsoluteUrlFilterParser;
impl FilterReflection for AbsoluteUrlFilterParser {
    fn name(&self) -> &str { "absolute_url" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for AbsoluteUrlFilterParser {
    fn parse(&self, _arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        Ok(Box::new(AbsoluteUrlFilter))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct AbsoluteUrlFilter;
impl fmt::Display for AbsoluteUrlFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "absolute_url") } }
impl Filter for AbsoluteUrlFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, _runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let ctx_opt = RUBY_FILTER_CONTEXT.with(|cell| cell.borrow().clone());
        let Some(ctx) = ctx_opt else { return Err(LiquidError::with_msg("Ruby filter context unavailable")); };
        let regs: Value = ctx.context.funcall("registers", ()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let site_key = ruby.sym_new("site").into_value_with(&ruby);
        let site: Value = regs.funcall("[]", (site_key,)).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let result = rust_module
            .funcall::<_, _, Value>("url_filters_absolute_url", (site, input_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct RelativeUrlFilterParser;
impl FilterReflection for RelativeUrlFilterParser {
    fn name(&self) -> &str { "relative_url" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for RelativeUrlFilterParser {
    fn parse(&self, _arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        Ok(Box::new(RelativeUrlFilter))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct RelativeUrlFilter;
impl fmt::Display for RelativeUrlFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "relative_url") } }
impl Filter for RelativeUrlFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, _runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let ctx_opt = RUBY_FILTER_CONTEXT.with(|cell| cell.borrow().clone());
        let Some(ctx) = ctx_opt else { return Err(LiquidError::with_msg("Ruby filter context unavailable")); };
        let regs: Value = ctx.context.funcall("registers", ()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let site_key = ruby.sym_new("site").into_value_with(&ruby);
        let site: Value = regs.funcall("[]", (site_key,)).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let result = rust_module
            .funcall::<_, _, Value>("url_filters_relative_url", (site, input_r))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct StripIndexFilterParser;
impl FilterReflection for StripIndexFilterParser {
    fn name(&self) -> &str { "strip_index" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for StripIndexFilterParser {
    fn parse(&self, _arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        Ok(Box::new(StripIndexFilter))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct StripIndexFilter;
impl fmt::Display for StripIndexFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "strip_index") } }
impl Filter for StripIndexFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, _runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        let ruby = ruby_handle().map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let rust_module = rust_bridge_module(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let input_r = liquid_value_to_ruby(&ruby, &input.to_value()).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let result = rust_module
            .funcall::<_, _, Value>("url_filters_strip_index", (input_r,))
            .map_err(|e| LiquidError::with_msg(e.to_string()))?;
        let mut conv = LiquidValueConverter::new(&ruby).map_err(|e| LiquidError::with_msg(e.to_string()))?;
        conv.convert(result).map_err(|e| LiquidError::with_msg(e.to_string()))
    }
}

#[derive(Clone)]
struct UniqFilterParser;
impl FilterReflection for UniqFilterParser {
    fn name(&self) -> &str { "uniq" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for UniqFilterParser {
    fn parse(&self, _arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        Ok(Box::new(UniqFilter))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct UniqFilter;
impl fmt::Display for UniqFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "uniq") } }
impl Filter for UniqFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, _runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if let Some(arr) = input.as_array() {
            use std::collections::HashSet;
            let mut seen: HashSet<String> = HashSet::new();
            let mut out: Vec<LiquidValue> = Vec::with_capacity(arr.size() as usize);
            for i in 0..arr.size() {
                if let Some(v) = arr.get(i) {
                    let s = v.to_kstr().to_string();
                    if seen.insert(s) {
                        out.push(v.to_value());
                    }
                }
            }
            return Ok(LiquidValue::array(out));
        }
        Ok(LiquidValue::Nil)
    }
}

#[derive(Clone)]
struct CompactFilterParser;
impl FilterReflection for CompactFilterParser {
    fn name(&self) -> &str { "compact" }
    fn description(&self) -> &str { "" }
    fn positional_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] { EMPTY_PARAMS }
}
impl ParseFilter for CompactFilterParser {
    fn parse(&self, _arguments: FilterArguments) -> LiquidResult<Box<dyn Filter>> {
        Ok(Box::new(CompactFilter))
    }
    fn reflection(&self) -> &dyn FilterReflection { self }
}
#[derive(Debug)]
struct CompactFilter;
impl fmt::Display for CompactFilter { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "compact") } }
impl Filter for CompactFilter {
    fn evaluate(&self, input: &dyn liquid::model::ValueView, _runtime: &dyn Runtime) -> LiquidResult<LiquidValue> {
        if let Some(arr) = input.as_array() {
            let mut out: Vec<LiquidValue> = Vec::new();
            out.reserve(arr.size() as usize);
            for i in 0..arr.size() {
                if let Some(v) = arr.get(i) {
                    if !v.is_nil() {
                        out.push(v.to_value());
                    }
                }
            }
            return Ok(LiquidValue::array(out));
        }
        Ok(LiquidValue::Nil)
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
            // Exclude filters we implement natively below.
            if name == "map"
                || name == "join"
                || name == "where"
                || name == "where_exp"
                || name == "sort"
                || name == "group_by"
                || name == "find"
                || name == "absolute_url"
                || name == "relative_url"
                || name == "strip_index"
                || name == "uniq"
                || name == "compact"
            {
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
        // Reconstruct raw markup by concatenating tokens with minimal spacing,
        // inserting spaces where Ruby tag parsers expect separation (e.g., after quoted values).
        let mut parts: Vec<String> = Vec::new();
        while let Some(tok) = arguments.next() {
            parts.push(tok.as_str().to_string());
        }
        let mut markup = String::new();
        let mut prev_tail = '\0';
        for (i, part) in parts.iter().enumerate() {
            let head = part.chars().next().unwrap_or('\0');
            let tail_needs_sep = prev_tail.is_alphanumeric()
                || matches!(prev_tail, '_' | '}' | '"' | '\'' | ']' | ')');
            let head_is_token_start = head.is_alphanumeric()
                || matches!(head, '_' | '{' | '"' | '\'' | '[' | '(');
            if i > 0 && tail_needs_sep && head_is_token_start {
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
        // Reconstruct raw markup similarly to non-block tags, with spacing rules as above
        let mut parts: Vec<String> = Vec::new();
        while let Some(tok) = arguments.next() {
            parts.push(tok.as_str().to_string());
        }
        let mut markup = String::new();
        let mut prev_tail = '\0';
        for (i, part) in parts.iter().enumerate() {
            let head = part.chars().next().unwrap_or('\0');
            let tail_needs_sep = prev_tail.is_alphanumeric()
                || matches!(prev_tail, '_' | '}' | '"' | '\'' | ']' | ')');
            let head_is_token_start = head.is_alphanumeric()
                || matches!(head, '_' | '{' | '"' | '\'' | '[' | '(');
            if i > 0 && tail_needs_sep && head_is_token_start {
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
        let mut body_value = match &self.body {
            Some(s) => ruby.str_new(s).into_value_with(&ruby),
            None => ruby.qnil().into_value_with(&ruby),
        };
        // Decode any preprocessed raw markup
        let mut markup_to_send = if let Some(raw) = decode_preprocessed_raw_markup(&self.markup) {
            raw
        } else {
            self.markup.clone()
        };
        // Special-case our synthetic highlight block: payload is hex-encoded markup and body
        let mut ruby_tag_name = self.name.as_str();
        if self.name == "jekyll_highlight_block" {
            // payload format: m:<hex>|b:<hex>
            let payload = markup_to_send.clone();
            let mut markup_decoded = String::new();
            let mut body_decoded = String::new();
            if let Some(mpos) = payload.find("m:") {
                if let Some(bpos) = payload[mpos+2..].find("|b:") {
                    let mhex = &payload[mpos+2..mpos+2+bpos];
                    let bhex = &payload[mpos+2+bpos+3..];
                    markup_decoded = String::from_utf8_lossy(&decode_hex(mhex)).to_string();
                    body_decoded = String::from_utf8_lossy(&decode_hex(bhex)).to_string();
                }
            }
            if !markup_decoded.is_empty() {
                markup_to_send = markup_decoded;
                body_value = ruby.str_new(&body_decoded).into_value_with(&ruby);
                ruby_tag_name = "highlight";
            }
        }
        // If markup still contains Liquid output (e.g., {{ slug }}), resolve it against the current runtime
        // Additionally, seed simple unquoted identifiers used as values (e.g., local=var) into the Ruby Context
        let mut locals_hash: Option<Value> = None;
        {
            use liquid_core::model::ScalarCow;
            if let Some(inner) = extract_simple_output_var(&markup_to_send) {
                let path = [ScalarCow::new(inner)];
                if let Ok(val) = _runtime.get(&path) {
                    let ruby_val = liquid_value_to_ruby(&ruby, &val.to_value())
                        .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    let h = ruby.hash_new();
                    h.aset(ruby.str_new(inner), ruby_val)
                        .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    locals_hash = Some(h.into_value_with(&ruby));
                    markup_to_send = val.to_kstr().to_string();
                }
            }
            // Seed simple identifier assignments like key=var (unquoted)
            let mut h_opt = locals_hash.map(|v| RHash::from_value(v)).flatten();
            let mut ensure_hash = || -> Result<RHash, LiquidError> {
                if let Some(h) = h_opt { return Ok(h); }
                let h = ruby.hash_new();
                h_opt = Some(h);
                Ok(h_opt.unwrap())
            };
            let mut i = 0usize;
            let chars: Vec<char> = markup_to_send.chars().collect();
            let mut in_quote: Option<char> = None;
            while i < chars.len() {
                let c = chars[i];
                if let Some(q) = in_quote {
                    if c == q { in_quote = None; }
                    i += 1; continue;
                }
                if c == '\'' || c == '"' { in_quote = Some(c); i += 1; continue; }
                if c == '=' {
                    // find key start (identifier to the left)
                    let k_end = i;
                    let mut k_start = k_end;
                    while k_start > 0 && (chars[k_start-1].is_ascii_alphanumeric() || chars[k_start-1] == '_') {
                        k_start -= 1;
                    }
                    // skip '=' and whitespace
                    let mut v_start = i + 1;
                    while v_start < chars.len() && chars[v_start].is_whitespace() { v_start += 1; }
                    // only handle unquoted simple identifiers
                    let mut v_end = v_start;
                    while v_end < chars.len() && (chars[v_end].is_ascii_alphanumeric() || chars[v_end] == '_' || chars[v_end] == '.') {
                        v_end += 1;
                    }
                    if v_start < v_end {
                        let key: String = chars[k_start..k_end].iter().collect();
                        let value_ident: String = chars[v_start..v_end].iter().collect();
                        // only seed simple identifiers without dot
                        if !key.is_empty() && !value_ident.contains('.') {
                            let path = [ScalarCow::new(value_ident.as_str())];
                            if let Ok(val) = _runtime.get(&path) {
                                let ruby_val = liquid_value_to_ruby(&ruby, &val.to_value())
                                    .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                                let h = ensure_hash()?;
                                h.aset(ruby.str_new(value_ident.as_str()), ruby_val)
                                    .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                            }
                        }
                    }
                }
                i += 1;
            }
            if let Some(h) = h_opt { locals_hash = Some(h.into_value_with(&ruby)); }
            // Also seed simple output variables like {{ name }} that may be used in tag markup concatenations
            let mut h_opt2 = locals_hash.map(|v| RHash::from_value(v)).flatten();
            let mut ensure_hash2 = || -> Result<RHash, LiquidError> {
                if let Some(h) = h_opt2 { return Ok(h); }
                let h = ruby.hash_new();
                h_opt2 = Some(h);
                Ok(h_opt2.unwrap())
            };
            let idents = extract_all_simple_output_vars(&markup_to_send);
            for ident in idents {
                let path = [ScalarCow::new(ident.as_str())];
                if let Ok(val) = _runtime.get(&path) {
                    let ruby_val = liquid_value_to_ruby(&ruby, &val.to_value())
                        .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                    let h = ensure_hash2()?;
                    h.aset(ruby.str_new(ident.as_str()), ruby_val)
                        .map_err(|e| LiquidError::with_msg(e.to_string()))?;
                }
            }
            if let Some(h) = h_opt2 { locals_hash = Some(h.into_value_with(&ruby)); }
        }
        let out: Value = if let Some(locals) = locals_hash {
            rust_module
                .funcall(
                    "apply_liquid_tag_with_locals",
                    (ctx.context, ruby_tag_name, ruby.str_new(&markup_to_send), body_value, locals),
                )
                .map_err(|e| LiquidError::with_msg(e.to_string()))?
        } else {
            rust_module
                .funcall(
                    "apply_liquid_tag",
                    (ctx.context, ruby_tag_name, ruby.str_new(&markup_to_send), body_value),
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

fn extract_all_simple_output_vars(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    while i + 3 < bytes.len() {
        if bytes[i] == '{' && bytes[i + 1] == '{' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_whitespace() { j += 1; }
            let start = j;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == '_') { j += 1; }
            let ident_ok = j > start;
            while j < bytes.len() && bytes[j].is_whitespace() { j += 1; }
            if ident_ok && j + 1 < bytes.len() && bytes[j] == '}' && bytes[j + 1] == '}' {
                let ident: String = bytes[start..(start + (j - start))].iter().collect();
                out.push(ident);
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    out
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
    // Register native Jekyll filters implemented via Rust fast paths
    builder = builder
        .filter(MapFilterParser)
        .filter(JoinFilterParser)
        .filter(WhereFilterParser)
        .filter(WhereExpFilterParser)
        .filter(SortFilterParser)
        .filter(GroupByFilterParser)
        .filter(FindFilterParser)
        .filter(AbsoluteUrlFilterParser)
        .filter(RelativeUrlFilterParser)
        .filter(StripIndexFilterParser)
        .filter(UniqFilterParser)
        .filter(CompactFilterParser);
    let (mut tag_names, mut block_names) = fetch_tag_kinds(&ruby, rust_module)?;
    // Ensure core Jekyll blocks are recognized even if not present in Liquid::Template.tags yet
    if !block_names.iter().any(|n| n.eq_ignore_ascii_case("highlight")) {
        block_names.push("highlight".to_string());
    }
    for name in tag_names.drain(..) {
        // Avoid bridging stdlib tags that mutate runtime (e.g., 'assign')
        if name.eq_ignore_ascii_case("assign") {
            continue;
        }
        builder = builder.tag(RubyTagParser::new(name));
    }
    // Register synthetic helper tags used by the preprocessor
    builder = builder.tag(RubyTagParser::new("jekyll_highlight_block".to_string()));
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
    for name in block_names.drain(..) {
        if stdlib_blocks.iter().any(|s| s.eq_ignore_ascii_case(&name)) {
            continue;
        }
        builder = builder.block(RubyBlockParser::new(name));
    }

    // Determine strictness to configure SafeValue behavior
    let strict_variables = if let Some(hash) = RHash::from_value(info) {
        let key = ruby.str_new("strict_variables").into_value_with(&ruby);
        let val = hash
            .aref(key)
            .unwrap_or_else(|_| ruby.qfalse().into_value_with(&ruby));
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
