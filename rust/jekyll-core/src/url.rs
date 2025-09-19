use magnus::r_hash::ForEach;
use magnus::{function, prelude::*, Error, IntoValue, RHash, RModule, RString, Ruby, Value};
use once_cell::sync::Lazy;
use percent_encoding::percent_decode_str;
use regex::Regex;

use crate::ruby_utils::ruby_handle;

static DROP_PLACEHOLDER_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r":([a-z_]+)").expect("valid drop placeholder regex"));

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("url_escape_path", function!(url_escape_path, 1))?;
    bridge.define_singleton_method("url_unescape_path", function!(url_unescape_path, 1))?;
    bridge.define_singleton_method("url_sanitize", function!(url_sanitize, 1))?;
    bridge.define_singleton_method(
        "url_generate_from_hash",
        function!(url_generate_from_hash, 2),
    )?;
    bridge.define_singleton_method(
        "url_generate_from_drop",
        function!(url_generate_from_drop, 2),
    )?;
    Ok(())
}

fn url_escape_path(path: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let string = path.to_string()?;
    let escaped = escape_path_internal(&string);
    Ok(ruby.str_new(&escaped).into_value_with(&ruby))
}

fn url_unescape_path(path: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let string = path.to_string()?;
    let decoded = percent_decode_str(&string)
        .decode_utf8()
        .map_err(|err| Error::new(ruby.exception_arg_error(), err.to_string()))?;
    Ok(ruby.str_new(decoded.as_ref()).into_value_with(&ruby))
}

fn url_sanitize(path: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let string = path.to_string()?;
    let sanitized = sanitize_internal(&string);
    Ok(ruby.str_new(&sanitized).into_value_with(&ruby))
}

fn url_generate_from_hash(template: Value, placeholders: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let template = String::try_convert(template)?;
    let hash = RHash::from_value(placeholders)
        .ok_or_else(|| Error::new(ruby.exception_type_error(), "placeholders must be a hash"))?;

    let result = generate_url_from_hash_internal(&template, &hash)?;
    Ok(ruby.str_new(&result).into_value_with(&ruby))
}

fn url_generate_from_drop(template: Value, drop: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let template = String::try_convert(template)?;
    let result = generate_url_from_drop_internal(&ruby, &template, drop)?;
    Ok(ruby.str_new(&result).into_value_with(&ruby))
}

fn escape_path_internal(path: &str) -> String {
    if path.is_empty() || path.bytes().all(is_simple_path_char) {
        return path.to_owned();
    }

    let mut escaped = String::with_capacity(path.len());

    for &byte in path.as_bytes() {
        if is_allowed_url_char(byte) {
            escaped.push(byte as char);
        } else {
            escaped.push('%');
            escaped.push_str(&format!("{:02X}", byte));
        }
    }

    if let Some(index) = escaped.find('#') {
        escaped.replace_range(index..=index, "%23");
    }

    escaped
}

fn sanitize_internal(input: &str) -> String {
    let mut result = format!("/{}", input);

    while result.contains("..") {
        result = result.replace("..", "/");
    }

    while result.contains("./") {
        result = result.replace("./", "");
    }

    squeeze_slashes(&result)
}

fn squeeze_slashes(path: &str) -> String {
    let mut squeezed = String::with_capacity(path.len());
    let mut prev_was_slash = false;

    for ch in path.chars() {
        if ch == '/' {
            if !prev_was_slash {
                squeezed.push(ch);
            }
            prev_was_slash = true;
        } else {
            squeezed.push(ch);
            prev_was_slash = false;
        }
    }

    squeezed
}

fn is_simple_path_char(byte: u8) -> bool {
    matches!(byte,
        b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'.'
            | b'/'
            | b'-'
    )
}

fn is_allowed_url_char(byte: u8) -> bool {
    matches!(byte,
        b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'-'
            | b'.'
            | b'_'
            | b'~'
            | b'!'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b';'
            | b'='
            | b':'
            | b'@'
            | b'/'
            | b'#'
    )
}

fn generate_url_from_hash_internal(template: &str, placeholders: &RHash) -> Result<String, Error> {
    let mut pairs: Vec<(String, Option<String>)> = Vec::new();

    placeholders.foreach(|key: Value, value: Value| {
        let key_string = value_to_string(key)?;
        let converted = if value.is_nil() {
            None
        } else {
            Some(value_to_string(value)?)
        };
        pairs.push((key_string, converted));
        Ok(ForEach::Continue)
    })?;

    Ok(generate_url_from_pairs(template, &pairs))
}

fn generate_url_from_pairs(template: &str, pairs: &[(String, Option<String>)]) -> String {
    let mut result = template.to_string();

    for (key, value) in pairs {
        if !result.contains(':') {
            break;
        }

        if let Some(ref value) = value {
            let escaped = escape_path_internal(value);
            let placeholder = format!(":{}", key);
            result = result.replace(&placeholder, &escaped);
        } else {
            let placeholder = format!("/:{}", key);
            result = result.replace(&placeholder, "");
        }
    }

    result
}

