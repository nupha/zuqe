// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Callback,
    atom::Atom,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunction},
    object::{JS_PROP_C_W_E, ObjectPayload},
    value::JSValue,
};

use super::ProxyPayload;

pub fn js_proxy_constructor(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

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
    let mut ctx = cc.ctx_ptr();

    let proxy = js_proxy_constructor(CallContext::new_simple(
        ctx,
        JSValue::UNDEFINED,
        cc.arguments(),
    ))?;

    // create revoke function
    let func_proto = ctx.function_proto();
    let revoke_func = NativeFunction::create(
        &mut ctx,
        Callback::Func(|mut cc| -> JSResult {
            if let Some(px) = cc.data(0).as_object_Proxy() {
                px.payload().set_revoked();
                // clear attached data 0: the Proxy value
                let _ = cc.replace_data(0, JSValue::NULL);
            }
            Ok(JSValue::UNDEFINED)
        }),
        "",
        0,
        0,
        Some(func_proto),
        Some([proxy].into()),
    )?;

    let mut obj = ctx.create_base_object()?;
    obj.define_property_value(&mut ctx, Atom::CONST_proxy, proxy, JS_PROP_C_W_E)?;
    obj.define_property_value(
        &mut ctx,
        Atom::CONST_revoke,
        revoke_func.into(),
        JS_PROP_C_W_E,
    )?;

    Ok(obj.into())
}

pub const js_proxy_funcs: &[FieldDef] = &[FieldDef::CALL("revocable", 2, js_proxy_revocable, 0)];
