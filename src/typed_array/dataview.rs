use std::ptr::NonNull;

use crate::{
    class::{js_create_from_ctor, JSClassID},
    context::JSContext,
    error::{JSResult, JS_ThrowRangeError, JS_ThrowTypeErrorDetachedArrayBuffer},
    function::{CallArgs, JSCFunctionListEntry},
    mem::js_malloc_typed,
    types::list_add_tail,
    value::{
        object::{JSObject, JS_PROP_CONFIGURABLE},
        JSValue, JS_FreeValue, JS_ToBool, JS_ToFloat64, JS_ToIndex, JS_ToUint32,
    },
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_MAGIC_DEF, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_DATAVIEW, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    JS_CLASS_UINT8_ARRAY, JS_PROP_STRING_DEF,
};

use super::{
    js_typed_array_get_buffer, js_typed_array_get_byteLength, js_typed_array_get_byteOffset,
    typed_array_size_log2, JSTypedArray,
};

impl JSContext {
    /// create a DataView with given array buffer.
    pub fn create_dataview(
        &mut self,
        new_target: JSValue,
        buffer: JSValue,
        offset: usize,
        length: Option<usize>,
    ) -> JSResult {
        let abuf = if let Ok(ab) = buffer.array_buffer() {
            ab
        } else {
            self.JS_ThrowTypeError(c"expect an ArrayBuffer");
            return Err(-1);
        };

        if abuf.detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(self);
            return Err(-1);
        }

        if offset > abuf.byte_length as usize {
            JS_ThrowRangeError(self, c"invalid byteOffset");
            return Err(-1);
        }

        let mut len = abuf.byte_length as usize - offset;
        if let Some(l) = length {
            if l > len {
                JS_ThrowRangeError(self, c"invalid byteLength");
                return Err(-1);
            } else {
                len = l;
            }
        }

        let obj =
            unsafe { js_create_from_ctor(self, new_target, JS_CLASS_DATAVIEW).check_exception()? };

        if abuf.detached {
            // check again, could have been detached in js_create_from_ctor()
            unsafe {
                JS_ThrowTypeErrorDetachedArrayBuffer(self);
                JS_FreeValue(self, obj);
            }
            return Err(-1);
        }

        let mut ta = unsafe {
            if let Some(p) = js_malloc_typed::<JSTypedArray>(self, 1, false) {
                Box::from_raw(p.as_ptr())
            } else {
                JS_FreeValue(self, obj);
                return Err(-1);
            }
        };

        unsafe {
            let p = obj.object_unchecked() as *mut JSObject;
            ta.obj = p;
            ta.buffer = NonNull::from(buffer.dup().object_unchecked()); // buffer is duplicated
            ta.offset = offset as u32;
            ta.byte_length = len as _;
            list_add_tail(&mut (*ta).link, &mut abuf.array_list);

            (*p).u.typed_array = Box::into_raw(ta); // same as JS_SetOpaque(obj, ...)
        }

        Ok(obj)
    }
}

pub unsafe fn js_dataview_constructor(
    ctx: *mut JSContext,
    new_target: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let buffer = *argv.offset(0);

    let mut offset = 0;
    if argc > 1 {
        if JS_ToIndex(ctx, &mut offset, *argv.offset(1)) != 0 {
            return JSValue::EXCEPTION;
        }
    }

    let mut length: Option<usize> = None;
    if argc > 2 && !(*argv.offset(2)).is_undefined() {
        let mut l: u64 = 0;
        if JS_ToIndex(ctx, &mut l, *argv.offset(2)) != 0 {
            return JSValue::EXCEPTION;
        }
        length = Some(l as usize);
    }

    (*ctx)
        .create_dataview(new_target, buffer, offset as _, length)
        .unwrap_or(JSValue::EXCEPTION)
}

