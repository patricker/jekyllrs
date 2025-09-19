use magnus::{function, prelude::*, Error, RModule, Value};
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

use crate::ruby_utils::ruby_handle;

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("has_yaml_header?", function!(has_yaml_header, 1))?;
    Ok(())
}

fn has_yaml_header(path: Value) -> Result<bool, Error> {
    let ruby = ruby_handle()?;
    let path_str = String::try_convert(path)?;

    let file = match File::open(Path::new(&path_str)) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(Error::new(ruby.exception_io_error(), err.to_string())),
    };

    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();

    match reader.read_until(b'\n', &mut buffer) {
        Ok(0) => Ok(false),
        Ok(_) => Ok(matches_yaml_header(&buffer)),
        Err(err) => Err(Error::new(ruby.exception_io_error(), err.to_string())),
    }
}

fn matches_yaml_header(bytes: &[u8]) -> bool {
    let mut slice = bytes;

    slice = trim_trailing(slice, |b| matches!(b, b'\n' | b'\r' | b' ' | b'\t'));

    if slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
        slice = &slice[3..];
        slice = trim_trailing(slice, |b| matches!(b, b'\n' | b'\r' | b' ' | b'\t'));
    }

    slice == b"---"
}

fn trim_trailing(slice: &[u8], predicate: impl Fn(u8) -> bool) -> &[u8] {
    let mut end = slice.len();
    while end > 0 && predicate(slice[end - 1]) {
        end -= 1;
    }
    &slice[..end]
}

#[cfg(test)]
mod tests {
    use super::matches_yaml_header;

    #[test]
    fn accepts_standard_yaml_header() {
        assert!(matches_yaml_header(b"---\n"));
        assert!(matches_yaml_header(b"---  \r\n"));
    }

    #[test]
    fn ignores_utf8_bom() {
        assert!(matches_yaml_header(b"\xEF\xBB\xBF---\n"));
    }

    #[test]
    fn rejects_non_header() {
        assert!(!matches_yaml_header(b"# not front matter\n"));
    }
}
