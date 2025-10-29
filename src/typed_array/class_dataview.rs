use std::cell::{Cell, UnsafeCell};

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY,
    JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_DATAVIEW, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY,
    JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_PROP_STRING_DEF,
    array_buffer::ArrayBuffer,
    class::ClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef, NativeFunctionType},
    number::JSFloat64Union,
    object::{JS_PROP_CONFIGURABLE, JSObject, JSObjectWeakref, ObjectPayload},
    object_cast,
    runtime::JSRuntime,
    typed_array::{TA_GET_BUFFER, TA_GET_BYTE_LENGTH, TA_GET_BYTE_OFFSET, TypedArray},
    value::JSValue,
};

use super::TypedArrayPayload;

pub type DataViewPayload = TypedArrayPayload;

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct DataView(pub(crate) JSObject);

object_cast!(DataView, JS_CLASS_DATAVIEW);

impl DataView {
    pub fn class_initialize(ctx: &JSContext, class_id: ClassID) -> Result<Option<JSObject>, i32> {
        debug_assert_eq!(class_id, JS_CLASS_DATAVIEW);

        let dv_obj = ctx.create_base_object()?;
        dv_obj.install_field_defs(ctx, &js_dataview_proto_funcs)?;

        let dv_ctor: JSValue = ctx
            .create_native_function(
                NativeFunctionType::Constructor(js_dataview_constructor),
                "DataView",
                1,
                0,
            )?
            .into();
        ctx.add_global_constructor("DataView", &dv_ctor, &dv_obj.clone().into());

        Ok(Some(dv_obj))
    }

    pub fn class_finalize(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {
        if let Some(dv) = isa.data_view() {
            // during the GC the finalizers are called in an arbitrary
            // order so the ArrayBuffer finalize may have been called
            dv.array_buffer().payload().remove_typed_array_weakref(id);
        }
    }

    /// create a DataView with AarrayBuffer, offset and length.
    pub(crate) fn create(
        ctx: &JSContext,
        ctor: &JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        length: Option<usize>,
    ) -> Result<Self, i32> {
        debug_assert!(!array_buffer.payload().is_detached());

        let byte_length = array_buffer.payload().byte_length();
        if offset > byte_length {
            ctx.throw_range_error("invalid byteOffset");
            return Err(-1);
        }

        let mut len = byte_length - offset;
        if let Some(l) = length {
            if l > len {
                ctx.throw_range_error("invalid byteLength");
                return Err(-1);
            } else {
                len = l;
            }
        }

        let o = ctx
            .create_object_from_ctor(
                ctor,
                ObjectPayload::DATAVIEW(Box::new(DataViewPayload {
                    obj: UnsafeCell::new(JSObjectWeakref::default()),
                    buffer: array_buffer.clone(),
                    offset: offset as u32,
                    byte_length: len as _,
                    count: Cell::new(0), // no use
                })),
            )
            .map_err(|_| -1)?;

        // could have been detached in create_object_from_ctor(), reading prototype method.
        if array_buffer.payload().is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        // let o = ctx
        //     .create_object_from_ctor_with(ctor, || {
        //         // Note: test array buffer detach again,
        //         // because get prototype may potentially detach the buffer.
        //         if unlikely(array_buffer.payload().is_detached()) {
        //             JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        //             Err(-1)
        //         } else {
        //             let isa = Box::new(DataViewPayload {
        //                 obj: UnsafeCell::new(JSObjectWeakref::default()),
        //                 buffer: array_buffer,
        //                 offset: offset as u32,
        //                 byte_length: len as _,
        //                 count: Cell::new(0),
        //             });
        //             Ok(ObjectPayload::DATAVIEW(isa))
        //         }
        //     })
        //     .map_err(|_| -1)?;

        // set back ref to DataView object self
        let dv = Self(o);
        dv.payload().set_obj(&dv.0);

        Ok(dv)
    }

    pub fn payload(&self) -> &DataViewPayload {
        self.0.payload().data_view().unwrap()
    }

    /// check if the TypedArray is detached.
    #[inline]
    pub fn is_detached(&self) -> bool {
        self.payload().is_detached()
    }

    pub fn array_buffer(&self) -> &ArrayBuffer {
        self.payload().array_buffer()
    }

    /// Get elements count of typed array.
    /// If underlaying array buffer is detached, returns 0.
    #[inline]
    pub fn length(&self) -> usize {
        #[cfg(debug_assertions)]
        if self.is_detached() {
            debug_assert_eq!(self.payload().count(), 0);
            0
        } else {
            self.payload().count()
        }

        #[cfg(not(debug_assertions))]
        self.payload().count()
    }

    pub fn byte_length(&self) -> usize {
        if self.is_detached() {
            0
        } else {
            self.payload().byte_length()
        }
    }

    /// Get underlying array buffer byte offset.
    /// When underlaying array buffer is detached, returns Err.
    pub fn array_buffer_byte_offset(&self, ctx: &JSContext) -> Result<usize, i32> {
        if self.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            Err(-1)
        } else {
            Ok(self.payload().offset as usize)
        }
    }
}

