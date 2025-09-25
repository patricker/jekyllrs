use magnus::{prelude::*, Error, RModule, RString, Value};

// Normalize a Path to a forward-slash string regardless of platform
fn to_forward_slash(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        s.into_owned()
    } else {
        s.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

// Recursively list all entries under base_dir, returning relative paths with '/'
// Uses EntryFilter#symlink? to decide whether to descend into directories (safe-mode semantics)
pub fn recursive_list_site(site: Value, base_dir: &str) -> Result<Vec<String>, Error> {
    let ruby = crate::ruby_utils::ruby_handle()?;
    let jekyll: RModule = ruby.class_object().const_get("Jekyll")?;
    let ef_class: Value = jekyll.const_get("EntryFilter")?;
    let ef: Value = ef_class.funcall("new", (site,))?;
    let file: Value = ruby.class_object().const_get("File")?;

    // canonicalize base via Ruby expand_path to mirror Ruby behavior
    let base_abs: RString = file.funcall("expand_path", (ruby.str_new(base_dir),))?;
    let base_path = std::path::PathBuf::from(base_abs.to_string()?);

    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![base_path.clone()];

    while let Some(dir) = stack.pop() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                let path = entry.path();
                let rel = match path.strip_prefix(&base_path) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if rel.as_os_str().is_empty() {
                    continue;
                }
                let rel_str = to_forward_slash(rel);
                out.push(rel_str.clone());

                if let Ok(ft) = entry.file_type() {
                    if ft.is_dir() {
                        let full: RString = file.funcall(
                            "join",
                            (
                                ruby.str_new(base_path.to_string_lossy().as_ref()),
                                ruby.str_new(&rel_str),
                            ),
                        )?;
                        let is_bad: bool = ef.funcall("symlink?", (full,))?;
                        if !is_bad {
                            stack.push(path);
                        }
                    }
                }
            }
        }
    }

    Ok(out)
}
