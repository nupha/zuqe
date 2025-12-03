use crate::{
    atom::Atom,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunctionWithData},
    object::{JS_PROP_C_W_E, ObjectPayload},
    value::JSValue,
};

use super::ProxyPayload;

pub fn js_proxy_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.context();

    let Some(target) = cc.arg(0).as_object().copied() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let Some(handler) = cc.arg(1).as_object().copied() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let mut obj = ctx
        .create_object_with_proto(
            None,
            ObjectPayload::PROXY(ProxyPayload::new(target, handler.into())),
        )
        .map_err(|_| -1)?;

    obj.set_flag_constructor(target.is_constructor());

    Ok(obj.into())
}

fn js_proxy_revocable(cc: CallContext) -> JSResult {
    let cc2 = CallContext::new_simple(cc.ctx_ptr(), JSValue::UNDEFINED, cc.arguments());
    let proxy = js_proxy_constructor(cc2)?;

    let ctx = cc.ctx();

    // create revoke function
    let revoke_obj = NativeFunctionWithData::create(
        ctx,
        |mut cc| -> JSResult {
            if let Some(px) = cc.data(0).as_object_Proxy() {
                px.payload().set_revoked();
                // clear attached data (Proxy value)
                let _ = cc.replace_data(0, JSValue::NULL);
            }
            Ok(JSValue::UNDEFINED)
        },
        0,
        0,
        [proxy].into(),
    )?;

    let mut obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, Atom::CONST_proxy, proxy, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, Atom::CONST_revoke, revoke_obj.into(), JS_PROP_C_W_E)?;

    Ok(obj.into())
}

pub static js_proxy_funcs: [FieldDef; 1] = [FieldDef::CALL("revocable", 2, js_proxy_revocable, 0)];
