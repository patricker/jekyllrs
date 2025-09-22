use magnus::{function, prelude::*, Error, IntoValue, RArray, RHash, RModule, RString, Value};
use once_cell::sync::Lazy;
use regex::Regex;

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("document_basename", function!(document_basename, 1))?;
    bridge.define_singleton_method(
        "document_basename_without_ext",
        function!(document_basename_without_ext, 1),
    )?;
    bridge.define_singleton_method(
        "document_cleaned_relative_path",
        function!(document_cleaned_relative_path, 3),
    )?;
    bridge.define_singleton_method(
        "document_categories_from_path",
        function!(document_categories_from_path, 3),
    )?;
    bridge.define_singleton_method(
        "document_populate_categories",
        function!(document_populate_categories, 1),
    )?;
    bridge.define_singleton_method(
        "document_populate_tags",
        function!(document_populate_tags, 1),
    )?;
    bridge.define_singleton_method("document_title_parts", function!(document_title_parts, 2))?;
    bridge.define_singleton_method("document_metadata", function!(document_metadata, 3))?;
    Ok(())
}

struct TitleParts {
    slug: String,
    ext: Option<String>,
    date: Option<String>,
}

fn document_basename(path: String) -> String {
    basename_component(&path).to_string()
}

fn document_basename_without_ext(path: String) -> String {
    let basename = basename_component(&path);
    let extname = extname_from_basename(basename);
    if extname.is_empty() {
        basename.to_string()
    } else {
        basename[..basename.len() - extname.len()].to_string()
    }
}

fn document_cleaned_relative_path(
    relative_path: String,
    extname: String,
    relative_directory: String,
) -> String {
    let mut relative_path = normalize_owned(relative_path);
    let relative_directory = normalize_owned(relative_directory);

    if !extname.is_empty() && relative_path.ends_with(&extname) {
        let new_len = relative_path.len() - extname.len();
        relative_path.truncate(new_len);
    }

    if !relative_directory.is_empty() {
        if let Some(index) = relative_path.find(&relative_directory) {
            let end = index + relative_directory.len();
            relative_path.replace_range(index..end, "");
        }
    }

    strip_trailing_dots(&mut relative_path);
    relative_path
}

fn document_categories_from_path(
    relative_path: String,
    special_dir: String,
    basename: String,
) -> Result<RArray, Error> {
    let relative_path = normalize_owned(relative_path);
    let special_dir = normalize_owned(special_dir);
    let basename = normalize_owned(basename);
    let categories = categories_from_path(&relative_path, &special_dir, &basename);
    let ruby = ruby_handle()?;
    let array = ruby.ary_new_capa(categories.len());
    for category in categories {
        array.push(category)?;
    }
    Ok(array)
}

fn basename_component(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return path;
    }

    match trimmed.rfind(['/', '\\']) {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    }
}

fn extname_from_basename(name: &str) -> &str {
    if name.is_empty() || name == "." || name == ".." {
        return "";
    }

    let start = name
        .char_indices()
        .find_map(|(idx, ch)| (ch != '.').then_some(idx))
        .unwrap_or(name.len());

    if start == name.len() {
        return "";
    }

    if let Some(relative_index) = name[start..].rfind('.') {
        let dot_index = start + relative_index;
        &name[dot_index..]
    } else {
        ""
    }
}

fn strip_trailing_dots(value: &mut String) {
    while value.ends_with('.') {
        value.pop();
    }
}

fn categories_from_path(relative_path: &str, special_dir: &str, basename: &str) -> Vec<String> {
    if special_dir.is_empty() || relative_path.starts_with(special_dir) {
        return Vec::new();
    }

    let prefix = if let Some(index) = relative_path.find(special_dir) {
        &relative_path[..index]
    } else {
        relative_path
    };

    prefix
        .split(|ch| ch == '/' || ch == '\\')
        .filter(|segment| !segment.is_empty() && *segment != special_dir && *segment != basename)
        .map(|segment| segment.to_string())
        .collect()
}

