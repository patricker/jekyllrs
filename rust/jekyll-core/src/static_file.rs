use magnus::{function, prelude::*, Error, ExceptionClass, IntoValue, RHash, RModule, Ruby, Value};
use once_cell::sync::Lazy;
use std::{collections::HashMap, fs, io, path::Path, sync::Mutex};

use crate::ruby_utils::ruby_handle;

static MTIMES: Lazy<Mutex<HashMap<String, i64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("static_file_basename", function!(basename, 2))?;
    bridge.define_singleton_method(
        "static_file_cleaned_relative_path",
        function!(cleaned_relative_path, 3),
    )?;
    bridge.define_singleton_method("static_file_write", function!(static_file_write, 5))?;
    bridge.define_singleton_method(
        "static_file_destination_rel_dir",
        function!(destination_rel_dir, 3),
    )?;
    bridge.define_singleton_method("static_file_mtime_get", function!(static_file_mtime_get, 1))?;
    bridge.define_singleton_method("static_file_mtime_set", function!(static_file_mtime_set, 2))?;
    bridge.define_singleton_method(
        "static_file_mtimes_reset",
        function!(static_file_mtimes_reset, 0),
    )?;
    bridge.define_singleton_method(
        "static_file_mtimes_snapshot",
        function!(static_file_mtimes_snapshot, 0),
    )?;
    bridge.define_singleton_method(
        "static_file_write_batch",
        function!(static_file_write_batch, 3),
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

fn destination_rel_dir(
    url: Option<String>,
    dir: Option<String>,
    has_collection: bool,
) -> Result<String, Error> {
    if has_collection {
        let ruby = ruby_handle()?;
        let file_class: Value = ruby.class_object().const_get("File")?;
        let url_string = url.unwrap_or_default();
        let url_value = ruby.str_new(&url_string);
        let dirname: Value = file_class.funcall("dirname", (url_value,))?;
        let result: String = String::try_convert(dirname)?;
        Ok(result)
    } else {
        Ok(dir.unwrap_or_default())
    }
}

fn static_file_write(
    src_path: String,
    dest_path: String,
    mtime: i64,
    safe_mode: bool,
    production: bool,
) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    perform_copy(&ruby, &src_path, &dest_path, mtime, safe_mode, production)?;
    Ok(true)
}

fn perform_copy(
    ruby: &Ruby,
    src: &str,
    dest: &str,
    mtime: i64,
    _safe: bool,
    production: bool,
) -> Result<(), Error> {
    let dest_path = Path::new(dest);

    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| io_error(ruby, parent.to_string_lossy().as_ref(), &err))?;
    }

    match fs::remove_file(dest_path) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) if err.kind() == io::ErrorKind::IsADirectory => {
            fs::remove_dir_all(dest_path).map_err(|err| io_error(ruby, dest, &err))?;
        }
        Err(err) => return Err(io_error(ruby, dest, &err)),
    }

    fs::copy(src, dest_path).map_err(|err| io_error(ruby, dest, &err))?;

    if production {
        if let Ok(src_meta) = std::fs::metadata(src) {
            let perms = src_meta.permissions();
            let _ = std::fs::set_permissions(dest_path, perms);
        }
    }

    apply_times(ruby, dest, mtime)?;
    Ok(())
}

fn apply_times(ruby: &Ruby, dest: &str, mtime: i64) -> Result<(), Error> {
    let file_class: Value = ruby.class_object().const_get("File")?;
    let dest_value = ruby.str_new(dest);
    let is_symlink: bool = file_class.funcall("symlink?", (dest_value,))?;
    if !is_symlink {
        let time_value = mtime.into_value_with(ruby);
        let dest_value = ruby.str_new(dest);
        let _ = file_class.funcall::<_, _, Value>("utime", (time_value, time_value, dest_value))?;
    }
    Ok(())
}

fn static_file_mtime_get(path: String) -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let map = MTIMES.lock().map_err(|_| {
        Error::new(
            ruby.exception_runtime_error(),
            "static file mtime cache poisoned",
        )
    })?;
    let value = match map.get(&path) {
        Some(&mtime) => mtime.into_value_with(&ruby),
        None => ruby.qnil().into_value_with(&ruby),
    };
    Ok(value)
}

fn static_file_mtime_set(path: String, mtime: i64) -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let mut map = MTIMES.lock().map_err(|_| {
        Error::new(
            ruby.exception_runtime_error(),
            "static file mtime cache poisoned",
        )
    })?;
    map.insert(path, mtime);
    Ok(())
}

fn static_file_mtimes_reset() -> Result<(), Error> {
    let ruby = ruby_handle()?;
    let mut map = MTIMES.lock().map_err(|_| {
        Error::new(
            ruby.exception_runtime_error(),
            "static file mtime cache poisoned",
        )
    })?;
    map.clear();
    Ok(())
}

fn static_file_mtimes_snapshot() -> Result<Value, Error> {
    let ruby = ruby_handle()?;
    let hash: RHash = ruby.hash_new();
    let map = MTIMES.lock().map_err(|_| {
        Error::new(
            ruby.exception_runtime_error(),
            "static file mtime cache poisoned",
        )
    })?;
    for (path, mtime) in map.iter() {
        hash.aset(ruby.str_new(path), mtime.into_value_with(&ruby))?;
    }
    Ok(hash.into_value_with(&ruby))
}

fn io_error(ruby: &Ruby, path: &str, err: &io::Error) -> Error {
    let error_class: ExceptionClass = ruby
        .class_object()
        .const_get("IOError")
        .expect("IOError constant");
    Error::new(error_class, format!("Error handling '{}': {}", path, err))
}


fn simple_copy(src: &str, dest: &str, production: bool) -> io::Result<()> {
    let dest_path = Path::new(dest);
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::remove_file(dest_path) {
        Ok(_) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) if err.kind() == io::ErrorKind::IsADirectory => {
            fs::remove_dir_all(dest_path)?;
        }
        Err(err) => return Err(err),
    }
    fs::copy(src, dest_path)?;
    if production {
        if let Ok(meta) = fs::metadata(src) {
            let _ = fs::set_permissions(dest_path, meta.permissions());
        }
    }
    Ok(())
}

fn static_file_write_batch(jobs: Value, _safe: bool, production: bool) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    let mut list: Vec<(String, String, i64)> = Vec::new();

    if let Some(ary) = magnus::RArray::from_value(jobs) {
        for elem in ary.each() {
            let elem = elem?;
            let job = magnus::RArray::from_value(elem)
                .ok_or_else(|| Error::new(ruby.exception_type_error(), "job must be an array"))?;
            let src: String = String::try_convert(job.entry(0)?)?;
            let dest: String = String::try_convert(job.entry(1)?)?;
            let mtime: i64 = i64::try_convert(job.entry(2)?).unwrap_or(0);
            list.push((src, dest, mtime));
        }
    } else {
        return Err(Error::new(
            ruby.exception_type_error(),
            "jobs must be an array of [src, dest, mtime]",
        ));
    }

    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let chunk_size = (list.len() / threads).max(1);
    let mut handles = Vec::new();
    for chunk in list.chunks(chunk_size) {
        let jobs = chunk.to_owned();
        let prod = production;
        let handle = std::thread::spawn(move || {
            for (src, dest, _mtime) in jobs {
                let _ = simple_copy(&src, &dest, prod);
            }
        });
        handles.push(handle);
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(true)
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
