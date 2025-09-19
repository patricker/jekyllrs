use magnus::{function, prelude::*, Error, RModule, Value};

pub fn define_into(bridge: &RModule) -> Result<(), Error> {
    bridge.define_singleton_method("has_liquid_construct?", function!(has_liquid_construct, 1))?;
    Ok(())
}

fn has_liquid_construct(content: Option<Value>) -> Result<bool, Error> {
    let Some(content) = content else {
        return Ok(false);
    };
    let string = String::try_convert(content)?;
    Ok(string.contains("{{") || string.contains("{%"))
}