fn document_populate_categories(data: Value) -> Result<RArray, Error> {
    let ruby = ruby_handle()?;

    let data_hash = match RHash::from_value(data) {
        Some(hash) => hash,
        None => return Ok(ruby.ary_new()),
    };

    let categories_key = ruby.str_new("categories").into_value_with(&ruby);
    let singular = ruby.str_new("category").into_value_with(&ruby);
    let plural = ruby.str_new("categories").into_value_with(&ruby);

    let categories_value = data_hash.aref(categories_key)?;
    let categories_array = arrayify(categories_value)?;

    let pluralized = crate::utils::pluralized_array_from_hash(data, singular, plural)?;
    let pluralized_array = arrayify(pluralized)?;

    let combined = ruby.ary_new();
    append_array(&combined, &categories_array)?;
    append_array(&combined, &pluralized_array)?;

    let stringified = ruby.ary_new_capa(combined.len());
    for value in combined.each() {
        let value = value?;
        let string: RString = value.funcall("to_s", ())?;
        stringified.push(string)?;
    }

    stringified.funcall::<_, _, Value>("flatten!", ())?;
    stringified.funcall::<_, _, Value>("uniq!", ())?;

    Ok(stringified)
}

fn document_populate_tags(data: Value) -> Result<RArray, Error> {
    let ruby = ruby_handle()?;

    if RHash::from_value(data).is_none() {
        return Ok(ruby.ary_new());
    }

    let tag = ruby.str_new("tag").into_value_with(&ruby);
    let tags = ruby.str_new("tags").into_value_with(&ruby);
    let values = crate::utils::pluralized_array_from_hash(data, tag, tags)?;
    let array = arrayify(values)?;
    array.funcall::<_, _, Value>("flatten!", ())?;
    Ok(array)
}

fn document_title_parts(
    relative_path: String,
    basename_without_ext: String,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let TitleParts { slug, ext, date } = title_parts(&relative_path, &basename_without_ext);

    let hash = ruby.hash_new();
    hash.aset(ruby.str_new("slug"), ruby.str_new(&slug))?;
    match ext {
        Some(ext) => hash.aset(ruby.str_new("ext"), ruby.str_new(&ext))?,
        None => hash.aset(ruby.str_new("ext"), ruby.qnil())?,
    }
    match date {
        Some(date) => hash.aset(ruby.str_new("date"), ruby.str_new(&date))?,
        None => hash.aset(ruby.str_new("date"), ruby.qnil())?,
    }

    Ok(hash.into_value_with(&ruby))
}

fn arrayify(value: Value) -> Result<RArray, Error> {
    let ruby = ruby_handle()?;

    if value.is_nil() {
        return Ok(ruby.ary_new());
    }

    if let Some(array) = RArray::from_value(value) {
        return Ok(array);
    }

    if value.respond_to("to_ary", false)? {
        let converted: Value = value.funcall("to_ary", ())?;
        return arrayify(converted);
    }

    if value.respond_to("to_a", false)? {
        let converted: Value = value.funcall("to_a", ())?;
        return arrayify(converted);
    }

    let array = ruby.ary_new();
    array.push(value)?;
    Ok(array)
}

fn append_array(target: &RArray, source: &RArray) -> Result<(), Error> {
    for value in source.each() {
        target.push(value?)?;
    }
    target.funcall::<_, _, Value>("compact!", ())?;
    Ok(())
}

fn normalize_owned(value: String) -> String {
    if value.contains('\\') {
        value.replace('\\', "/")
    } else {
        value
    }
}

static DATE_FILENAME_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:.*/)*?(\d{2,4}-\d{1,2}-\d{1,2})-([^/]*)(\.[^.]+)$")
        .expect("valid date filename regex")
});

static DATELESS_FILENAME_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:.*/)*([^/]*)(\.[^.]+)$").expect("valid dateless filename regex"));

fn title_parts(relative_path: &str, fallback: &str) -> TitleParts {
    if let Some(caps) = DATE_FILENAME_RE.captures(relative_path) {
        let date = caps.get(1).map(|m| m.as_str().to_string());
        let slug_raw = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let ext = caps.get(3).map(|m| m.as_str().to_string());
        let slug = trim_trailing_dots(slug_raw);
        return TitleParts { slug, ext, date };
    }

    if let Some(caps) = DATELESS_FILENAME_RE.captures(relative_path) {
        let slug_raw = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let ext = caps.get(2).map(|m| m.as_str().to_string());
        let slug = trim_trailing_dots(slug_raw);
        return TitleParts {
            slug,
            ext,
            date: None,
        };
    }

    let slug = trim_trailing_dots(fallback);
    TitleParts {
        slug,
        ext: None,
        date: None,
    }
}

