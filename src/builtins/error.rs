// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::borrow::Cow;

use gc_lite::GcNode;

use crate::{
    FieldDef, JS_CLASS_ERROR,
    atom::Atom,
    builtins::iterator::iterator_to_array,
    context::ContextPtr,
    error::{ErrorKind, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL, JSResult},
    function::CallContext,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, ObjectPayload},
    value::JSValue,
};

pub fn js_error_constructor(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let mut ctx = cc.ctx_ptr();

    let mut new_target = *cc.this();
    if new_target.is_undefined() {
        new_target = ctx.rt_ref().current_function();
    }

    // use proper proto, ensure proto is an object
    let mut proto = new_target.get_property(&mut ctx, Atom::CONST_prototype)?;
    if !proto.is_object() {
        let ctx_p = ContextPtr::from(ctx.gc_ref());
        let realm = new_target
            .as_object()
            .map_or(Ok(ctx_p), |o| o.realm(ctx_p))?;

        proto = if magic < 0 {
            realm.class_proto(JS_CLASS_ERROR).unwrap().into()
        } else {
            realm.error_protos[magic as usize]
        };
    }
    debug_assert!(proto.is_object());

    let mut obj = ctx
        .create_object_with_proto(Object::try_from(proto).ok(), ObjectPayload::ERROR)
        .map_err(|_| -1)?;

    let mut arg_index = if magic == ErrorKind::Aggregate as i32 {
        1
    } else {
        0
    };
    let message = cc.arg(arg_index);
    arg_index += 1;

    if !message.is_undefined() {
        let msg = message.to_string(&mut ctx)?;
        obj.define_property_value(
            &mut ctx,
            Atom::CONST_message,
            msg.gc_ref().into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    if arg_index < cc.length() {
        let options = cc.arg(arg_index);

        if options.is_object() && options.has_property(&mut ctx, Atom::CONST_cause)? {
            let cause = options.get_property(&mut ctx, Atom::CONST_cause)?;
            obj.define_property_value(
                &mut ctx,
                Atom::CONST_cause,
                cause,
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
        }
    }

    if magic == ErrorKind::Aggregate as i32 {
        let error_list = iterator_to_array(ctx, cc.arg(0))?;
        obj.define_property_value(
            &mut ctx,
            Atom::CONST_errors,
            error_list,
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }

    // skip the Error() function in the backtrace
    ctx.build_backtrace_to(obj.into(), None, JS_BACKTRACE_FLAG_SKIP_FIRST_LEVEL);

    Ok(obj.into())
}

pub(crate) fn js_aggregate_error_constructor(mut ctx: ContextPtr, errors: JSValue) -> JSValue {
    let err_proto = ctx.error_protos[ErrorKind::Aggregate as usize]
        .as_object()
        .copied();
    
    if let Ok(mut obj) = ctx.create_object_with_proto(err_proto, ObjectPayload::ERROR) {
        obj.define_property_value(
            &mut ctx,
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

pub fn js_error_toString(cc: CallContext) -> JSResult {
    let mut ctx = cc.ctx_ptr();

    if !cc.this().is_object() {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }

    let name = cc.this().get_property(&mut ctx, Atom::CONST_name)?;
    let name = if name.is_undefined() {
        Atom::CONST_Error.force_string(&mut ctx)?
    } else {
        name.to_string(&mut ctx)?
    };

    let message = cc.this().get_property(&mut ctx, Atom::CONST_message)?;
    let message = if message.is_undefined() {
        Atom::CONST_empty_string.force_string(&mut ctx)?
    } else {
        message.to_string(&mut ctx)?
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

    ctx.create_string_from_str(&txt).map(JSValue::from)
}

pub const js_error_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("toString", 0, js_error_toString, 0),
    FieldDef::STR("name", "Error", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
    FieldDef::STR("message", "", JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE),
];
