// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{borrow::Cow, ops::Deref};

use crate::{
    ClassCtorDef, FieldDef, JS_CLASS_ERROR,
    atom::Atom,
    builtins::iterator::iterator_to_array,
    context::{ContextPtr, ScopedContext},
    error::{ErrorKind, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL, JSResult},
    function::CallContext,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, PayloadPtr},
    value::JSValue,
};

pub(crate) fn js_error_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    let mut new_target = cc.this();
    if new_target.is_undefined() {
        new_target = ctx.rt().current_function();
    }

    // use proper proto, ensure proto is an object
    let mut proto = new_target.get_property(ctx, Atom::CONST_prototype)?;
    if !proto.is_object() {
        let realm: ContextPtr = new_target
            .as_object()
            .map_or(Ok(ctx.into()), |o| o.realm(ctx.into()))?;

        proto = if magic < 0 {
            realm.deref().class_proto(JS_CLASS_ERROR).unwrap().into()
        } else {
            realm.error_protos[magic as usize]
        };
    }
    debug_assert!(proto.is_object());

    let mut obj = ctx
        .create_instance_using_proto(
            Object::try_from(proto).ok(),
            PayloadPtr::none(JS_CLASS_ERROR),
        )
        .map_err(|_| -1)?;

    let mut arg_index = if magic == ErrorKind::Aggregate as i32 {
        1
    } else {
        0
    };
    let message = cc.arg(arg_index);
    arg_index += 1;

    if !message.is_undefined() {
        let msg = message.to_string(ctx)?;
        obj.define_property_value(
            ctx,
            Atom::CONST_message,
            JSValue::make_string(msg),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    if arg_index < cc.length() {
        let options = cc.arg(arg_index);

        if options.is_object() && options.has_property(ctx, Atom::CONST_cause)? {
            let cause = options.get_property(ctx, Atom::CONST_cause)?;
            obj.define_property_value(
                ctx,
                Atom::CONST_cause,
                cause,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
        }
    }

    if magic == ErrorKind::Aggregate as i32 {
        let error_list = iterator_to_array(ctx, cc.arg(0))?;
        obj.define_property_value(
            ctx,
            Atom::CONST_errors,
            error_list,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    // skip the Error() function in the backtrace
    ctx.build_backtrace_to(obj.into(), None, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL);

    Ok(obj.into())
}

pub(crate) fn js_aggregate_error_constructor(ctx: &mut ScopedContext, errors: JSValue) -> JSValue {
    let err_proto = ctx.error_protos[ErrorKind::Aggregate as usize].as_object();

    if let Ok(mut obj) =
        ctx.create_instance_using_proto(err_proto, PayloadPtr::none(JS_CLASS_ERROR))
    {
        obj.define_property_value(
            ctx,
            Atom::CONST_errors,
            errors,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )
        .unwrap();

        obj.into()
    } else {
        JSValue::EXCEPTION
    }
}

pub fn js_error_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this().ensure_object(ctx)?;

    let name = this.get_property(ctx, Atom::CONST_name)?;
    let name = if name.is_undefined() {
        Atom::CONST_Error.force_string(ctx)?
    } else {
        name.to_string(ctx)?
    };

    let message = this.get_property(ctx, Atom::CONST_message)?;
    let message = if message.is_undefined() {
        Atom::CONST_empty_string.force_string(ctx)?
    } else {
        message.to_string(ctx)?
    };

    let txt: Cow<'_, str> = match (name.is_empty(), message.is_empty()) {
        (false, false) => format!(
            "{}: {}",
            name.encode_utf8(false),
            message.encode_utf8(false)
        )
        .into(),
        (false, true) => name.encode_utf8(false),
        (true, false) => message.encode_utf8(false),
        (true, true) => "".into(),
    };

    ctx.create_string_from_str(&txt).map(JSValue::make_string)
}

pub fn js_error_isError(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    Ok(JSValue::from(val.is_error()))
}

const js_error_funcs: &[FieldDef] = &[FieldDef::FUNC("isError", 1, js_error_isError, 0)];

pub(crate) const js_error_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_error_toString, 0),
    FieldDef::STR("name", "Error", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
    FieldDef::STR("message", "", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
];

crate::def_native_object!(
    builtin,
    "Error",
    CLASS_DEF_ERROR,
    class_id: (JS_CLASS_ERROR, Atom::CONST_Error),
    (),
    js_error_proto_funcs,
    Some(
        ClassCtorDef::new(js_error_constructor, 1, true)
            .magic(-1)
            .entries(js_error_funcs)
            .expose_to_global(),
    )
);