pub fn js_dataview_getValue(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_obj = args.this_raw();
    let class_id = args.magic as JSClassID;

    let ta = unsafe {
        this_obj
            .object_unchecked()
            .opaque_must::<JSTypedArray>(JS_CLASS_DATAVIEW, ctx)?
            .as_ref()
    };

    let size = 1 << typed_array_size_log2(class_id);

    let mut pos: u64 = 0;
    if unsafe { JS_ToIndex(ctx, &mut pos, args.get_raw(0)) } != 0 {
        return Err(-1);
    }

    // is little endian?
    let le = if args.length() > 1 {
        unsafe { JS_ToBool(ctx, args.get_raw(1))? }
    } else {
        false // default to BE
    };

    let res: JSValue = unsafe {
        let abuf = ta.buffer.as_ref().u.array_buffer;

        if (*abuf).detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        if (pos + size as u64) > (*ta).byte_length as u64 {
            JS_ThrowRangeError(ctx, c"out of bound");
            return Err(-1);
        }

        let ptr = (*abuf).data.offset(ta.offset as isize).offset(pos as isize);

        match class_id {
            JS_CLASS_INT8_ARRAY => JSValue::from(*ptr.cast::<i8>() as i32),
            JS_CLASS_UINT8_ARRAY => JSValue::from(*ptr as i32),
            JS_CLASS_INT16_ARRAY => {
                let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                let n = if le {
                    i16::from_le_bytes(bb)
                } else {
                    i16::from_be_bytes(bb)
                };
                JSValue::from(n as i32)
            }
            JS_CLASS_UINT16_ARRAY => {
                let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                let n = if le {
                    u16::from_le_bytes(bb)
                } else {
                    u16::from_be_bytes(bb)
                };
                JSValue::from(n as i32)
            }
            JS_CLASS_INT32_ARRAY => {
                let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                let n = if le {
                    i32::from_le_bytes(bb)
                } else {
                    i32::from_be_bytes(bb)
                };
                JSValue::from(n)
            }
            JS_CLASS_UINT32_ARRAY => {
                let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                let n = if le {
                    u32::from_le_bytes(bb)
                } else {
                    u32::from_be_bytes(bb)
                };
                JSValue::from(n)
            }
            JS_CLASS_BIG_INT64_ARRAY => {
                ctx.JS_ThrowTypeError(c"BigInt64Array is not supported");
                return Err(-1);
            }
            JS_CLASS_BIG_UINT64_ARRAY => {
                ctx.JS_ThrowTypeError(c"BigUint64Array is not supported");
                return Err(-1);
            }
            JS_CLASS_FLOAT32_ARRAY => {
                let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                let n = if le {
                    f32::from_le_bytes(bb)
                } else {
                    f32::from_be_bytes(bb)
                };
                JSValue::from_f64_as_is(n as f64)
            }
            JS_CLASS_FLOAT64_ARRAY => {
                let bb: [u8; 8] = std::slice::from_raw_parts(ptr, 8).try_into().unwrap();
                let n = if le {
                    f64::from_le_bytes(bb)
                } else {
                    f64::from_be_bytes(bb)
                };
                JSValue::from_f64_as_is(n)
            }
            _ => {
                unreachable!();
            }
        }
    };

    Ok(res)
}

