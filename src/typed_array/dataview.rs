use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_MAGIC_DEF, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_DATAVIEW, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY,
    JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY, JS_PROP_STRING_DEF,
    class::JSClassID,
    context::JSContext,
    error::{JS_ThrowRangeError, JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, NativeField},
    mem::js_malloc_typed,
    number::JSFloat64Union,
    object::{JS_PROP_CONFIGURABLE, JSObjectWeakref, ObjectDetail},
    typed_array::TypedArray,
    value::JSValue,
};

use super::{
    ObjTypedArray, js_typed_array_get_buffer, js_typed_array_get_byteLength,
    js_typed_array_get_byteOffset, typed_array_size_log2,
};

impl JSContext {
    /// create a DataView with given array buffer.
    pub fn create_dataview(
        &self,
        new_target: &JSValue,
        buffer: JSValue,
        offset: usize,
        length: Option<usize>,
    ) -> JSResult {
        let abuf = buffer
            .object()
            .and_then(|o| o.0.payload.borrow().array_buffer().cloned())
            .ok_or_else(|| {
                self.throw_type_error("expect an ArrayBuffer");
                -1
            })?;

        if abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(self);
            return Err(-1);
        }

        let byte_length = abuf.byte_length();

        if offset > byte_length {
            JS_ThrowRangeError(self, "invalid byteOffset");
            return Err(-1);
        }

        let mut len = byte_length - offset;
        if let Some(l) = length {
            if l > len {
                JS_ThrowRangeError(self, "invalid byteLength");
                return Err(-1);
            } else {
                len = l;
            }
        }

        let ta = unsafe {
            if let Some(ptr) = js_malloc_typed::<ObjTypedArray>(self, 1, false) {
                ptr.write(ObjTypedArray {
                    obj: JSObjectWeakref::default(),
                    buffer: buffer.object_unchecked().clone(), // underlaying array buffer
                    offset: offset as u32,
                    byte_length: len as _,
                    count: 0,
                });
                Box::from_raw(ptr.as_ptr())
            } else {
                return Err(-1);
            }
        };

        // create dataview instance
        let o = self
            .create_object_apply_ctor(new_target, ObjectDetail::DATAVIEW(ta))
            .map_err(|_| -1)?;

        // set back ref to the new created DataView object
        o.0.payload.borrow_mut().typed_array_mut().unwrap().obj = o.downgrade();

        Ok(o.into())
    }
}

pub fn js_dataview_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let buffer = cc.arg(0);
    let offset = if cc.length() > 1 {
        cc.arg(1).coerce_to_index(ctx)?
    } else {
        0
    };

    let length: Option<usize> = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        Some(cc.arg(2).coerce_to_index(ctx)?)
    } else {
        None
    };
    ctx.create_dataview(cc.this(), buffer.clone(), offset as _, length)
}

