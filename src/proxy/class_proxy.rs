use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::consts::{JS_ATOM_proxy, JS_ATOM_revoke},
    callable::NativeFunctionWithData,
    class::ClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunctionType},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JSObject, ObjectPayload},
    proxy::ProxyPayload,
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

    let obj = ctx
        .create_object_with_proto(
            None,
            ObjectPayload::PROXY(ProxyPayload::new(target.clone(), handler.clone().into())),
        )
        .map_err(|_| -1)?;

    obj.set_flag_constructor(target.is_constructor());

    Ok(obj.into())
}

fn js_proxy_revocable(ctx: &JSContext, cc: CallContext) -> JSResult {
    let proxy_obj = js_proxy_constructor(
        ctx,
        CallContext::new(ctx, JSValue::UNDEFINED, cc.arguments(), 0, None),
    )?;

    // create revoke function
    let revoke_obj = NativeFunctionWithData::create(
        ctx,
        |_, mut cc| -> JSResult {
            if let Some(p) = cc.data(0).as_object_Proxy() {
                p.payload().set_revoked();
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
    obj.define_property_value(ctx, &JS_ATOM_proxy, proxy_obj, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, &JS_ATOM_revoke, revoke_obj.into(), JS_PROP_C_W_E)?;

    Ok(obj.into())
}

pub static js_proxy_funcs: [FieldDef; 1] =
    [JS_CFUNC_NATIVE_DEF!("revocable", 2, js_proxy_revocable, 0)];

pub fn class_proxy_init(ctx: &JSContext, _: ClassID) -> Result<JSObject, i32> {
    let proto = ctx.create_native_function(
        NativeFunctionType::Constructor(js_proxy_constructor),
        "Proxy",
        2,
        0,
    )?;

    proto.set_flag_constructor(true);
    proto.install_field_defs(ctx, &js_proxy_funcs).unwrap();

    ctx.global_obj().define_property_value_str(
        ctx,
        "Proxy",
        proto.clone().into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;

    Ok(proto)
}