pub fn js_dataview_setValue(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_obj = args.this_raw();
    let class_id = args.magic as JSClassID;

    let ta = unsafe {
        this_obj
            .object_unchecked()
            .opaque_must::<JSTypedArray>(JS_CLASS_DATAVIEW, ctx)?
            .as_ref()
    };

    let size = 1 << typed_array_size_log2(class_id);
    let mut pos = 0_u64;
    if unsafe { JS_ToIndex(ctx, &mut pos, args.get_raw(0)) } != 0 {
        return Err(-1);
    }

    let val = args.get_raw(1);
    let mut v = 0_u32;
    let mut v64 = 0_u64;

    if class_id <= JS_CLASS_UINT32_ARRAY {
        v = unsafe { JS_ToUint32(ctx, val)? };
    } else if class_id <= JS_CLASS_BIG_UINT64_ARRAY {
        ctx.JS_ThrowTypeError(c"BigInt is not supported");
        return Err(-1);
    } else {
        let mut d: f64 = 0.0;
        if unsafe { JS_ToFloat64(ctx, &mut d, val) } != 0 {
            return Err(-1);
        }
        if class_id == JS_CLASS_FLOAT32_ARRAY {
            union U {
                f: f32,
                i: u32,
            }
            let u = U { f: d as f32 };
            v = unsafe { u.i };
        } else {
            union U {
                d: f64,
                u64: u64,
            }
            let u = U { d };
            v64 = unsafe { u.u64 };
        }
    }

    let le = if args.length() > 2 {
        unsafe { JS_ToBool(ctx, args.get_raw(2))? }
    } else {
        false
    };

    unsafe {
        let abuf = ta.buffer.as_ref().u.array_buffer;
        if (*abuf).detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }
        if (pos + size as u64) > ta.byte_length as u64 {
            JS_ThrowRangeError(ctx, c"out of bound");
            return Err(-1);
        }

        let ptr = (*abuf).data.offset(ta.offset as isize).offset(pos as isize);

        match class_id {
            JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY => {
                *ptr = v as u8;
            }
            JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY => {
                let bb = if le {
                    (v as u16).to_le_bytes()
                } else {
                    (v as u16).to_be_bytes()
                };
                std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 2);
            }
            JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY | JS_CLASS_FLOAT32_ARRAY => {
                let bb = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 4);
            }
            JS_CLASS_FLOAT64_ARRAY => {
                let bb = if le {
                    v64.to_le_bytes()
                } else {
                    v64.to_be_bytes()
                };
                std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 8);
            }
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                ctx.JS_ThrowTypeError(c"BigInt64Array, BigUint64Array is not supported");
                return Err(-1);
            }
            _ => {
                unreachable!();
            }
        }
    }

    Ok(JSValue::UNDEFINED)
}

pub const js_dataview_proto_funcs: [JSCFunctionListEntry; 20] = [
    JS_CGETSET_MAGIC_DEF!(c"buffer", js_typed_array_get_buffer, NONE, 1),
    JS_CGETSET_MAGIC_DEF!(c"byteLength", js_typed_array_get_byteLength, NONE, 1),
    JS_CGETSET_MAGIC_DEF!(c"byteOffset", js_typed_array_get_byteOffset, NONE, 1),
    JS_CFUNC_NATIVE_DEF!(
        c"getInt8",
        1,
        js_dataview_getValue,
        JS_CLASS_INT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"getUint8",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"getInt16",
        1,
        js_dataview_getValue,
        JS_CLASS_INT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"getUint16",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"getInt32",
        1,
        js_dataview_getValue,
        JS_CLASS_INT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"getUint32",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT32_ARRAY as i16
    ),
    // JS_CFUNC_NATIVE_DEF!(
    //     c"getBigInt64",
    //     1,
    //     js_dataview_getValue,
    //     JS_CLASS_BIG_INT64_ARRAY as i16
    // ),
    // JS_CFUNC_NATIVE_DEF!(
    //     c"getBigUint64",
    //     1,
    //     js_dataview_getValue,
    //     JS_CLASS_BIG_UINT64_ARRAY as i16
    // ),
    JS_CFUNC_NATIVE_DEF!(
        c"getFloat32",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"getFloat64",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT64_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setInt8",
        2,
        js_dataview_setValue,
        JS_CLASS_INT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setUint8",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setInt16",
        2,
        js_dataview_setValue,
        JS_CLASS_INT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setUint16",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setInt32",
        2,
        js_dataview_setValue,
        JS_CLASS_INT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setUint32",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT32_ARRAY as i16
    ),
    // JS_CFUNC_NATIVE_DEF!(
    //     c"setBigInt64",
    //     2,
    //     js_dataview_setValue,
    //     JS_CLASS_BIG_INT64_ARRAY as i16
    // ),
    // JS_CFUNC_NATIVE_DEF!(
    //     c"setBigUint64",
    //     2,
    //     js_dataview_setValue,
    //     JS_CLASS_BIG_UINT64_ARRAY as i16
    // ),
    JS_CFUNC_NATIVE_DEF!(
        c"setFloat32",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"setFloat64",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT64_ARRAY as i16
    ),
    JS_PROP_STRING_DEF!(c"[Symbol.toStringTag]", c"DataView", JS_PROP_CONFIGURABLE),
];
