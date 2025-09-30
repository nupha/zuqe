use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::consts::{JS_ATOM_false, JS_ATOM_true},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::ObjectPayload,
    value::JSValue,
};

pub fn js_boolean_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = JSValue::Bool(cc.arg(0).to_bool().unwrap_or(true));

    let new_target = cc.this();
    if !new_target.is_undefined() {
        ctx.create_object_from_ctor(new_target, ObjectPayload::BOOLEAN(val))
            .map(JSValue::Object)
            .map_err(|_| -1)
    } else {
        Ok(val)
    }
}

pub fn js_thisBooleanValue(ctx: &JSContext, this_val: JSValue) -> JSResult {
    if this_val.is_bool() {
        Ok(this_val)
    } else if let Some(d) = this_val
        .as_object()
        .and_then(|p| p.payload().value())
        .and_then(|x| if x.is_bool() { Some(x) } else { None })
    {
        Ok(d.clone())
    } else {
        ctx.throw_type_error("not a boolean");
        Err(-1)
    }
}

pub fn js_boolean_toString(ctx: &JSContext, args: CallContext) -> JSResult {
    match js_thisBooleanValue(ctx, args.this_raw())? {
        JSValue::Bool(true) => JS_ATOM_true,
        JSValue::Bool(false) => JS_ATOM_false,
        _ => {
            ctx.throw_type_error("not a boolean");
            return Err(-1);
        }
    }
    .force_string(ctx)
    .map(JSValue::String)
}

pub fn js_boolean_valueOf(ctx: &JSContext, args: CallContext) -> JSResult {
    js_thisBooleanValue(ctx, args.this_raw())
}

pub static js_boolean_proto_funcs: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_boolean_toString, 0),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_boolean_valueOf, 0),
];
