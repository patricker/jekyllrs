use magnus::{function, prelude::*, Error, RModule, RString, Ruby, Value};
use once_cell::sync::Lazy;
use regex::Regex;
use std::{collections::HashMap, sync::Mutex};

use crate::ruby_utils::{frozen_string, ruby_handle};

static DRIVE_PREFIX_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\w:/").expect("valid drive regex"));
static JOIN_CACHE: Lazy<Mutex<HashMap<(String, String), String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static SANITIZED_CACHE: Lazy<Mutex<HashMap<(String, Option<String>), String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static SLASHED_DIR_CACHE: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("path_manager_join", function!(path_manager_join, 2))?;
    bridge.define_singleton_method(
        "path_manager_sanitized_path",
        function!(path_manager_sanitized_path, 2),
    )
}

fn path_manager_join(base: Option<RString>, item: Option<RString>) -> Result<RString, Error> {
    let ruby = ruby_handle()?;

    let base_str = match base {
        Some(value) => value.to_string()?,
        None => String::new(),
    };
    let item_str = match item {
        Some(value) => value.to_string()?,
        None => String::new(),
    };

    let key = (base_str.clone(), item_str.clone());
    if let Some(cached) = JOIN_CACHE
        .lock()
        .expect("join cache poisoned")
        .get(&key)
        .cloned()
    {
        return Ok(frozen_string(&ruby, &cached));
    }

    let (joined_value, joined_str) = compute_join(&ruby, &base_str, &item_str)?;
    JOIN_CACHE
        .lock()
        .expect("join cache poisoned")
        .insert(key, joined_str);
    Ok(joined_value)
}

fn path_manager_sanitized_path(
    base_directory: RString,
    questionable_path: Option<RString>,
) -> Result<RString, Error> {
    let ruby = ruby_handle()?;
    let base_str = base_directory.to_string()?;

    if questionable_path.is_none() {
        base_directory.freeze();
        SANITIZED_CACHE
            .lock()
            .expect("sanitized cache poisoned")
            .insert((base_str.clone(), None), base_str.clone());
        return Ok(base_directory);
    }

    let question = questionable_path.unwrap();
    let question_str = question.to_string()?;
    let key = (base_str.clone(), Some(question_str.clone()));

    if let Some(cached) = SANITIZED_CACHE
        .lock()
        .expect("sanitized cache poisoned")
        .get(&key)
        .cloned()
    {
        return Ok(frozen_string(&ruby, &cached));
    }

    let (result_value, result_string) = compute_sanitized_path(&ruby, &base_str, question)?;

    SANITIZED_CACHE
        .lock()
        .expect("sanitized cache poisoned")
        .insert(key, result_string);

    Ok(result_value)
}

fn compute_join(ruby: &Ruby, base: &str, item: &str) -> Result<(RString, String), Error> {
    let file = ruby.class_object().const_get::<_, Value>("File")?;
    let base_arg = ruby.str_new(base);
    let item_arg = ruby.str_new(item);
    let joined: RString = file.funcall::<_, _, RString>("join", (base_arg, item_arg))?;
    joined.freeze();
    let joined_str = joined.to_string()?;
    Ok((joined, joined_str))
}

fn compute_sanitized_path(
    ruby: &Ruby,
    base_directory: &str,
    questionable_path: RString,
) -> Result<(RString, String), Error> {
    let file = ruby.class_object().const_get::<_, Value>("File")?;

    let starts_with_tilde: bool = questionable_path.funcall::<_, _, bool>("start_with?", ("~",))?;

    let clean = if starts_with_tilde {
        let dup: RString = questionable_path.funcall::<_, _, RString>("dup", ())?;
        let _: Value = dup.funcall::<_, _, Value>("insert", (0, "/"))?;
        dup
    } else {
        questionable_path
    };

    let clean = file.funcall::<_, _, RString>("expand_path", (clean, "/"))?;
    let base_value = ruby.str_new(base_directory);
    if clean.funcall::<_, _, bool>("==", (base_value,))? {
        clean.freeze();
        let clean_str = clean.to_string()?;
        return Ok((clean, clean_str));
    }

    let _: Value = clean.funcall::<_, _, Value>("squeeze!", ("/",))?;
    let clean_str = clean.to_string()?;

    let slashed_dir = slashed_dir_cache(base_directory);
    if clean_str.starts_with(&slashed_dir) {
        clean.freeze();
        return Ok((clean, clean_str));
    }

    let adjusted = DRIVE_PREFIX_RE.replace(&clean_str, "/").into_owned();
    compute_join(ruby, base_directory, &adjusted)
}

fn slashed_dir_cache(base: &str) -> String {
    let mut cache = SLASHED_DIR_CACHE
        .lock()
        .expect("slashed dir cache poisoned");

    if let Some(value) = cache.get(base) {
        return value.clone();
    }

    let mut value = base.to_owned();
    value.push('/');
    cache.insert(base.to_owned(), value.clone());
    value
}
