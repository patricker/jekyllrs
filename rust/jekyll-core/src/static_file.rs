use magnus::{function, prelude::*, Error, RModule};

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("static_file_basename", function!(basename, 2))?;
    bridge.define_singleton_method(
        "static_file_cleaned_relative_path",
        function!(cleaned_relative_path, 3),
    )?;
    Ok(())
}

fn basename(name: String, extname: Option<String>) -> String {
    let mut base = match extname {
        Some(ref ext) if !ext.is_empty() && name.ends_with(ext) => {
            name[..name.len() - ext.len()].to_string()
        }
        _ => name,
    };

    trim_trailing_dots(&mut base);
    base
}

fn cleaned_relative_path(
    relative_path: String,
    extname: Option<String>,
    collection_relative_directory: Option<String>,
) -> String {
    let mut cleaned = match extname {
        Some(ref ext) if !ext.is_empty() && relative_path.ends_with(ext) => {
            relative_path[..relative_path.len() - ext.len()].to_string()
        }
        _ => relative_path,
    };

    trim_trailing_dots(&mut cleaned);

    if let Some(dir) = collection_relative_directory {
        if !dir.is_empty() && cleaned.starts_with(&dir) {
            cleaned = cleaned[dir.len()..].to_string();
        }
    }

    cleaned
}

fn trim_trailing_dots(value: &mut String) {
    while value.ends_with('.') {
        value.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_strips_extension_and_dots() {
        assert_eq!(
            basename("static_file.txt".into(), Some(".txt".into())),
            "static_file"
        );
        assert_eq!(
            basename("trail...dots.txt".into(), Some(".txt".into())),
            "trail...dots"
        );
        assert_eq!(basename("noext".into(), None), "noext");
    }

    #[test]
    fn cleaned_relative_path_removes_extension_and_collection_dir() {
        assert_eq!(
            cleaned_relative_path(
                "_foo/dir/file.txt".into(),
                Some(".txt".into()),
                Some("_foo".into()),
            ),
            "/dir/file"
        );
    }

    #[test]
    fn cleaned_relative_path_handles_no_collection() {
        assert_eq!(
            cleaned_relative_path("dir/my-cool-avatar...png".into(), Some(".png".into()), None,),
            "dir/my-cool-avatar"
        );
    }
}
