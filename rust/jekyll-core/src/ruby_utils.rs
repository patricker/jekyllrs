use magnus::{exception, prelude::*, Error, RString, Ruby};

#[allow(deprecated)]
fn runtime_error() -> exception::ExceptionClass {
    Ruby::get()
        .ok()
        .map(|ruby| ruby.exception_runtime_error())
        .unwrap_or_else(|| exception::runtime_error())
}

pub fn ruby_handle() -> Result<Ruby, Error> {
    Ruby::get().map_err(|err| Error::new(runtime_error(), format!("Ruby API unavailable: {err}")))
}

pub fn frozen_string(ruby: &Ruby, content: &str) -> RString {
    let string = ruby.str_new(content);
    string.freeze();
    string
}
