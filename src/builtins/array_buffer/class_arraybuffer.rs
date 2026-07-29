// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY,
    builtins::js_get_this,
    class::ClassID,
    context::ScopedContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef},
    object::{JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    utils::unlikely,
    value::JSValue,
};

use super::ArrayBuffer;

/// Get the `ArrayBuffer` payload for `this` with the given `class_id`.
/// Throws TypeError if `this` is not an object of the expected class.
fn get_array_buffer(
    this: JSValue,
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<ArrayBuffer, i32> {
    this.as_object()
        .filter(|o| o.class_id() == class_id)
        .map(|o| ArrayBuffer(o.gc_ref()))
        .ok_or_else(|| ctx.throw_type_error_invalid_class(class_id))
}

fn js_array_buffer_get_byteLength(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;
    let ab = get_array_buffer(cc.this(), ctx, class_id)?;
    Ok(JSValue::from(ab.payload().byte_length() as i64))
}

fn js_array_buffer_detached(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let ab = get_array_buffer(cc.this(), ctx, JS_CLASS_ARRAY_BUFFER)?;
    Ok(JSValue::from(ab.payload().is_detached()))
}

fn js_array_buffer_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;
    let this = cc.this();

    let ab = get_array_buffer(this, ctx, class_id)?;
    let abuf = ab.payload();

    if abuf.is_detached() {
        return Err(JS_ThrowTypeErrorDetachedArrayBuffer(ctx));
    }

    let len = abuf.byte_length() as i64;
    let start: i64 = cc.arg(0).to_number(ctx)?.to_i64_clamp(0, len, len);
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_number(ctx)?.to_i64_clamp(0, len, len)
    } else {
        len
    };

    let new_len = std::cmp::max(end - start, 0);
    let ctor = JS_SpeciesConstructor(ctx, this, JSValue::UNDEFINED)?;

    let new_obj = if ctor.is_undefined() {
        ArrayBuffer::with_capacity_ex(
            ctx,
            JSValue::UNDEFINED,
            class_id == JS_CLASS_SHARED_ARRAY_BUFFER,
            new_len as usize,
            std::ptr::null_mut(),
        )
        .map(|x| x.into())
    } else {
        ctor.call_ctor(ctx, &[JSValue::from(new_len)])
    }?;

    let mut new_ab = get_array_buffer(new_obj, ctx, class_id)?;

    if unlikely(new_obj.is_same(this)) {
        Err(ctx.throw_type_error("cannot use identical ArrayBuffer"))
    } else if unlikely(new_ab.payload().is_detached()) {
        Err(JS_ThrowTypeErrorDetachedArrayBuffer(ctx))
    } else if unlikely(new_ab.payload().byte_length() < new_len as usize) {
        Err(ctx.throw_type_error("new ArrayBuffer is too small"))
    } else {
        // must test again because of side effects
        if abuf.is_detached() {
            return Err(JS_ThrowTypeErrorDetachedArrayBuffer(ctx));
        }

        let new_abuf_mut = new_ab.payload_mut();

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

fn js_array_buffer_transfer(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut ab = get_array_buffer(cc.this(), ctx, JS_CLASS_ARRAY_BUFFER)?;

    if ab.is_detached() {
        return Err(JS_ThrowTypeErrorDetachedArrayBuffer(ctx));
    }

    let old_len = ab.payload().byte_length();

    let new_len = if cc.length() < 1 || cc.arg(0).is_undefined() {
        old_len
    } else {
        cc.arg(0).to_index(ctx)? as usize
    };

    let copy_len = std::cmp::min(old_len, new_len);

    // Read old data before creating new ArrayBuffer to avoid borrow conflicts
    let src = if copy_len > 0 {
        let bytes = ab.payload().bytes();
        bytes[..copy_len].to_vec()
    } else {
        Vec::new()
    };

    // Create new ArrayBuffer
    let mut new_ab = ArrayBuffer::create(ctx, false, new_len)?;

    // Copy data into new buffer
    if copy_len > 0 {
        new_ab.payload_mut().with_bytes_mut(|dst| {
            dst[..copy_len].copy_from_slice(&src);
        });
    }

    // Detach old ArrayBuffer
    ab.detatch(ctx);

    Ok(JSValue::from(new_ab))
}

pub const js_array_buffer_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET(
        "byteLength",
        Some(js_array_buffer_get_byteLength),
        None,
        JS_CLASS_ARRAY_BUFFER as i16,
    ),
    FieldDef::GETSET("detached", Some(js_array_buffer_detached), None, 0),
    FieldDef::FUNC(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_ARRAY_BUFFER as i16,
    ),
    FieldDef::FUNC("transfer", 0, js_array_buffer_transfer, 0),
    FieldDef::FUNC("transferToFixedLength", 0, js_array_buffer_transfer, 1),
    FieldDef::STR("[Symbol.toStringTag]", "ArrayBuffer", JS_PROP_CONFIGURABLE),
];

pub const js_shared_array_buffer_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET(
        "byteLength",
        Some(js_array_buffer_get_byteLength),
        None,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16,
    ),
    FieldDef::FUNC(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16,
    ),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "SharedArrayBuffer",
        JS_PROP_CONFIGURABLE,
    ),
];

//
// static methods
//

fn js_array_buffer_isView(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    let b = cc
        .arg(0)
        .as_object()
        .is_some_and(|p| (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_DATAVIEW).contains(&p.class_id()));
    Ok(JSValue::from(b))
}

pub const js_array_buffer_funcs: &[FieldDef] = &[
    FieldDef::FUNC("isView", 1, js_array_buffer_isView, 0),
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

pub const js_shared_array_buffer_funcs: &[FieldDef] = &[FieldDef::GETSET(
    "[Symbol.species]",
    Some(js_get_this),
    None,
    0,
)];
