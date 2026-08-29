// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_DATAVIEW,
    JS_CLASS_FLOAT16_ARRAY, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY,
    JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY, ScopedContext,
    builtins::typed_array::{f16_to_f64, f64_to_f16},
    class::ClassID,
    error::JSResult,
    function::{CallContext, FieldDef},
    number::Float64Union,
    object::JS_PROP_CONFIGURABLE,
    value::JSValue,
};

use super::{
    DataView, TA_GET_BUFFER, TA_GET_BYTE_LENGTH, TA_GET_BYTE_OFFSET, TypedArray,
    dv::DataViewPayload,
};

pub(super) fn js_dataview_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // ES §25.3.2.1
    let Some(buffer) = cc.arg(0).as_object_ArrayBuffer() else {
        return Err(ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_BUFFER));
    };

    // step 3: ToIndex may run user code that detaches the buffer,
    // so the detach check comes right after the byteOffset conversion
    let offset = cc.arg(1).to_index(ctx)? as usize;
    buffer.ensure_attached(ctx)?;

    // steps 5-6
    let buffer_len = buffer.payload().byte_length();
    if offset > buffer_len {
        return Err(ctx.throw_range_error("invalid byteOffset"));
    }

    // steps 7-9: [[ByteLength]] is `auto` when the length argument is omitted
    // and the viewed buffer is resizable (length-tracking view).
    let has_len_arg = !cc.arg(2).is_undefined();
    let length_tracked = !has_len_arg && buffer.payload().is_resizable();
    let byte_length = if has_len_arg {
        let l = cc.arg(2).to_index(ctx)? as usize;
        if offset + l > buffer_len {
            return Err(ctx.throw_range_error("invalid byteLength"));
        }
        l
    } else {
        buffer_len - offset
    };

    // steps 10-14: instance creation reads `newTarget.prototype`, which may
    // run user code that resizes/detaches the buffer — create() re-validates.
    DataView::create(ctx, cc.this(), buffer, offset, byte_length, length_tracked).map(|o| o.into())
}

