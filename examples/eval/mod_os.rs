use std::time::{Instant, SystemTime};

use zhuque::{
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    value::JSValue,
};

fn js_os_now(_: ContextRef, _: CallContext) -> JSResult {
    let ns = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;

    Ok(JSValue::from(ns))
}

static MOD_OS_ENTRIES: [FieldDef; 1] = [FieldDef::FUNCTION("now", 0, js_os_now, 0)];

pub struct ModuleOS;

impl ModuleOS {
    pub fn register(ctx: &mut ContextImpl) -> Result<(), i32> {
        ctx.register_native_module("os", &MOD_OS_ENTRIES)?;
        Ok(())
    }
}