fn generate_url_from_drop_internal(
    ruby: &Ruby,
    template: &str,
    drop: Value,
) -> Result<String, Error> {
    let mut result = String::with_capacity(template.len());
    let mut last_index = 0;

    for captures in DROP_PLACEHOLDER_RE.captures_iter(template) {
        let matched = captures.get(0).expect("match exists");
        let name = captures.get(1).expect("capture exists").as_str();

        result.push_str(&template[last_index..matched.start()]);

        let replacement = replace_drop_placeholder(ruby, drop, matched.as_str(), name)?;
        result.push_str(&replacement);

        last_index = matched.end();
    }

    result.push_str(&template[last_index..]);

    Ok(result)
}

fn replace_drop_placeholder(
    ruby: &Ruby,
    drop: Value,
    matched: &str,
    name: &str,
) -> Result<String, Error> {
    let candidates = placeholder_candidates(name);

    let winner = find_drop_key(ruby, drop, &candidates)?;

    let Some(winner) = winner else {
        let message = format!(
            "The URL template doesn't have {} keys. Check your permalink template!",
            candidates.join(" or ")
        );
        return Err(Error::new(ruby.exception_no_method_error(), message));
    };

    let value = drop.funcall::<_, _, Value>("[]", (winner.accessor,))?;
    let replacement = if value.is_nil() {
        String::new()
    } else {
        let value_string = value_to_string(value)?;
        escape_path_internal(&value_string)
    };

    apply_replacement(matched, &winner.key_string, &replacement)
}

fn placeholder_candidates(name: &str) -> Vec<String> {
    let mut candidates = Vec::with_capacity(2);
    candidates.push(name.to_string());
    if name.ends_with('_') && name.len() > 1 {
        let trimmed = name[..name.len() - 1].to_string();
        candidates.push(trimmed);
    }
    candidates
}

struct DropKey {
    accessor: Value,
    key_string: String,
}

fn find_drop_key(
    ruby: &Ruby,
    drop: Value,
    candidates: &[String],
) -> Result<Option<DropKey>, Error> {
    for candidate in candidates {
        let string_value = ruby.str_new(candidate);
        let present: bool = drop.funcall("key?", (string_value,))?;
        if present {
            return Ok(Some(DropKey {
                accessor: string_value.into_value_with(ruby),
                key_string: candidate.clone(),
            }));
        }

        let symbol = ruby.to_symbol(candidate);
        let present_symbol: bool = drop.funcall("key?", (symbol,))?;
        if present_symbol {
            return Ok(Some(DropKey {
                accessor: symbol.into_value_with(ruby),
                key_string: candidate.clone(),
            }));
        }
    }

    Ok(None)
}

fn value_to_string(value: Value) -> Result<String, Error> {
    match String::try_convert(value) {
        Ok(string) => Ok(string),
        Err(_) => {
            let coerced = value.funcall::<_, _, Value>("to_s", ())?;
            String::try_convert(coerced)
        }
    }
}

fn apply_replacement(matched: &str, key: &str, escaped: &str) -> Result<String, Error> {
    if key.is_empty() {
        return Ok(escaped.to_string());
    }

    let target = format!(":{}", key);
    if let Some(pos) = matched.find(&target) {
        let mut replaced = matched.to_string();
        replaced.replace_range(pos..pos + target.len(), escaped);
        Ok(replaced)
    } else {
        Ok(escaped.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_path_returns_unchanged_for_simple_paths() {
        assert_eq!("/a/b-c", escape_path_internal("/a/b-c"));
    }

    #[test]
    fn escape_path_encodes_spaces_and_unicode() {
        assert_eq!("/foo%20bar", escape_path_internal("/foo bar"));
        assert_eq!("/caf%C3%A9", escape_path_internal("/caf\u{00E9}"));
    }

    #[test]
    fn escape_path_replaces_first_hash() {
        assert_eq!("/foo%23bar#baz", escape_path_internal("/foo#bar#baz"));
    }

    #[test]
    fn sanitize_normalizes_dots_and_slashes() {
        assert_eq!("/foo/bar", sanitize_internal("foo/./bar"));
        assert_eq!("/foo/bar", sanitize_internal("foo//bar"));
        assert_eq!("/foo/bar", sanitize_internal("foo/../bar"));
    }

    #[test]
    fn generate_url_from_hash_replaces_placeholders() {
        let pairs = vec![
            ("x".to_string(), Some("foo".to_string())),
            ("y".to_string(), Some("bar".to_string())),
        ];
        let result = generate_url_from_pairs("/:x/:y", &pairs);
        assert_eq!("/foo/bar", result);
    }
}