fn js_dataview_getValue(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    let this = cc.this();
    let Some(dataview) = this.as_object_DataView() else {
        return Err(ctx.throw_type_error_invalid_class(JS_CLASS_DATAVIEW));
    };

    let size = 1 << TypedArray::SIZE_LOG2(class_id);
    let pos = cc.arg(0).to_index(ctx)? as usize;

    // is little endian?
    let le = if cc.length() > 1 {
        cc.arg(1).to_bool()?
    } else {
        false // default to BE
    };

    let ab = dataview.array_buffer();
    let abuf = ab.payload();
    abuf.ensure_attached(ctx)?;

    let dv = dataview.payload();
    // GetViewValue: the out-of-bounds TypeError is checked before the
    // RangeError below.
    if dv.is_out_of_bounds() {
        return Err(ctx.throw_type_error("DataView is out of bounds"));
    }
    if pos + size > dv.view_byte_length() {
        return Err(ctx.throw_range_error("out of bound"));
    }

    dv.with_bytes(|buf| unsafe {
        let ptr = buf.as_ptr().add(pos);

        match class_id {
            JS_CLASS_INT8_ARRAY => Ok(JSValue::from(*ptr.cast::<i8>() as i32)),
            JS_CLASS_UINT8_ARRAY => Ok(JSValue::from(*ptr as i32)),
            JS_CLASS_INT16_ARRAY => {
                let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                let n = if le {
                    i16::from_le_bytes(bb)
                } else {
                    i16::from_be_bytes(bb)
                };
                Ok(JSValue::from(n as i32))
            }
            JS_CLASS_UINT16_ARRAY => {
                let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                let n = if le {
                    u16::from_le_bytes(bb)
                } else {
                    u16::from_be_bytes(bb)
                };
                Ok(JSValue::from(n as i32))
            }
            JS_CLASS_INT32_ARRAY => {
                let bb: [u8; 4] = std::slice::from_raw_parts(ptr, 4).try_into().unwrap();
                let n = if le {
                    i32::from_le_bytes(bb)
                } else {
                    i32::from_be_bytes(bb)
                };
                Ok(JSValue::from(n))
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
                #[cfg(feature = "big_int")]
                {
                    let bb: [u8; 8] = std::slice::from_raw_parts(ptr, 8).try_into().unwrap();
                    let n = if le {
                        i64::from_le_bytes(bb)
                    } else {
                        i64::from_be_bytes(bb)
                    };
                    crate::bigint::from_i64(ctx, n).map(JSValue::make_bigint)
                }
                #[cfg(not(feature = "big_int"))]
                {
                    Err(ctx.throw_type_error("BigInt64Array is not supported"))
                }
            }
            JS_CLASS_BIG_UINT64_ARRAY => {
                #[cfg(feature = "big_int")]
                {
                    let bb: [u8; 8] = std::slice::from_raw_parts(ptr, 8).try_into().unwrap();
                    let n = if le {
                        u64::from_le_bytes(bb)
                    } else {
                        u64::from_be_bytes(bb)
                    };
                    crate::bigint::from_u64(ctx, n).map(JSValue::make_bigint)
                }
                #[cfg(not(feature = "big_int"))]
                {
                    Err(ctx.throw_type_error("BigUint64Array is not supported"))
                }
            }
            JS_CLASS_FLOAT16_ARRAY => {
                let bb: [u8; 2] = std::slice::from_raw_parts(ptr, 2).try_into().unwrap();
                let n = if le {
                    u16::from_le_bytes(bb)
                } else {
                    u16::from_be_bytes(bb)
                };
                Ok(JSValue::from(f16_to_f64(n)))
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

fn js_dataview_setValue(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();

    let mut o = this
        .as_object()
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_DATAVIEW))?;
    let dv = o
        .payload_data_view()
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_DATAVIEW))?;

    let class_id = cc.magic() as ClassID;
    let size = 1 << TypedArray::SIZE_LOG2(class_id);
    let pos = cc.arg(0).to_index(ctx)? as usize;

    let val = cc.arg(1);
    let mut v = 0_u32;
    let mut v64 = 0_u64;

    if class_id <= JS_CLASS_UINT32_ARRAY {
        v = val.to_number(ctx)?.to_u32();
    } else if class_id <= JS_CLASS_BIG_UINT64_ARRAY {
        #[cfg(feature = "big_int")]
        {
            // ToBigInt64 / ToBigUint64.
            let bi = crate::builtins::bigint::to_big_int(ctx, val)?;
            let bi = unsafe { bi.as_bigint_unchecked() };
            v64 = if class_id == JS_CLASS_BIG_INT64_ARRAY {
                crate::bigint::to_bigint64(&bi) as u64
            } else {
                crate::bigint::to_biguint64(&bi)
            };
        }
        #[cfg(not(feature = "big_int"))]
        {
            return Err(ctx.throw_type_error("BigInt is not supported"));
        }
    } else if class_id == JS_CLASS_FLOAT16_ARRAY {
        v = f64_to_f16(val.to_number(ctx)?.to_f64()) as u32;
    } else {
        let d: f64 = val.to_number(ctx)?.to_f64();
        if class_id == JS_CLASS_FLOAT32_ARRAY {
            union U {
                f: f32,
                i: u32,
            }
            let u = U { f: d as f32 };
            v = unsafe { u.i };
        } else {
            let u = Float64Union { d };
            v64 = unsafe { u.uint64 };
        }
    }

    let le = if cc.length() > 2 {
        cc.arg(2).to_bool()?
    } else {
        false
    };

    dv.ensure_attached(ctx)?;
    // SetViewValue: the out-of-bounds TypeError is checked before the
    // RangeError below.
    if dv.is_out_of_bounds() {
        return Err(ctx.throw_type_error("DataView is out of bounds"));
    }
    if pos + size > dv.view_byte_length() {
        return Err(ctx.throw_range_error("out of bound"));
    }

    let dv_mut = o
        .payload_data_view_mut()
        .ok_or_else(|| ctx.throw_type_error_invalid_class(JS_CLASS_DATAVIEW))?;

    dv_mut.with_bytes_mut(|buf| -> Result<(), i32> {
        unsafe {
            let ptr = buf.as_mut_ptr().add(pos);

            match class_id {
                JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY => {
                    *ptr = v as u8;
                    Ok(())
                }
                JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY | JS_CLASS_FLOAT16_ARRAY => {
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
                JS_CLASS_FLOAT64_ARRAY | JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                    let bb = if le {
                        v64.to_le_bytes()
                    } else {
                        v64.to_be_bytes()
                    };
                    std::ptr::copy_nonoverlapping(bb.as_ptr(), ptr, 8);
                    Ok(())
                }
                _ => {
                    unreachable!();
                }
            }
        }
    })?;

    Ok(JSValue::UNDEFINED)
}

fn js_dataview_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(dv) = cc.this().as_object_DataView() {
        match cc.magic() {
            TA_GET_BUFFER => Ok(dv.array_buffer().into()),
            TA_GET_BYTE_LENGTH => {
                dv.ensure_attached(ctx)?;
                let p = dv.payload();
                if p.is_out_of_bounds() {
                    return Err(ctx.throw_type_error("DataView is out of bounds"));
                }
                Ok(JSValue::from(p.view_byte_length() as i32))
            }
            TA_GET_BYTE_OFFSET => {
                dv.ensure_attached(ctx)?;
                let p = dv.payload();
                if p.is_out_of_bounds() {
                    return Err(ctx.throw_type_error("DataView is out of bounds"));
                }
                Ok(JSValue::from(p.byte_offset() as i32))
            }
            _ => unreachable!(),
        }
    } else {
        Err(ctx.throw_type_error("not a DataView"))
    }
}

