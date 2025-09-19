use magnus::{function, prelude::*, Error, RArray, RModule};

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
    Ok(())
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
    mut relative_path: String,
    extname: String,
    relative_directory: String,
) -> String {
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
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != special_dir && *segment != basename)
        .map(|segment| segment.to_string())
        .collect()
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
}
