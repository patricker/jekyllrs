use magnus::{function, prelude::*, Error, IntoValue, RArray, RModule, Value};
use std::collections::HashSet;
use std::path::{Path, MAIN_SEPARATOR};

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method(
        "cleaner_existing_files",
        function!(cleaner_existing_files, 2),
    )?;
    Ok(())
}

fn cleaner_existing_files(site_dest: String, keep_files_val: Value) -> Result<Value, Error> {
    let ruby = ruby_handle()?;

    // Convert keep_files to Vec<String>
    let mut keep_files: Vec<String> = Vec::new();
    if let Ok(arr) = RArray::try_convert(keep_files_val) {
        for v in arr.each() {
            if let Ok(s) = String::try_convert(v?) {
                keep_files.push(s);
            }
        }
    }

    // Compute keep_dirs as all parent dirs of site_dest/keep_file entries
    let keep_dirs = compute_keep_dirs(&site_dest, &keep_files);

    // Native traversal under site_dest (include files and dirs; include dotfiles)
    let mut entries: Vec<String> = Vec::new();
    let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(&site_dest)];
    while let Some(dirp) = stack.pop() {
        if let Ok(read) = std::fs::read_dir(&dirp) {
            for ent in read.flatten() {
                let path = ent.path();
                let s = path.to_string_lossy().to_string();
                if s.is_empty() {
                    continue;
                }
                entries.push(s.clone());
                if let Ok(ft) = ent.file_type() {
                    if ft.is_dir() {
                        stack.push(path);
                    }
                }
            }
        }
    }

    let mut results: Vec<String> = Vec::new();

    for entry in entries {
        if is_hidden_meta(&entry) {
            continue;
        }
        if is_kept_file(&site_dest, &entry, &keep_files) {
            continue;
        }
        if keep_dirs.contains(&entry) {
            continue;
        }
        results.push(entry);
    }

    let array = ruby.ary_new();
    for e in results {
        array.push(ruby.str_new(&e))?;
    }
    Ok(array.into_value_with(&ruby))
}

fn is_hidden_meta(path: &str) -> bool {
    path.ends_with(&format!("{}.", MAIN_SEPARATOR))
        || path.ends_with(&format!("{}..", MAIN_SEPARATOR))
}

fn is_kept_file(site_dest: &str, entry: &str, keep_files: &[String]) -> bool {
    // Prefix to match: "#{site_dest}/#{keep}"
    let prefix = format!("{}{}", site_dest, MAIN_SEPARATOR);
    for k in keep_files {
        let mut target = String::with_capacity(prefix.len() + k.len());
        target.push_str(&prefix);
        target.push_str(k);
        if entry.starts_with(&target) {
            return true;
        }
    }
    false
}

fn compute_keep_dirs(site_dest: &str, keep_files: &[String]) -> HashSet<String> {
    let mut set = HashSet::new();
    for k in keep_files {
        let full = Path::new(site_dest).join(k);
        // Push parent directories up to, but not including, site_dest
        let mut current = full.as_path();
        while let Some(parent) = current.parent() {
            let parent_str = parent.to_string_lossy().to_string();
            if parent_str == site_dest {
                break;
            }
            set.insert(parent_str.clone());
            current = parent;
        }
    }
    set
}
