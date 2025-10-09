use std::cell::RefCell;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY,
    JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_DATAVIEW, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY,
    JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_PROP_STRING_DEF,
    array_buffer::ArrayBuffer,
    branch_hints::unlikely,
    class::JSClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef},
    number::JSFloat64Union,
    object::{JS_PROP_CONFIGURABLE, JSObjectWeakref, ObjectPayload},
    typed_array::{TA_GET_BUFFER, TA_GET_BYTE_LENGTH, TA_GET_BYTE_OFFSET, TypedArray},
    value::JSValue,
};

use super::{TypedArrayPayload, js_typed_array_getter};

impl JSContext {
    /// create a DataView with given array buffer.
    pub fn create_dataview(
        &self,
        ctor: &JSValue,
        buffer: ArrayBuffer,
        offset: usize,
        length: Option<usize>,
    ) -> JSResult {
        let abuf = buffer.payload();
        if abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(self);
            return Err(-1);
        }

        let byte_length = abuf.byte_length();
        if offset > byte_length {
            self.throw_range_error("invalid byteOffset");
            return Err(-1);
        }

        let mut len = byte_length - offset;
        if let Some(l) = length {
            if l > len {
                self.throw_range_error("invalid byteLength");
                return Err(-1);
            } else {
                len = l;
            }
        }

        let o = self
            .create_object_from_ctor_with(ctor, || {
                // Note: test array buffer detach again,
                // because get prototype may potentially detach the buffer.
                if unlikely(buffer.payload().is_detached()) {
                    JS_ThrowTypeErrorDetachedArrayBuffer(self);
                    Err(-1)
                } else {
                    let isa = Box::new(TypedArrayPayload {
                        obj: JSObjectWeakref::default(),
                        buffer: buffer.into(),
                        offset: offset as u32,
                        byte_length: len as _,
                        count: 0,
                    });
                    Ok(ObjectPayload::DATAVIEW(RefCell::new(isa)))
                }
            })
            .map_err(|_| -1)?;

        // set back ref to the new created DataView object
        o.payload().typed_array().unwrap().borrow_mut().obj = o.downgrade();

        Ok(o.into())
    }
}

pub fn js_dataview_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(buffer) = cc.arg(0).as_object_ArrayBuffer() else {
        ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_BUFFER);
        return Err(-1);
    };

    let offset = if cc.length() > 1 {
        cc.arg(1).to_array_index(ctx)?
    } else {
        0
    };

    let length: Option<usize> = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        Some(cc.arg(2).to_array_index(ctx)? as usize)
    } else {
        None
    };
    ctx.create_dataview(cc.this(), buffer.clone(), offset as _, length)
}

pub fn js_dataview_getValue(ctx: &JSContext, cc: CallContext) -> JSResult {
    let ta = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_DATAVIEW)?
        .typed_array()
        .unwrap();

    let class_id = cc.magic() as JSClassID;
    let size = 1 << TypedArray::SIZE_LOG2(class_id);
    let pos = cc.arg(0).to_array_index(ctx)? as usize;

    // is little endian? default to BE
    let le = if cc.length() > 1 {
        cc.arg(1).to_bool()?
    } else {
        false // default to BE
    };

    unsafe {
        let abuf = ta.borrow().buffer.payload();

        if abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        if pos + size > ta.borrow().byte_length as usize {
            ctx.throw_range_error("out of bound");
            return Err(-1);
        }

        ta.borrow().with_bytes(|buf| -> JSResult {
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
                    Ok(JSValue::from(n as f64))
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    let bb: [u8; 8] = std::slice::from_raw_parts(ptr, 8).try_into().unwrap();
                    let n = if le {
                        f64::from_le_bytes(bb)
                    } else {
                        f64::from_be_bytes(bb)
                    };
                    Ok(JSValue::from(n))
                }
                _ => {
                    unreachable!();
                }
            }
        })
    }
}

pub fn js_dataview_setValue(ctx: &JSContext, cc: CallContext) -> JSResult {
    let ta = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_DATAVIEW)?
        .typed_array()
        .unwrap();

    let class_id = cc.magic() as JSClassID;
    let size = 1 << TypedArray::SIZE_LOG2(class_id);
    let pos = cc.arg(0).to_array_index(ctx)? as usize;

    let val = cc.arg(1);
    let mut v = 0_u32;
    let mut v64 = 0_u64;

    if class_id <= JS_CLASS_UINT32_ARRAY {
        v = val.to_u32(ctx)?;
    } else if class_id <= JS_CLASS_BIG_UINT64_ARRAY {
        ctx.throw_type_error("BigInt is not supported");
        return Err(-1);
    } else {
        let d: f64 = val.to_f64(ctx)?;
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
        cc.arg(2).to_bool()?
    } else {
        false
    };

    let (is_detached, byte_length) = {
        let b = ta.borrow();
        (b.is_detached(), b.byte_length())
    };

    if is_detached {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }
    if pos + size > byte_length as usize {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }

    ta.borrow_mut().with_bytes_mut(|buf| -> Result<(), i32> {
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

pub static js_dataview_proto_funcs: [FieldDef; 20] = [
    JS_CGETSET_NATIVE_DEF!("buffer", js_typed_array_getter, NONE, TA_GET_BUFFER as _),
    JS_CGETSET_NATIVE_DEF!(
        "byteLength",
        js_typed_array_getter,
        NONE,
        TA_GET_BYTE_LENGTH as _
    ),
    JS_CGETSET_NATIVE_DEF!(
        "byteOffset",
        js_typed_array_getter,
        NONE,
        TA_GET_BYTE_OFFSET as _
    ),
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
