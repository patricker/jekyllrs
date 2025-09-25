use magnus::{function, prelude::*, Error, IntoValue, RModule, RString, Ruby, Value};

use crate::ruby_utils::ruby_handle;

fn to_forward_slash(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("theme_assets_list", function!(theme_assets_list, 1))?;
    Ok(())
}

fn theme_assets_list(root: RString) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let root_str = root.to_string()?;
    let root_path = std::path::PathBuf::from(&root_str);
    let theme_root = root_path.parent().map(|p| p.to_path_buf());
    let mut stack: Vec<std::path::PathBuf> = vec![root_path.clone()];
    let mut files: Vec<(String, String, bool)> = Vec::new();

    while let Some(dirp) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dirp) else {
            continue;
        };

        let mut entries: Vec<std::fs::DirEntry> = read.filter_map(Result::ok).collect();
        entries.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

        for ent in entries {
            let Ok(ft) = ent.file_type() else {
                continue;
            };

            let is_symlink = ft.is_symlink();
            let path = ent.path();

            if ft.is_dir() && !is_symlink {
                stack.push(path.clone());
                continue;
            }

            let relative_path = if let Some(root_parent) = &theme_root {
                match path.strip_prefix(root_parent) {
                    Ok(rel) => rel,
                    Err(_) => continue,
                }
            } else {
                match path.strip_prefix(&root_path) {
                    Ok(rel) => rel,
                    Err(_) => continue,
                }
            };
            if relative_path.as_os_str().is_empty() {
                continue;
            }

            let relative_str = to_forward_slash(relative_path);
            let absolute_str = to_forward_slash(&path);

            files.push((absolute_str, relative_str, is_symlink));
        }
    }

    files.sort_by(|a, b| a.1.cmp(&b.1));

    let array = ruby.ary_new_capa(files.len());
    for (absolute, relative, symlink) in files {
        let entry = build_entry(&ruby, &absolute, &relative, symlink)?;
        array.push(entry)?;
    }
    Ok(array.into_value_with(&ruby))
}

fn build_entry(ruby: &Ruby, absolute: &str, relative: &str, symlink: bool) -> Result<Value, Error> {
    let entry = ruby.ary_new_capa(3);
    entry.push(ruby.str_new(absolute))?;
    entry.push(ruby.str_new(relative))?;
    let bool_value = if symlink {
        true.into_value_with(ruby)
    } else {
        false.into_value_with(ruby)
    };
    entry.push(bool_value)?;
    Ok(entry.into_value_with(ruby))
}