impl JSContext {
    /// create a DataView with underlaying AarrayBuffer.
    #[deprecated(note = "use DataView::create() instead")]
    pub fn create_dataview(
        &self,
        ctor: &JSValue,
        array_buffer: ArrayBuffer,
        offset: usize,
        length: Option<usize>,
    ) -> Result<DataView, i32> {
        DataView::create(self, ctor, array_buffer, offset, length)
    }
}

pub fn js_dataview_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(buffer) = cc.arg(0).as_object_ArrayBuffer() else {
        ctx.throw_type_error_invalid_class(JS_CLASS_ARRAY_BUFFER);
        return Err(-1);
    };

    let offset = if cc.length() > 1 {
        let n = cc.arg(1).to_array_index(ctx)? as usize;
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
        Some(cc.arg(2).to_array_index(ctx)? as usize)
    } else {
        None
    };

    DataView::create(ctx, cc.this(), buffer.clone(), offset as _, length).map(|o| o.into())
}

pub fn js_dataview_getValue(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(dataview) = cc.this().as_object_DataView() else {
        ctx.throw_type_error_invalid_class(JS_CLASS_DATAVIEW);
        return Err(-1);
    };

    let class_id = cc.magic() as ClassID;
    let size = 1 << TypedArray::SIZE_LOG2(class_id);
    let pos = cc.arg(0).to_array_index(ctx)? as usize;

    // is little endian?
    let le = if cc.length() > 1 {
        cc.arg(1).to_bool()?
    } else {
        false // default to BE
    };

    let abuf = dataview.array_buffer().payload();
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

pub fn js_dataview_setValue(ctx: &JSContext, cc: CallContext) -> JSResult {
    let dv = cc
        .this()
        .retrive_object_payload(ctx, JS_CLASS_DATAVIEW)?
        .data_view()
        .unwrap();

    let class_id = cc.magic() as ClassID;
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

    if dv.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }
    if pos + size > dv.byte_length() {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }

    dv.with_bytes_mut(|buf| -> Result<(), i32> {
        unsafe {
            let ptr = buf.as_mut_ptr().add(pos);

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

fn js_dataview_getter(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let Some(dv) = cc.this().as_object_DataView() {
        match cc.magic() {
            TA_GET_BUFFER => Ok(dv.array_buffer().clone().into()),
            TA_GET_BYTE_LENGTH => {
                if !dv.is_detached() {
                    Ok(JSValue::Int(dv.byte_length() as i32))
                } else {
                    JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                    Err(-1)
                }
            }
            TA_GET_BYTE_OFFSET => dv
                .array_buffer_byte_offset(ctx)
                .map(|n| JSValue::Int(n as i32)),
            _ => unreachable!(),
        }
    } else {
        ctx.throw_type_error("not a DataView");
        Err(-1)
    }
}

pub static js_dataview_proto_funcs: [FieldDef; 20] = [
    JS_CGETSET_NATIVE_DEF!("buffer", js_dataview_getter, NONE, TA_GET_BUFFER as _),
    JS_CGETSET_NATIVE_DEF!(
        "byteLength",
        js_dataview_getter,
        NONE,
        TA_GET_BYTE_LENGTH as _
    ),
    JS_CGETSET_NATIVE_DEF!(
        "byteOffset",
        js_dataview_getter,
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
