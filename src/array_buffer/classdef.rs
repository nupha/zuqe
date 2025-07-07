use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_CGETSET_MAGIC_DEF, JS_CLASS_ARRAY_BUFFER,
    JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY, JS_PROP_STRING_DEF,
    class::JSClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, JS_CallConstructor, NativeFieldDef},
    js_core::js_get_this,
    object::{JS_PROP_CONFIGURABLE, JS_SpeciesConstructor},
    value::JSValue,
};

pub fn js_array_buffer_isView(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut res = false;
    if let JSValue::Object(p) = cc.arg(0) {
        if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_DATAVIEW).contains(&p.class_id()) {
            res = true;
        }
    }
    Ok(JSValue::Bool(res))
}

pub const js_array_buffer_funcs: [NativeFieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("isView", 1, js_array_buffer_isView, 0),
    JS_CGETSET_DEF!("[Symbol.species]", js_get_this, NONE),
];

pub const js_shared_array_buffer_funcs: [NativeFieldDef; 1] =
    [JS_CGETSET_DEF!("[Symbol.species]", js_get_this, NONE)];

//
// Prototype
//

pub unsafe fn js_array_buffer_get_byteLength(
    ctx: &JSContext,
    this_val: JSValue,
    class_id: i32,
) -> JSValue {
    this_val
        .object_unchecked()
        .0
        .isa
        .borrow()
        .array_buffer()
        .map_or(
            JSValue::EXCEPTION,
            |p| JSValue::from(p.byte_length() as i32),
        )
}

pub fn js_array_buffer_slice(ctx: &JSContext, cc: CallContext) -> JSResult {
    let abuf = unsafe {
        cc.this()
            .object_unchecked()
            .0
            .isa
            .borrow()
            .array_buffer()
            .cloned()
            .ok_or(-1)?
    };

    if abuf.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let len = abuf.byte_length() as i64;
    let start: i64 = cc.arg(0).coerce_to_i64_clamp(ctx, 0, len, len)?;
    let end = if !cc.arg(1).is_undefined() {
        cc.arg(1).coerce_to_i64_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    let new_len = std::cmp::max(end - start, 0);
    let ctor = JS_SpeciesConstructor(ctx, cc.this(), JSValue::UNDEFINED)?;

    let new_obj = if ctor.is_undefined() {
        ctx.create_array_buffer(
            &JSValue::UNDEFINED,
            cc.magic() as JSClassID,
            new_len as usize,
            std::ptr::null_mut(),
        )
    } else {
        JS_CallConstructor(ctx, &ctor, &[JSValue::from(new_len)]).check_exception()
    }?;

    if new_obj.is_same(cc.this()) {
        ctx.JS_ThrowTypeError("cannot use identical ArrayBuffer");
        return Err(-1);
    }

    if unsafe { new_obj.object_unchecked() }
        .0
        .isa
        .borrow()
        .class_id()
        != cc.magic() as JSClassID
    {
        return Err(-1);
    }

    let mut new_abuf = unsafe { new_obj.object_unchecked() }
        .0
        .isa
        .borrow()
        .array_buffer()
        .cloned()
        .ok_or(-1)?;

    if new_abuf.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    if new_abuf.byte_length() < new_len as usize {
        ctx.JS_ThrowTypeError("new ArrayBuffer is too small");
        return Err(-1);
    }

    // must test again because of side effects
    if abuf.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    abuf.with_bytes(|src| {
        new_abuf.with_bytes_mut(|dst| unsafe {
            std::ptr::copy_nonoverlapping(
                src.as_ptr().offset(start as isize),
                dst.as_mut_ptr(),
                new_len as usize,
            );
        })
    });

    Ok(new_obj.into())
}

pub const js_array_buffer_proto_funcs: [NativeFieldDef; 3] = [
    JS_CGETSET_MAGIC_DEF!(
        "byteLength",
        js_array_buffer_get_byteLength,
        NONE,
        JS_CLASS_ARRAY_BUFFER as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_ARRAY_BUFFER as i16
    ),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "ArrayBuffer", JS_PROP_CONFIGURABLE),
];

pub const js_shared_array_buffer_proto_funcs: [NativeFieldDef; 3] = [
    JS_CGETSET_MAGIC_DEF!(
        "byteLength",
        js_array_buffer_get_byteLength,
        NONE,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16
    ),
    JS_PROP_STRING_DEF!(
        "[Symbol.toStringTag]",
        "SharedArrayBuffer",
        JS_PROP_CONFIGURABLE as i16
    ),
];