pub const js_dataview_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("buffer", Some(js_dataview_getter), None, TA_GET_BUFFER as _),
    FieldDef::GETSET(
        "byteLength",
        Some(js_dataview_getter),
        None,
        TA_GET_BYTE_LENGTH as _,
    ),
    FieldDef::GETSET(
        "byteOffset",
        Some(js_dataview_getter),
        None,
        TA_GET_BYTE_OFFSET as _,
    ),
    FieldDef::FUNC(
        "getInt8",
        1,
        js_dataview_getValue,
        JS_CLASS_INT8_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getUint8",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT8_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getInt16",
        1,
        js_dataview_getValue,
        JS_CLASS_INT16_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getUint16",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT16_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getInt32",
        1,
        js_dataview_getValue,
        JS_CLASS_INT32_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getUint32",
        1,
        js_dataview_getValue,
        JS_CLASS_UINT32_ARRAY as i16,
    ),
    #[cfg(feature = "big_int")]
    FieldDef::FUNC(
        "getBigInt64",
        1,
        js_dataview_getValue,
        JS_CLASS_BIG_INT64_ARRAY as i16,
    ),
    #[cfg(feature = "big_int")]
    FieldDef::FUNC(
        "getBigUint64",
        1,
        js_dataview_getValue,
        JS_CLASS_BIG_UINT64_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getFloat16",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT16_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getFloat32",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT32_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "getFloat64",
        1,
        js_dataview_getValue,
        JS_CLASS_FLOAT64_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setInt8",
        2,
        js_dataview_setValue,
        JS_CLASS_INT8_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setUint8",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT8_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setInt16",
        2,
        js_dataview_setValue,
        JS_CLASS_INT16_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setUint16",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT16_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setInt32",
        2,
        js_dataview_setValue,
        JS_CLASS_INT32_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setUint32",
        2,
        js_dataview_setValue,
        JS_CLASS_UINT32_ARRAY as i16,
    ),
    #[cfg(feature = "big_int")]
    FieldDef::FUNC(
        "setBigInt64",
        2,
        js_dataview_setValue,
        JS_CLASS_BIG_INT64_ARRAY as i16,
    ),
    #[cfg(feature = "big_int")]
    FieldDef::FUNC(
        "setBigUint64",
        2,
        js_dataview_setValue,
        JS_CLASS_BIG_UINT64_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setFloat16",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT16_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setFloat32",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT32_ARRAY as i16,
    ),
    FieldDef::FUNC(
        "setFloat64",
        2,
        js_dataview_setValue,
        JS_CLASS_FLOAT64_ARRAY as i16,
    ),
    FieldDef::STR("[Symbol.toStringTag]", "DataView", JS_PROP_CONFIGURABLE),
];