fn trim_trailing_dots(input: &str) -> String {
    let mut slug = input.to_string();
    while slug.ends_with('.') {
        slug.pop();
    }
    slug
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_handles_unix_paths() {
        assert_eq!(
            document_basename("/foo/bar/configuration.md".to_string()),
            "configuration.md"
        );
    }

    #[test]
    fn basename_handles_windows_paths() {
        assert_eq!(
            document_basename("C:/foo/bar/configuration.md".to_string()),
            "configuration.md"
        );
    }

    #[test]
    fn basename_without_ext_regular_file() {
        assert_eq!(
            document_basename_without_ext("/foo/bar/post.md".to_string()),
            "post"
        );
    }

    #[test]
    fn basename_without_ext_trailing_dots() {
        assert_eq!(
            document_basename_without_ext("/foo/bar/trailing-dots...md".to_string()),
            "trailing-dots.."
        );
    }

    #[test]
    fn basename_without_ext_dotfile() {
        assert_eq!(
            document_basename_without_ext("/foo/.bashrc".to_string()),
            ".bashrc"
        );
    }

    #[test]
    fn basename_without_ext_double_dotfile() {
        assert_eq!(
            document_basename_without_ext("/foo/..hidden".to_string()),
            "..hidden"
        );
    }

    #[test]
    fn cleaned_relative_path_removes_ext_and_dots() {
        let result = document_cleaned_relative_path(
            "_methods/site/generate...md".to_string(),
            ".md".to_string(),
            "_methods".to_string(),
        );
        assert_eq!(result, "/site/generate");
    }

    #[test]
    fn cleaned_relative_path_without_extension() {
        let result = document_cleaned_relative_path(
            "_methods/site/generate".to_string(),
            "".to_string(),
            "_methods".to_string(),
        );
        assert_eq!(result, "/site/generate");
    }

    #[test]
    fn categories_inside_special_dir_returns_empty() {
        let categories = categories_from_path(
            "_posts/2018-10-12-hello.md",
            "_posts",
            "2018-10-12-hello.md",
        );
        assert!(categories.is_empty());
    }

    #[test]
    fn categories_outside_special_dir_collects_segments() {
        let categories = categories_from_path(
            "blog/_posts/2018-10-12-hello.md",
            "_posts",
            "2018-10-12-hello.md",
        );
        assert_eq!(categories, vec!["blog".to_string()]);
    }

    #[test]
    fn categories_without_special_dir_match() {
        let categories = categories_from_path("es/blog/hello.md", "_drafts", "hello.md");
        assert_eq!(categories, vec!["es".to_string(), "blog".to_string()]);
    }

    #[test]
    fn extname_trailing_dots_returns_single_dot() {
        assert_eq!(extname_from_basename("foo..."), ".");
    }

    #[test]
    fn title_parts_extracts_date_slug_and_ext() {
        let TitleParts { slug, ext, date } = title_parts(
            "_posts/2018-10-12-trailing-dots...markdown",
            "trailing-dots..",
        );
        assert_eq!(slug, "trailing-dots");
        assert_eq!(ext.as_deref(), Some(".markdown"));
        assert_eq!(date.as_deref(), Some("2018-10-12"));
    }

    #[test]
    fn title_parts_handles_dateless_filename() {
        let TitleParts { slug, ext, date } = title_parts("docs/guide.md", "guide");
        assert_eq!(slug, "guide");
        assert_eq!(ext.as_deref(), Some(".md"));
        assert!(date.is_none());
    }

    #[test]
    fn title_parts_falls_back_to_basename_without_ext() {
        let TitleParts { slug, ext, date } = title_parts("README", "README");
        assert_eq!(slug, "README");
        assert!(ext.is_none());
        assert!(date.is_none());
    }
}

fn document_metadata(
    path: String,
    relative_path: String,
    special_dir: String,
) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let basename = document_basename(path.clone());
    let basename_wo = document_basename_without_ext(path.clone());
    let TitleParts { slug, ext, date } = title_parts(&relative_path, &basename_wo);
    let categories = categories_from_path(&relative_path, &special_dir, &basename);

    let hash = ruby.hash_new();
    hash.aset(ruby.str_new("basename"), ruby.str_new(&basename))?;
    hash.aset(
        ruby.str_new("basename_without_ext"),
        ruby.str_new(&basename_wo),
    )?;
    hash.aset(ruby.str_new("slug"), ruby.str_new(&slug))?;
    match ext {
        Some(ext) => hash.aset(ruby.str_new("ext"), ruby.str_new(&ext))?,
        None => hash.aset(ruby.str_new("ext"), ruby.qnil())?,
    }
    match date {
        Some(date) => hash.aset(ruby.str_new("date"), ruby.str_new(&date))?,
        None => hash.aset(ruby.str_new("date"), ruby.qnil())?,
    }

    let array = ruby.ary_new_capa(categories.len());
    for c in categories {
        array.push(c)?;
    }
    hash.aset(ruby.str_new("categories"), array)?;

    Ok(hash.into_value_with(&ruby))
}
