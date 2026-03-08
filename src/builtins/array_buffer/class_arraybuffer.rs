// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY,
    ScopedContext,
    builtins::js_get_this,
    class::ClassID,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef},
    object::{JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    utils::unlikely,
    value::JSValue,
};

use super::ArrayBuffer;

//
// Prototype
//

fn js_array_buffer_get_byteLength(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    cc.this()
        .retrive_object_payload(ctx, class_id)
        .map(|isa| JSValue::from(isa.array_buffer().unwrap().byte_length() as i64))
}

fn js_array_buffer_detached(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this()
        .as_object()
        .filter(|o| o.class_id() == JS_CLASS_ARRAY_BUFFER)
        .and_then(|o| {
            o.payload()
                .array_buffer()
                .map(|p| JSValue::Bool(p.is_detached()))
        })
        .ok_or_else(|| {
            ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_BUFFER);
            -1
        })
}

fn js_array_buffer_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    let this = cc.this();
    let abuf = this
        .retrive_object_payload(ctx, class_id)
        .map(|isa| isa.array_buffer().unwrap())?;

    if abuf.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let len = abuf.byte_length() as i64;
    let start: i64 = cc.arg(0).to_i64_clamp(ctx, 0, len, len)?;
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i64_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    let new_len = std::cmp::max(end - start, 0);
    let ctor = JS_SpeciesConstructor(ctx, this, JSValue::UNDEFINED)?;

    let mut new_obj = if ctor.is_undefined() {
        ArrayBuffer::with_capacity_ex(
            ctx,
            JSValue::UNDEFINED,
            cc.magic() as ClassID == JS_CLASS_SHARED_ARRAY_BUFFER,
            new_len as usize,
            std::ptr::null_mut(),
        )
        .map(|x| x.into())
    } else {
        ctor.call_ctor(ctx, &[JSValue::from(new_len)])
    }?;

    let new_abuf = new_obj
        .retrive_object_payload(ctx, class_id)
        .map(|isa| isa.array_buffer().unwrap())?;

    if unlikely(new_obj.is_same(this)) {
        ctx.throw_type_error("cannot use identical ArrayBuffer");
        Err(-1)
    } else if unlikely(new_abuf.is_detached()) {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        Err(-1)
    } else if unlikely(new_abuf.byte_length() < new_len as usize) {
        ctx.throw_type_error("new ArrayBuffer is too small");
        Err(-1)
    } else {
        // must test again because of side effects
        if abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let new_abuf_mut = new_obj
            .retrive_object_payload_mut(ctx, class_id)
            .map(|isa| isa.array_buffer_mut().unwrap())?;

        abuf.with_bytes(|src| {
            new_abuf_mut.with_bytes_mut(|dst| unsafe {
                std::ptr::copy_nonoverlapping(
                    src.as_ptr().offset(start as isize),
                    dst.as_mut_ptr(),
                    new_len as usize,
                );
            })
        });

        Ok(new_obj)
    }
}

pub const js_array_buffer_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET(
        "byteLength",
        Some(js_array_buffer_get_byteLength),
        None,
        JS_CLASS_ARRAY_BUFFER as i16,
    ),
    FieldDef::GETSET("detached", Some(js_array_buffer_detached), None, 0),
    FieldDef::CALL(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_ARRAY_BUFFER as i16,
    ),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "ArrayBuffer", JS_PROP_CONFIGURABLE),
];

pub const js_shared_array_buffer_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET(
        "byteLength",
        Some(js_array_buffer_get_byteLength),
        None,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16,
    ),
    FieldDef::CALL(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16,
    ),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "SharedArrayBuffer",
        JS_PROP_CONFIGURABLE,
    ),
];

//
// static methods
//

fn js_array_buffer_isView(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let res = if let JSValue::Object(p) = cc.arg(0)
        && (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_DATAVIEW).contains(&p.class_id())
    {
        true
    } else {
        false
    };
    Ok(JSValue::Bool(res))
}

pub const js_array_buffer_funcs: &[FieldDef] = &[
    FieldDef::CALL("isView", 1, js_array_buffer_isView, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

pub const js_shared_array_buffer_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];
