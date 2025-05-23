use crate::{
    array_buffer::js_array_buffer_constructor2,
    class::JSClassID,
    context::JSContext,
    error::{JSResult, JS_ThrowTypeErrorDetachedArrayBuffer},
    function::{CallArgs, JSCFunctionListEntry, JS_CallConstructor},
    js_core::js_get_this,
    value::{
        object::{JS_SpeciesConstructor, JS_PROP_CONFIGURABLE},
        JSValue, JSValueRc, JS_FreeValue, JS_ToInt64Clamp,
    },
    JS_CFUNC_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_CGETSET_MAGIC_DEF, JS_CLASS_ARRAY_BUFFER,
    JS_CLASS_DATAVIEW, JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8C_ARRAY, JS_PROP_STRING_DEF,
};

use super::JSArrayBuffer;

pub unsafe fn js_array_buffer_isView(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let mut res = false;
    if (*argv.offset(0)).is_object() {
        let p = (*argv.offset(0)).object_unchecked();
        if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_DATAVIEW).contains(&p.class_id()) {
            res = true;
        }
    }
    res.into()
}

pub const js_array_buffer_funcs: [JSCFunctionListEntry; 2] = [
    JS_CFUNC_DEF!(c"isView", 1, js_array_buffer_isView),
    JS_CGETSET_DEF!(c"[Symbol.species]", js_get_this, NONE),
];

pub const js_shared_array_buffer_funcs: [JSCFunctionListEntry; 1] =
    [JS_CGETSET_DEF!(c"[Symbol.species]", js_get_this, NONE)];

//
// Prototype
//

pub unsafe fn js_array_buffer_get_byteLength(
    ctx: *mut JSContext,
    this_val: JSValue,
    class_id: i32,
) -> JSValue {
    this_val
        .object_unchecked()
        .isa()
        .array_buffer()
        .map_or(
            JSValue::EXCEPTION,
            |p| JSValue::from(p.byte_length() as i32),
        )
}

pub fn js_array_buffer_slice(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let abuf = unsafe {
        args.this_raw()
            .object_unchecked()
            .isa()
            .array_buffer()
            .ok_or(-1)?
    };

    if abuf.detached {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let len = abuf.byte_length() as i64;

    let mut start: i64 = 0;
    if unsafe { JS_ToInt64Clamp(ctx, &mut start, args.get_raw(0), 0, len, len) } != 0 {
        return Err(-1);
    }

    let mut end = len;
    if !args.get_raw(1).is_undefined() {
        if unsafe { JS_ToInt64Clamp(ctx, &mut end, args.get_raw(1), 0, len, len) } != 0 {
            return Err(-1);
        }
    }

    let new_len = std::cmp::max(end - start, 0);
    let ctor = unsafe {
        JS_SpeciesConstructor(ctx, args.this_raw(), JSValue::UNDEFINED).check_exception()?
    };

    let new_obj: JSValueRc = if ctor.is_undefined() {
        unsafe {
            js_array_buffer_constructor2(
                ctx,
                JSValue::UNDEFINED,
                new_len as usize,
                args.magic as JSClassID,
            )
        }
    } else {
        let mut args = [JSValue::from(new_len)];
        let o = unsafe { JS_CallConstructor(ctx, ctor, 1, args.as_mut_ptr()) };
        JS_FreeValue(ctx, ctor);
        JS_FreeValue(ctx, args[0]);
        o
    }
    .check_exception()?
    .into_rc(ctx.rt());

    if new_obj.is_same(args.this_raw()) {
        ctx.JS_ThrowTypeError(c"cannot use identical ArrayBuffer");
        return Err(-1);
    }

    let new_abuf = unsafe {
        if new_obj.object_unchecked().isa().class_id() != args.magic as JSClassID {
            return Err(-1);
        }
        new_obj
            .object_unchecked()
            .isa_mut()
            .array_buffer_mut()
            .ok_or(-1)?
    };

    if new_abuf.detached {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    if new_abuf.byte_length() < new_len as usize {
        ctx.JS_ThrowTypeError(c"new ArrayBuffer is too small");
        return Err(-1);
    }

    // must test again because of side effects
    if abuf.detached {
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

pub const js_array_buffer_proto_funcs: [JSCFunctionListEntry; 3] = [
    JS_CGETSET_MAGIC_DEF!(
        c"byteLength",
        js_array_buffer_get_byteLength,
        NONE,
        JS_CLASS_ARRAY_BUFFER as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_ARRAY_BUFFER as i16
    ),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"ArrayBuffer",
        JS_PROP_CONFIGURABLE
    ),
];

pub const js_shared_array_buffer_proto_funcs: [JSCFunctionListEntry; 3] = [
    JS_CGETSET_MAGIC_DEF!(
        c"byteLength",
        js_array_buffer_get_byteLength,
        NONE,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"slice",
        2,
        js_array_buffer_slice,
        JS_CLASS_SHARED_ARRAY_BUFFER as i16
    ),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"SharedArrayBuffer",
        JS_PROP_CONFIGURABLE as i16
    ),
];
