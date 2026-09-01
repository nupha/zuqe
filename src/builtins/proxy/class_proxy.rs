// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Callback, JS_CLASS_PROXY, ScopedContext,
    atom::Atom,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunction},
    object::{JS_PROP_C_W_E, PayloadPtr},
    value::JSValue,
};

use super::ProxyPayload;

pub fn js_proxy_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(target) = cc.arg(0).as_object() else {
        return Err(ctx.throw_type_error_not_an_object());
    };

    let Some(handler) = cc.arg(1).as_object() else {
        return Err(ctx.throw_type_error_not_an_object());
    };

    let mut obj = ctx.create_instance_using_proto(
        None,
        PayloadPtr::new(JS_CLASS_PROXY, ProxyPayload::new(target, handler.into())),
    )?;

    obj.set_flag_constructor(target.is_constructor());

    Ok(obj.into())
}

fn js_proxy_revocable(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let proxy = js_proxy_constructor(
        ctx,
        CallContext::new_simple(JSValue::UNDEFINED, cc.arguments()),
    )?;

    // create revoke function
    let func_proto = ctx.function_proto();
    let revoke_func = NativeFunction::create(
        ctx,
        Callback::Func(|_, mut cc| -> JSResult {
            if let Some(px) = cc.data(0).as_object_Proxy() {
                px.payload().set_revoked();
                // clear attached data 0: the Proxy value
                let _ = cc.replace_data(0, JSValue::NULL);
            }
            Ok(JSValue::UNDEFINED)
        }),
        Atom::CONST_empty_string,
        0,
        0,
        func_proto,
        Some([proxy].into()),
    )?;

    let mut obj = ctx.create_base_object()?;
    obj.define_property_value(ctx, Atom::CONST_proxy, proxy, JS_PROP_C_W_E)?;
    obj.define_property_value(ctx, Atom::CONST_revoke, revoke_func.into(), JS_PROP_C_W_E)?;

    Ok(obj.into())
}

pub const js_proxy_funcs: &[FieldDef] = &[FieldDef::FUNC("revocable", 2, js_proxy_revocable, 0)];
