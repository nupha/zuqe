use zhuque::{
    JS_CFUNC_NATIVE_DEF, JS_PROP_INT32_DEF,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_C_W_E,
    value::JSValue,
};

fn std_exit(ctx: &JSContext, cc: CallContext) -> JSResult {
    println!("*** exit called");
    Ok(JSValue::UNDEFINED)
}

static MOD_STD_ENTRIES: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("exit", 1, std_exit, 0),
    JS_PROP_INT32_DEF!("out", 9, JS_PROP_C_W_E),
];

pub fn init_mod_std(ctx: &JSContext) -> Result<(), i32> {
    ctx.register_native_module("std", &MOD_STD_ENTRIES)?;

    Ok(())
}
