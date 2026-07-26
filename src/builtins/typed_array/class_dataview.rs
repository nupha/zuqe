// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_DATAVIEW,
    JS_CLASS_FLOAT16_ARRAY, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY,
    JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY, ScopedContext,
    builtins::typed_array::{f16_to_f64, f64_to_f16},
    class::ClassID,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef},
    number::Float64Union,
    object::JS_PROP_CONFIGURABLE,
    value::JSValue,
};

use super::{TA_GET_BUFFER, TA_GET_BYTE_LENGTH, TA_GET_BYTE_OFFSET, TypedArray, dv::DataView};

pub(super) fn js_dataview_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(buffer) = cc.arg(0).as_object_ArrayBuffer() else {
        ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_BUFFER);
        return Err(-1);
    };

    let offset = if cc.length() > 1 {
        let n = cc.arg(1).to_index(ctx)? as usize;
        // guard array buffer after reading arg #1
        if buffer.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }
        n
    } else {
        0
    };

    let length: Option<usize> = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        Some(cc.arg(2).to_index(ctx)? as usize)
    } else {
        None
    };

    DataView::create(ctx, cc.this(), buffer, offset as _, length).map(|o| o.into())
}

fn js_dataview_getValue(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    let this = cc.this();
    let Some(dataview) = this.as_object_DataView() else {
        ctx.throw_type_error_invalid_class(JS_CLASS_DATAVIEW);
        return Err(-1);
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
    if abuf.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let dv = dataview.payload();
    if pos + size > dv.byte_length() {
        ctx.throw_range_error("out of bound");
        return Err(-1);
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
                ctx.throw_type_error("BigInt64Array is not supported");
                Err(-1)
            }
            JS_CLASS_BIG_UINT64_ARRAY => {
                ctx.throw_type_error("BigUint64Array is not supported");
                Err(-1)
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

    let dv = this
        .retrive_object_payload(ctx, JS_CLASS_DATAVIEW)?
        .data_view()
        .unwrap();

    let class_id = cc.magic() as ClassID;
    let size = 1 << TypedArray::SIZE_LOG2(class_id);
    let pos = cc.arg(0).to_index(ctx)? as usize;

    let val = cc.arg(1);
    let mut v = 0_u32;
    let mut v64 = 0_u64;

    if class_id <= JS_CLASS_UINT32_ARRAY {
        v = val.to_number(ctx)?.to_u32();
    } else if class_id <= JS_CLASS_BIG_UINT64_ARRAY {
        ctx.throw_type_error("BigInt is not supported");
        return Err(-1);
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

    if dv.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    } else if pos + size > dv.byte_length() {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }

    let dv_mut = this
        .retrive_object_payload_mut(ctx, JS_CLASS_DATAVIEW)?
        .data_view_mut()
        .unwrap();

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

fn js_dataview_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(dv) = cc.this().as_object_DataView() {
        match cc.magic() {
            TA_GET_BUFFER => Ok(dv.array_buffer().into()),
            TA_GET_BYTE_LENGTH => {
                if !dv.is_detached() {
                    Ok(JSValue::from(dv.byte_length() as i32))
                } else {
                    JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                    Err(-1)
                }
            }
            TA_GET_BYTE_OFFSET => dv
                .array_buffer_byte_offset(ctx)
                .map(|n| JSValue::from(n as i32)),
            _ => unreachable!(),
        }
    } else {
        ctx.throw_type_error("not a DataView");
        Err(-1)
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
    // FieldDef::FUNCTION(
    //     c"getBigInt64",
    //     1,
    //     js_dataview_getValue,
    //     JS_CLASS_BIG_INT64_ARRAY as i16
    // ),
    // FieldDef::FUNCTION(
    //     c"getBigUint64",
    //     1,
    //     js_dataview_getValue,
    //     JS_CLASS_BIG_UINT64_ARRAY as i16
    // ),
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
    // FieldDef::FUNCTION(
    //     c"setBigInt64",
    //     2,
    //     js_dataview_setValue,
    //     JS_CLASS_BIG_INT64_ARRAY as i16
    // ),
    // FieldDef::FUNCTION(
    //     c"setBigUint64",
    //     2,
    //     js_dataview_setValue,
    //     JS_CLASS_BIG_UINT64_ARRAY as i16
    // ),
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
