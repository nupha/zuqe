use std::cell::Cell;

use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::consts::{JS_ATOM_proxy, JS_ATOM_revoke},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{JS_PROP_C_W_E, JS_SetConstructorBit, ObjectPayload},
    proxy::{Proxy, ProxyObjPayload},
    value::JSValue,
};

pub fn js_proxy_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(target) = cc.arg(0).as_object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let Some(handler) = cc.arg(1).as_object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let obj: JSValue = ctx
        .create_object_from_proto(
            JSValue::NULL,
            ObjectPayload::PROXY(Box::new(ProxyObjPayload {
                target: target.clone(),
                handler: handler.clone().into(),
                is_func: target.is_function(),
                is_revoked: Cell::new(false),
            })),
        )
        .map_err(|_| -1)?
        .into();

    JS_SetConstructorBit(ctx, &obj, target.is_constructor());
    Ok(obj)
}

fn js_proxy_revocable(ctx: &JSContext, cc: CallContext) -> JSResult {
    let proxy_obj = js_proxy_constructor(
        ctx,
        CallContext::new(ctx, JSValue::UNDEFINED, cc.arguments(), 0, None),
    )?;

    // create revoke function
    let revoke_obj = ctx.create_native_function_with_data(
        |_, mut cc| -> JSResult {
            if let Ok(p) = Proxy::try_from(cc.data(0)) {
                p.payload().is_revoked.set(true);
                // clear attached data (Proxy value)
                let _ = cc.replace_data(0, JSValue::NULL);
            }
            Ok(JSValue::UNDEFINED)
        },
        0,
        0,
        [proxy_obj.clone()].into(),
    )?;

    let obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, JS_ATOM_proxy, proxy_obj.into(), JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, JS_ATOM_revoke, revoke_obj.into(), JS_PROP_C_W_E)?;

    Ok(obj.into())
}

pub static js_proxy_funcs: [FieldDef; 1] =
    [JS_CFUNC_NATIVE_DEF!("revocable", 2, js_proxy_revocable, 0)];