pub fn js_dataview_getValue(ctx: &JSContext, args: CallContext) -> JSResult {
    let this_obj = args.this_raw();
    let class_id = args.magic() as JSClassID;

    debug_assert_eq!(
        unsafe { this_obj.object_unchecked().class_id() },
        JS_CLASS_DATAVIEW
    );

    let size = 1 << typed_array_size_log2(class_id);

    let pos = args.arg(0).coerce_to_index(ctx)?;

    // is little endian? default to BE
    let le = if args.length() > 1 {
        args.arg(1).coerce_to_bool(ctx)?
    } else {
        false // default to BE
    };

    let isa = unsafe { this_obj.object_unchecked().0.payload.borrow() };
    let ta = isa.typed_array().unwrap();

    unsafe {
        let abuf = ta
            .buffer
            .0
            .payload
            .borrow()
            .array_buffer()
            .cloned()
            .unwrap();

        if abuf.0.borrow().detached {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        if pos + size > ta.byte_length as usize {
            JS_ThrowRangeError(ctx, "out of bound");
            return Err(-1);
        }

        ta.with_bytes(|buf| -> JSResult {
            let ptr = buf.as_ptr().add(pos as usize);

            match class_id {
                JS_CLASS_INT8_ARRAY => Ok(JSValue::Int(*ptr.cast::<i8>() as i32)),
                JS_CLASS_UINT8_ARRAY => Ok(JSValue::Int(*ptr as i32)),
                JS_CLASS_INT16_ARRAY => {
                    let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                    let n = if le {
                        i16::from_le_bytes(bb)
                    } else {
                        i16::from_be_bytes(bb)
                    };
                    Ok(JSValue::Int(n as i32))
                }
                JS_CLASS_UINT16_ARRAY => {
                    let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                    let n = if le {
                        u16::from_le_bytes(bb)
                    } else {
                        u16::from_be_bytes(bb)
                    };
                    Ok(JSValue::Int(n as i32))
                }
                JS_CLASS_INT32_ARRAY => {
                    let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                    let n = if le {
                        i32::from_le_bytes(bb)
                    } else {
                        i32::from_be_bytes(bb)
                    };
                    Ok(JSValue::Int(n))
                }
                JS_CLASS_UINT32_ARRAY => {
                    let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                    let n = if le {
                        u32::from_le_bytes(bb)
                    } else {
                        u32::from_be_bytes(bb)
                    };
                    Ok(JSValue::from(n))
                }
                JS_CLASS_BIG_INT64_ARRAY => {
                    ctx.throw_type_error("BigInt64Array is not supported");
                    Err(-1)
                }
                JS_CLASS_BIG_UINT64_ARRAY => {
                    ctx.throw_type_error("BigUint64Array is not supported");
                    Err(-1)
                }
                JS_CLASS_FLOAT32_ARRAY => {
                    let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                    let n = if le {
                        f32::from_le_bytes(bb)
                    } else {
                        f32::from_be_bytes(bb)
                    };
                    Ok(JSValue::Float64(n as f64))
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    let bb: [u8; 8] = std::slice::from_raw_parts(ptr, 8).try_into().unwrap();
                    let n = if le {
                        f64::from_le_bytes(bb)
                    } else {
                        f64::from_be_bytes(bb)
                    };
                    Ok(JSValue::Float64(n))
                }
                _ => {
                    unreachable!();
                }
            }
        })
    }
}

pub fn js_dataview_setValue(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_obj = cc.this_raw();
    let class_id = cc.magic() as JSClassID;

    debug_assert_eq!(
        unsafe { this_obj.object_unchecked().class_id() },
        JS_CLASS_DATAVIEW
    );

    let ta = TypedArray::try_from(this_obj).unwrap();

    let size = 1 << typed_array_size_log2(class_id);
    let pos = cc.arg(0).coerce_to_index(ctx)?;

    let val = cc.arg(1);
    let mut v = 0_u32;
    let mut v64 = 0_u64;

    if class_id <= JS_CLASS_UINT32_ARRAY {
        v = val.coerce_to_u32(ctx)?;
    } else if class_id <= JS_CLASS_BIG_UINT64_ARRAY {
        ctx.throw_type_error("BigInt is not supported");
        return Err(-1);
    } else {
        let d: f64 = val.coerce_to_f64(ctx)?;
        if class_id == JS_CLASS_FLOAT32_ARRAY {
            union U {
                f: f32,
                i: u32,
            }
            let u = U { f: d as f32 };
            v = unsafe { u.i };
        } else {
            let u = JSFloat64Union { d };
            v64 = unsafe { u.uint64 };
        }
    }

    let le = if cc.length() > 2 {
        cc.arg(2).coerce_to_bool(ctx)?
    } else {
        false
    };

    let (is_detached, byte_length) = ta.with(|ta| (ta.is_detached(), ta.byte_length));
    if is_detached {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }
    if pos + size > byte_length as usize {
        JS_ThrowRangeError(ctx, "out of bound");
        return Err(-1);
    }

    ta.with_bytes_mut(|buf| -> Result<(), i32> {
        unsafe {
            let ptr = buf.as_mut_ptr().add(pos as usize);

            match class_id {
                JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY => {
                    *ptr = v as u8;
                    Ok(())
                }
                JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY => {
                    let bb = if le {
                        (v as u16).to_le_bytes()
                    } else {
                        (v as u16).to_be_bytes()
                    };
                    std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 2);
                    Ok(())
                }
                JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY | JS_CLASS_FLOAT32_ARRAY => {
                    let bb = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                    std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 4);
                    Ok(())
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    let bb = if le {
                        v64.to_le_bytes()
                    } else {
                        v64.to_be_bytes()
                    };
                    std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 8);
                    Ok(())
                }
                JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                    ctx.throw_type_error("BigInt64Array, BigUint64Array is not supported");
                    Err(-1)
                }
                _ => {
                    unreachable!();
                }
            }
        }
    })?;

    Ok(JSValue::UNDEFINED)
}

pub const js_dataview_proto_funcs: [NativeField; 20] = [
    JS_CGETSET_MAGIC_DEF!("buffer", js_typed_array_get_buffer, NONE, 1),
    JS_CGETSET_MAGIC_DEF!("byteLength", js_typed_array_get_byteLength, NONE, 1),
    JS_CGETSET_MAGIC_DEF!("byteOffset", js_typed_array_get_byteOffset, NONE, 1),
    JS_CFUNC_NATIVE_DEF!(
        "getInt8",
        1,
        js_dataview_getValue,
        JS_CLASS_INT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getUint8",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getInt16",
        1,
        js_dataview_getValue,
        JS_CLASS_INT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getUint16",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getInt32",
        1,
        js_dataview_getValue,
        JS_CLASS_INT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getUint32",
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
        "getFloat32",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "getFloat64",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT64_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setInt8",
        2,
        js_dataview_setValue,
        JS_CLASS_INT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setUint8",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT8_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setInt16",
        2,
        js_dataview_setValue,
        JS_CLASS_INT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setUint16",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT16_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setInt32",
        2,
        js_dataview_setValue,
        JS_CLASS_INT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setUint32",
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
        "setFloat32",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT32_ARRAY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "setFloat64",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT64_ARRAY as i16
    ),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "DataView", JS_PROP_CONFIGURABLE),
];
