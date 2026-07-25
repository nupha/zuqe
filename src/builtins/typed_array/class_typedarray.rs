// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cmp::Ordering;

use crate::{
    Array, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_FLOAT16_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY,
    atom::Atom,
    builtins::{
        array::{
            js_array_every, js_array_reduce, special_TA, special_every, special_filter,
            special_find, special_findIndex, special_findLast, special_findLastIndex,
            special_forEach, special_includes, special_indexOf, special_lastIndexOf, special_map,
            special_reduce, special_reduceRight, special_some,
        },
        array_buffer::ABufData,
        iterator::{JSIteratorKindEnum, js_create_array_iterator},
        js_get_this,
        typed_array::{f16_to_f64, f64_to_f16},
    },
    class::ClassID,
    context::ScopedContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef},
    number::Float64Union,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_SpeciesConstructor, Object},
    string::StringBuf,
    utils::unlikely,
    value::{HINT_NUMBER, JSValue},
};

use super::TypedArray;

/// Get the length of the TypedArray, equivalent to TypedArrayObj::length() but
/// will return an error if the TypedArray is detached.
pub(crate) fn js_typed_array_get_length_internal(
    ctx: &mut ScopedContext,
    obj: JSValue,
) -> Result<u32, i32> {
    obj.as_object_TypedArray()
        .ok_or_else(|| {
            ctx.throw_type_error("not a TypedArray");
            -1
        })
        .and_then(|p| {
            if p.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                Err(-1)
            } else {
                Ok(p.length() as u32)
            }
        })
}

pub(crate) fn js_typed_array_base_constructor(ctx: &mut ScopedContext, _: CallContext) -> JSResult {
    ctx.throw_type_error("cannot be called");
    Err(-1)
}

pub(crate) fn js_typed_array___speciesCreate(
    ctx: &mut ScopedContext,
    _this_val: JSValue,
    args: &[JSValue],
) -> JSResult {
    let obj = args[0];

    let Some(ta) = obj.as_object_TypedArray() else {
        ctx.throw_type_error("not a TypedArray");
        return Err(-1);
    };

    let ctor = JS_SpeciesConstructor(ctx, obj, JSValue::UNDEFINED)?;
    let args_list = if args.len() > 1 { &args[1..] } else { &[] };
    if ctor.is_undefined() {
        js_typed_array_constructor(
            ctx,
            CallContext::new(JSValue::UNDEFINED, args_list, ta.class_id() as _, None),
        )
    } else {
        js_typed_array_create(ctx, ctor, args_list)
    }
}

pub fn js_typed_array_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let class_id = cc.magic() as ClassID;

    let ta = if let Some(p) = cc.arg(0).as_object() {
        if let Some(ab) = p.as_ArrayBuffer() {
            let size_log2 = TypedArray::SIZE_LOG2(class_id as _);
            let byte_length = ab.payload().byte_length();

            let offset = cc.arg(1).to_index(ctx)? as usize;
            // guard side effect (detach) of retrieving arg #1.
            if ab.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                return Err(-1);
            }

            if (offset & ((1 << size_log2) - 1)) != 0 {
                ctx.throw_range_error("offset should be a multiple of element size");
                return Err(-1);
            } else if offset > byte_length {
                ctx.throw_range_error("offset must not greater than byte length");
                return Err(-1);
            }

            let len = {
                let a2 = cc.arg(2);
                if a2.is_undefined() {
                    if (byte_length & ((1 << size_log2) - 1)) != 0 {
                        ctx.throw_range_error("byte length should be a multiple of element size");
                        return Err(-1);
                    }
                    (byte_length - offset) >> size_log2
                } else {
                    let n = a2.to_index(ctx)? as usize;
                    // guard side effect (detach) of retrieving arg #2.
                    if ab.is_detached() {
                        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                        return Err(-1);
                    }

                    if offset + (n << size_log2) > byte_length {
                        ctx.throw_range_error("invalid length");
                        return Err(-1);
                    }
                    n
                }
            };

            TypedArray::create_from(ctx, new_target, p.into(), offset, len, class_id as _)
        } else {
            TypedArray::create_from(ctx, new_target, cc.arg(0), 0, 0, class_id as _)
        }
    } else {
        let len = cc.arg(0).to_index(ctx)? as usize;
        TypedArray::create_empty(ctx, new_target, len, class_id)
    };

    ta.map(|x| x.into())
}

fn js_typed_array_create(ctx: &mut ScopedContext, ctor: JSValue, args: &[JSValue]) -> JSResult {
    let ret = ctor.call_ctor(ctx, args)?;

    // validate the typed array
    let new_len = js_typed_array_get_length_internal(ctx, ret)?;

    if args.len() == 1 {
        // ensure that it is large enough
        let len = args[0].to_length(ctx)?;
        if new_len < len as u32 {
            ctx.throw_type_error("TypedArray length is too small");
            return Err(-1);
        }
    }

    Ok(ret)
}

fn js_typed_array_from(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)

    let items = cc.arg(0);

    let mut mapping = false;
    let mut map_fn = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    let argc = cc.length();
    if argc > 1 {
        map_fn = cc.arg(1);
        if !map_fn.is_undefined() {
            map_fn.ensure_callable(ctx)?;
            mapping = true;
            if argc > 2 {
                this_arg = cc.arg(2);
            }
        }
    }

    // [Symbol.iterator]
    let iterator = {
        #[cfg(feature = "builtin_symbol")]
        {
            items.get_property(ctx, Atom::CONST_Symbol_iterator)?
        }
        #[cfg(not(feature = "builtin_symbol"))]
        JSValue::UNDEFINED
    };

    let arr: JSValue = if !iterator.is_undefined() {
        // target is enumerable
        let mut fa = Array::create(ctx)?;
        for (i, v) in items.iter(ctx)?.enumerate() {
            fa.define_property_value_i64(ctx, i as _, v?, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }
        fa.into()
    } else {
        items.objectify(ctx)?.into()
    };

    let len = arr.get_length_prop(ctx)?;
    let len_val = JSValue::from(len);
    let mut res_ta = js_typed_array_create(ctx, cc.this(), &[len_val])?;

    for i in 0..len {
        let mut el = arr.get_property_i64(ctx, i)?;
        if mapping {
            el = map_fn.call(ctx, this_arg, &[el, JSValue::from(i as i32)])?;
        }
        res_ta.set_property_i64(ctx, i, el)?;
    }

    Ok(res_ta)
}

fn js_typed_array_of(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut obj = js_typed_array_create(ctx, cc.this(), &[JSValue::from(cc.length() as i32)])?;
    for i in 0..cc.length() {
        obj.set_property_u32(ctx, i as u32, cc.arg(i))?;
    }
    Ok(obj)
}

pub const js_typed_array_base_funcs: &[FieldDef] = &[
    FieldDef::FUNC("from", 1, js_typed_array_from, 0),
    FieldDef::FUNC("of", 0, js_typed_array_of, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET("[Symbol.species]", Some(js_get_this), None, 0),
];

//
// Prototype
//

pub fn js_typed_array_get_length(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    TypedArray::try_from(cc.this())
        .map(|p| JSValue::from(p.length() as i32))
        .map_err(|_| {
            ctx.throw_type_error("not a TypedArray");
            -1
        })
}

pub(super) const TA_GET_BUFFER: i32 = 1;
pub(super) const TA_GET_BYTE_LENGTH: i32 = 2;
pub(super) const TA_GET_BYTE_OFFSET: i32 = 3;

fn js_typed_array_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(ta) = cc.this().as_object_TypedArray() {
        match cc.magic() {
            TA_GET_BUFFER => Ok(ta.array_buffer().into()),
            TA_GET_BYTE_LENGTH => {
                if !ta.is_detached() {
                    Ok(JSValue::from(ta.byte_length() as i32))
                } else {
                    Ok(JSValue::from(0))
                }
            }
            TA_GET_BYTE_OFFSET => Ok(JSValue::from(ta.array_buffer_byte_offset() as u32)),
            _ => unreachable!(),
        }
    } else {
        ctx.throw_type_error("not a TypedArray");
        Err(-1)
    }
}

fn js_typed_array_at(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let Some(ta) = this.as_object_TypedArray() else {
        ctx.throw_type_error("not a TypedArray");
        return Err(-1);
    };

    if ta.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
    let len = ta.length() as i64;
    if idx < 0 {
        idx += len;
    }

    if unlikely(idx < 0 || idx >= len) {
        Ok(JSValue::UNDEFINED)
    } else {
        this.get_property_value(ctx, JSValue::from(idx))
    }
}

fn js_typed_array_with(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let Some(ta) = this.as_object_TypedArray() else {
        ctx.throw_type_error("not a TypedArray");
        return Err(-1);
    };

    // don't change following 2 lines order
    let mut idx: i64 = cc.arg(0).to_number(ctx)?.to_i64_saturating();
    let val = cc.arg(1).to_primitive(ctx, HINT_NUMBER)?;

    let len = ta.length() as i64;
    if idx < 0 {
        idx += len;
    }
    if idx < 0 || idx >= len {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }

    let mut ta2 = ta.create_to_new(ctx, JSValue::UNDEFINED, ta.class_id())?;
    ta2.set_property_i64(ctx, idx, val)?;
    Ok(ta2.into())
}

fn js_typed_array_set_internal(
    ctx: &mut ScopedContext,
    mut dst: JSValue,
    src: JSValue,
    offset: &JSValue,
) -> JSResult {
    // don't change order
    let Some(mut dest) = dst.as_object_TypedArray() else {
        ctx.throw_type_error("not a TypedArray");
        return Err(-1);
    };

    let offset = offset.to_number(ctx)?.to_i64_saturating();
    if offset < 0 {
        ctx.throw_range_error("invalid array length");
        return Err(-1);
    }

    if dest.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    if let Some(src) = src.as_object_TypedArray()
        && (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&src.class_id())
    {
        let s = src.array_buffer();
        let d = dest.array_buffer();

        let src_abuf = s.payload();
        if src_abuf.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let count = src.payload().count();
        if offset > dest.length() as i64 - count as i64 {
            ctx.throw_range_error("invalid array length");
            return Err(-1);
        }

        let dst_bpe = TypedArray::SIZE_LOG2(dest.class_id());
        let src_bpe = TypedArray::SIZE_LOG2(src.class_id());
        let dest_abuf = d.payload();

        if dest_abuf.data().id() == src_abuf.data().id() {
            // use same underlying buffer
            if src.class_id() == dest.class_id() {
                // fast path: same type, use mem copy
                let src_ptr = src.with_bytes(|buf| buf.as_ptr());
                dest.with_bytes_mut(|buf| unsafe {
                    std::ptr::copy(
                        src_ptr,
                        buf.as_mut_ptr().add((offset << dst_bpe) as usize),
                        count << src_bpe, // count to byte_num
                    );
                });
                return Ok(JSValue::UNDEFINED);
            }

            // copying between the same buffer using different types of mappings
            // would require a temporary buffer.
            // TODO: XXX
        } else if src.class_id() == dest.class_id() {
            // fast path: same type, use mem copy
            src.with_bytes(|src_buf| {
                dest.with_bytes_mut(|dst_buf| unsafe {
                    std::ptr::copy(
                        src_buf.as_ptr(),
                        dst_buf.as_mut_ptr().add((offset << dst_bpe) as usize),
                        count << src_bpe, // count to byte_num
                    );
                })
            });
            return Ok(JSValue::UNDEFINED);
        }

        // use slow path now.
        // TODO: do transfer in efficient way
    }

    // slow path
    let src_p = src.objectify(ctx)?;
    let count = src_p.get_length_prop(ctx)?;
    if offset > dest.length() as i64 - count {
        ctx.throw_range_error("invalid array length");
        return Err(-1);
    }

    for i in 0..count as u32 {
        let val = src_p.get_property_u32(ctx, i)?;
        dst.set_property_u32(ctx, offset as u32 + i, val)?;
    }

    Ok(JSValue::UNDEFINED)
}

fn js_typed_array_set(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let offset = if cc.length() > 1 {
        cc.arg(1)
    } else {
        JSValue::UNDEFINED
    };
    js_typed_array_set_internal(ctx, cc.this(), cc.arg(0), &offset)
}

pub(crate) fn js_create_typed_array_iterator(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(ta) = cc.this().as_object_TypedArray() {
        if ta.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            Err(-1)
        } else {
            js_create_array_iterator(ctx, cc)
        }
    } else {
        ctx.throw_type_error("not a TypedArray");
        Err(-1)
    }
}

fn js_typed_array_get_toStringTag(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(ta) = cc.this().as_object_TypedArray() {
        ctx.rt_ptr().class_registry[ta.class_id() as usize]
            .name_atom
            .force_string(ctx)
            .map(JSValue::make_string)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_typed_array_copyWithin(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let len = js_typed_array_get_length_internal(ctx, cc.this())? as i32;
    let to: i32 = cc.arg(0).to_number(ctx)?.to_i32_clamp(0, len, len);
    let from: i32 = cc.arg(1).to_number(ctx)?.to_i32_clamp(0, len, len);
    let final_ = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_number(ctx)?.to_i32_clamp(0, len, len)
    } else {
        len
    };

    let count = std::cmp::min(final_ - from, len - to);
    if count > 0 {
        let mut tao = TypedArray::try_from(cc.this())?;
        if unlikely(tao.is_detached()) {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let shift = TypedArray::SIZE_LOG2(tao.class_id());

        tao.with_bytes_mut(|buf| {
            let base = buf.as_mut_ptr();
            unsafe {
                std::ptr::copy(
                    base.add((from << shift) as usize),
                    base.add((to << shift) as usize),
                    (count << shift) as usize,
                );
            }
        });
    }

    Ok(cc.this())
}

fn js_typed_array_fill(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let len = js_typed_array_get_length_internal(ctx, cc.this())? as i32;

    // we are sure cc.this() is a TypedArray
    let mut ta = cc.this().as_object_TypedArray().unwrap();

    let shift = TypedArray::SIZE_LOG2(ta.class_id());
    let val = cc.arg(0);
    let v64: u64;

    match ta.class_id() {
        JS_CLASS_UINT8C_ARRAY => {
            v64 = val.to_number(ctx)?.to_u8_clamped() as _;
        }
        JS_CLASS_INT8_ARRAY
        | JS_CLASS_UINT8_ARRAY
        | JS_CLASS_INT16_ARRAY
        | JS_CLASS_UINT16_ARRAY
        | JS_CLASS_INT32_ARRAY
        | JS_CLASS_UINT32_ARRAY => {
            v64 = val.to_number(ctx)?.to_u32() as _;
        }

        JS_CLASS_FLOAT16_ARRAY => {
            let d: f64 = val.to_number(ctx)?.to_f64();
            v64 = f64_to_f16(d) as u64;
        }
        JS_CLASS_FLOAT32_ARRAY => {
            let d: f64 = val.to_number(ctx)?.to_f64();
            v64 = unsafe {
                #[repr(C)]
                union Float32Union {
                    f: f32,
                    uint32: u32,
                }
                let u = Float32Union { f: d as f32 };
                u.uint32 as _
            };
        }
        JS_CLASS_FLOAT64_ARRAY => {
            let d: f64 = val.to_number(ctx)?.to_f64();
            v64 = unsafe {
                let u = Float64Union { d };
                u.uint64
            };
        }

        c @ (JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY) => {
            //JS_ToBigInt64(ctx, &mut v64 as *mut u64 as *mut i64, (*argv.offset(0)))
            ctx.throw_type_error(&format!(
                "typed array for {} is not supported",
                if c == JS_CLASS_BIG_INT64_ARRAY {
                    "BigInt"
                } else {
                    "BigUint"
                }
            ));
            return Err(-1);
        }
        _ => {
            unreachable!()
        }
    }

    let start = if cc.length() > 1 {
        cc.arg(1)
            .to_number(ctx)?
            .to_i32_clamp(0, len as _, len as _) as usize
    } else {
        0
    };
    let final_ = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2)
            .to_number(ctx)?
            .to_i32_clamp(0, len as _, len as _) as usize
    } else {
        len as _
    };

    // Note: check detached again here, because the typed array may detach the array buffer
    // by side effect
    if unlikely(ta.is_detached()) {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    match shift {
        0 => {
            // 1 byte per element
            if start < final_ {
                ta.with_bytes_mut(|buf| unsafe {
                    std::ptr::write_bytes(buf.as_mut_ptr().add(start), v64 as u8, final_ - start);
                });
            }
        }
        1 => {
            // 2 bytes per element
            ta.with_bytes_mut(|buf| unsafe {
                let mut ptr = buf.as_mut_ptr().cast::<u16>().add(start);
                for _ in start..final_ {
                    ptr.write(v64 as u16);
                    ptr = ptr.add(1);
                }
            });
        }
        2 => {
            // 4 bytes per element
            ta.with_bytes_mut(|buf| unsafe {
                let mut ptr = buf.as_mut_ptr().cast::<u32>().add(start);
                for _ in start..final_ {
                    ptr.write(v64 as u32);
                    ptr = ptr.add(1);
                }
            });
        }
        3 => {
            // 8 bytes per element
            ta.with_bytes_mut(|buf| unsafe {
                let mut ptr = buf.as_mut_ptr().cast::<u64>().add(start);
                for _ in start..final_ {
                    ptr.write(v64);
                    ptr = ptr.add(1);
                }
            });
        }
        _ => unreachable!(),
    }

    Ok(ta.into())
}

fn js_typed_array_find(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    let len = js_typed_array_get_length_internal(ctx, this_val)? as i32;

    let arg0 = cc.arg(0);
    let func = arg0.ensure_callable(ctx)?;

    let this_arg = if cc.length() > 1 {
        cc.arg(1)
    } else {
        JSValue::UNDEFINED
    };

    let mut k: i32;
    let end: i32;
    let dir: i32;

    if matches!(cc.magic(), special_findLast | special_findLastIndex) {
        k = len - 1;
        dir = -1;
        end = -1;
    } else {
        k = 0;
        dir = 1;
        end = len;
    }

    while k != end {
        let index_val = JSValue::from(k);
        let val = this_val.get_property_value(ctx, index_val)?;

        let res = func.call(ctx, this_arg, &[val, index_val, this_val])?;
        if res.to_bool().unwrap_or(true) {
            return if matches!(cc.magic(), special_findIndex | special_findLastIndex) {
                Ok(index_val)
            } else {
                Ok(val)
            };
        }

        k += dir;
    }

    if matches!(cc.magic(), special_findIndex | special_findLastIndex) {
        Ok(JSValue::from(-1))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_typed_array_reverse(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let len = js_typed_array_get_length_internal(ctx, cc.this())?;
    if len > 0 {
        let mut ta = cc.this().as_object_TypedArray().unwrap();

        macro_rules! swap_slice {
            ($ty:ident, $shift:expr) => {
                ta.with_bytes_mut(|buf| {
                    if !buf.is_empty() {
                        let len = buf.len() >> $shift;
                        let lst = unsafe {
                            std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<$ty>(), len)
                        };
                        for i in 0..(len >> 1) {
                            lst.swap(i, len - i - 1);
                        }
                    }
                });
            };
        }

        match TypedArray::SIZE_LOG2(ta.class_id()) {
            0 => {
                swap_slice!(u8, 0);
            }
            1 => {
                swap_slice!(u16, 1);
            }
            2 => {
                swap_slice!(u32, 2);
            }
            3 => {
                swap_slice!(u64, 3);
            }
            _ => unreachable!(),
        }

        Ok(ta.into())
    } else {
        Ok(cc.this())
    }
}

fn js_typed_array_toReversed(ctx: &mut ScopedContext, mut cc: CallContext) -> JSResult {
    let tao = TypedArray::try_from(cc.this()).map_err(|_| {
        ctx.throw_type_error("not a TypedArray");
        -1
    })?;
    let arr = tao.create_to_new(ctx, JSValue::UNDEFINED, tao.class_id())?;

    cc.set_this(arr.into());
    js_typed_array_reverse(ctx, cc)
}

fn js_typed_array_slice(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let len = js_typed_array_get_length_internal(ctx, cc.this())?;
    let start = cc
        .arg(0)
        .to_number(ctx)?
        .to_i32_clamp(0, len as _, len as _) as usize;
    let final_ = if !cc.arg(1).is_undefined() {
        cc.arg(1)
            .to_number(ctx)?
            .to_i32_clamp(0, len as _, len as _) as usize
    } else {
        len as usize
    };

    let this_val = cc.this();
    let count = final_.saturating_sub(start);
    let shift = TypedArray::SIZE_LOG2(cc.this().as_object().unwrap().class_id());

    // create result typed array
    let mut arr = js_typed_array___speciesCreate(
        ctx,
        JSValue::UNDEFINED,
        &[this_val, JSValue::from(count as i32)],
    )?;

    if count > 0 {
        // copy values from src to result array
        let Some(src) = this_val.as_object_TypedArray() else {
            ctx.throw_type_error("not a TypedArray");
            return Err(-1);
        };
        if src.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let Some(mut dst) = arr.as_object_TypedArray() else {
            ctx.throw_type_error("not a TypedArray");
            return Err(-1);
        };
        if dst.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        if src.class_id() == dst.class_id()
            && dst.capacity() >= count
            && src.capacity() >= start + count
        {
            // fast path
            if src.array_buffer().payload().data().id() == dst.array_buffer().payload().data().id()
            {
                // src, dst are in same memory chunk
                let src_ptr = src.with_bytes(|b| b.as_ptr());

                dst.with_bytes_mut(|buf| unsafe {
                    // Hint: 当用src_ptr, dst_ptr复制 count<<shift 个字节，且两个内存区域发生重叠时，
                    // std::ptr::copy() 能正确处理覆盖的情况。
                    // 但是，ECMA规范偏偏选择了错误的方向，以类似C memcpy()那种方式，规定发生重叠覆盖为"正宗"。
                    // 所以，本处不使用std::ptr::copy()，而用手工指针复制来模拟带bug的memcpy()模式。
                    // std::ptr::copy(
                    //     src_ptr.add(start << shift),
                    //     buf.as_mut_ptr(),
                    //     count << shift,
                    // );

                    // may have overwrite
                    let mut src_ptr = src_ptr.add(start << shift);
                    let mut dst_ptr = buf.as_mut_ptr();
                    for _ in 0..count << shift {
                        *dst_ptr = *src_ptr;
                        src_ptr = src_ptr.add(1);
                        dst_ptr = dst_ptr.add(1);
                    }
                });
            } else {
                src.with_bytes(|src_buf| {
                    dst.with_bytes_mut(|dst_buf| unsafe {
                        std::ptr::copy(
                            src_buf.as_ptr().add(start << shift),
                            dst_buf.as_mut_ptr(),
                            count << shift,
                        );
                    });
                });
            }
            return Ok(arr);
        }

        // slow path using property set
        for n in 0..count {
            let val = this_val.get_property_value(ctx, JSValue::from((start + n) as i32))?;
            arr.set_property_u32(ctx, n as u32, val)?;
        }
    }

    Ok(arr)
}

fn js_typed_array_subarray(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();

    let Some(ta) = this.as_object_TypedArray() else {
        ctx.throw_type_error("not a TypedArray");
        return Err(-1);
    };

    // in order to accept detached typed array
    // do not use js_typed_array_get_length_internal() here.
    let len = ta.length();

    let start = cc
        .arg(0)
        .to_number(ctx)?
        .to_i32_clamp(0, len as _, len as _) as usize;
    // Note: calc byte offset before read `final` argument
    let byte_offset =
        ta.array_buffer_byte_offset() + (start << TypedArray::SIZE_LOG2(ta.class_id()));

    let final_ = if !cc.arg(1).is_undefined() {
        cc.arg(1)
            .to_number(ctx)?
            .to_i32_clamp(0, len as _, len as _) as usize
    } else {
        len
    };
    let count = final_.saturating_sub(start);

    js_typed_array___speciesCreate(
        ctx,
        JSValue::UNDEFINED,
        &[
            this,
            ta.array_buffer().into(), // array buffer
            JSValue::from(byte_offset as i32),
            JSValue::from(count as i32),
        ],
    )
}

fn sort_f64_slice(slice: &mut [f64]) {
    slice.sort_by(|a, b| {
        // NaN is at last
        match (a.is_nan(), b.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => {
                // Infinity
                match (a.is_infinite(), b.is_infinite()) {
                    (true, true) => {
                        // 都是无穷大：+inf > -inf
                        if a.is_sign_positive() == b.is_sign_positive() {
                            std::cmp::Ordering::Equal
                        } else if a.is_sign_positive() {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    (true, false) => {
                        // 只有 a 是无穷大
                        if a.is_sign_positive() {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    (false, true) => {
                        // 只有 b 是无穷大
                        if b.is_sign_positive() {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        }
                    }
                    (false, false) => {
                        // 都是有限值，处理 -0.0 和 0.0
                        if *a == 0.0 && *b == 0.0 {
                            // 都是零值，需要区分 +0 和 -0
                            match (a.is_sign_negative(), b.is_sign_negative()) {
                                (true, false) => std::cmp::Ordering::Less,    // -0 < +0
                                (false, true) => std::cmp::Ordering::Greater, // +0 > -0
                                _ => std::cmp::Ordering::Equal,               // 符号相同
                            }
                        } else {
                            // 普通数值比较
                            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                        }
                    }
                }
            }
        }
    });
}

fn sort_f32_slice(slice: &mut [f32]) {
    slice.sort_by(|a, b| {
        // NaN is at last
        match (a.is_nan(), b.is_nan()) {
            (true, true) => std::cmp::Ordering::Equal,
            (true, false) => std::cmp::Ordering::Greater,
            (false, true) => std::cmp::Ordering::Less,
            (false, false) => {
                // Infinity
                match (a.is_infinite(), b.is_infinite()) {
                    (true, true) => {
                        // 都是无穷大：+inf > -inf
                        if a.is_sign_positive() == b.is_sign_positive() {
                            std::cmp::Ordering::Equal
                        } else if a.is_sign_positive() {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    (true, false) => {
                        // 只有 a 是无穷大
                        if a.is_sign_positive() {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    (false, true) => {
                        // 只有 b 是无穷大
                        if b.is_sign_positive() {
                            std::cmp::Ordering::Less
                        } else {
                            std::cmp::Ordering::Greater
                        }
                    }
                    (false, false) => {
                        // 都是有限值，处理 -0.0 和 0.0
                        if *a == 0.0 && *b == 0.0 {
                            // 都是零值，需要区分 +0 和 -0
                            match (a.is_sign_negative(), b.is_sign_negative()) {
                                (true, false) => std::cmp::Ordering::Less,    // -0 < +0
                                (false, true) => std::cmp::Ordering::Greater, // +0 > -0
                                _ => std::cmp::Ordering::Equal,               // 符号相同
                            }
                        } else {
                            // 普通数值比较
                            a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                        }
                    }
                }
            }
        }
    });
}

fn call_cmp_fn(
    ctx: &mut ScopedContext,
    cmp: Object,
    a: JSValue,
    b: JSValue,
) -> Result<Ordering, i32> {
    let n = cmp
        .call(ctx, JSValue::UNDEFINED, &[a, b])
        .and_then(|v| v.to_number(ctx))?
        .to_i32();

    Ok(if n < 0 {
        Ordering::Less
    } else if n > 0 {
        Ordering::Greater
    } else {
        Ordering::Equal
    })
}

fn js_typed_array_sort(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let cmp = cc.arg(0);
    if unlikely(!cmp.is_undefined()) {
        cmp.ensure_callable(ctx)?;
    }

    let this_val = cc.this();
    let len = js_typed_array_get_length_internal(ctx, this_val)?;

    if len > 1 {
        let Some(ta) = this_val.as_object_TypedArray() else {
            ctx.throw_type_error("not a TypedArray");
            return Err(-1);
        };

        // Note: because callback handler may access array buffer,
        // to avoid borrow conflict, we take buffer content out then run closure with underlaying bytes,
        // after sorting, restore content back to array buffer.

        let mut abuf_content = ta.array_buffer().payload_mut().replace(ABufData::Empty);

        macro_rules! sort {
            ($ty:ident, $shift:expr, $cvt:expr) => {
                abuf_content.with_bytes_mut(|buf| -> Result<(), i32> {
                    let slice = unsafe {
                        std::slice::from_raw_parts_mut(
                            buf.as_mut_ptr().cast::<$ty>(),
                            buf.len() >> $shift,
                        )
                    };

                    if let Some(cmp) = cmp.as_object() {
                        let mut has_error = false;
                        slice.sort_by(|a, b| {
                            if !has_error {
                                if let Ok(r) = call_cmp_fn(ctx, cmp, $cvt(*a), $cvt(*b)) {
                                    r
                                } else {
                                    has_error = true;
                                    Ordering::Equal
                                }
                            } else {
                                Ordering::Equal
                            }
                        });

                        if has_error { Err(-1) } else { Ok(()) }
                    } else {
                        slice.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        Ok(())
                    }
                })
            };
        }

        let r = match ta.class_id() {
            JS_CLASS_INT8_ARRAY => {
                sort!(i8, 0, |v| JSValue::from(v as i32))
            }
            JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                sort!(u8, 0, |v| JSValue::from(v as i32))
            }
            JS_CLASS_INT16_ARRAY => {
                sort!(i16, 1, |v| JSValue::from(v as i32))
            }
            JS_CLASS_UINT16_ARRAY => {
                sort!(u16, 1, |v| JSValue::from(v as i32))
            }
            JS_CLASS_INT32_ARRAY => {
                sort!(i32, 2, JSValue::from)
            }
            JS_CLASS_UINT32_ARRAY => {
                sort!(u32, 2, JSValue::from)
            }
            JS_CLASS_FLOAT16_ARRAY => abuf_content.with_bytes_mut(|buf| -> Result<(), i32> {
                unsafe {
                    let p = buf.as_mut_ptr().cast::<u16>();
                    let len = buf.len() >> 1;
                    let mut vec: Vec<f32> =
                        (0..len).map(|i| f16_to_f64(*p.add(i)) as f32).collect();

                    if let Some(cmp_fn) = cmp.as_object() {
                        let mut has_error = false;
                        vec.sort_by(|a, b| {
                            if !has_error {
                                if let Ok(r) = call_cmp_fn(
                                    ctx,
                                    cmp_fn,
                                    JSValue::from(*a as f64),
                                    JSValue::from(*b as f64),
                                ) {
                                    r
                                } else {
                                    has_error = true;
                                    Ordering::Equal
                                }
                            } else {
                                Ordering::Equal
                            }
                        });
                        if has_error {
                            return Err(-1);
                        }
                    } else {
                        sort_f32_slice(&mut vec);
                    }

                    for (i, &v) in vec.iter().enumerate() {
                        *p.add(i) = f64_to_f16(v as f64);
                    }
                }
                Ok(())
            }),
            JS_CLASS_FLOAT32_ARRAY => abuf_content.with_bytes_mut(|buf| -> Result<(), i32> {
                let slice = unsafe {
                    std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<f32>(), buf.len() >> 2)
                };

                if let Some(cmp_fn) = cmp.as_object() {
                    let mut has_error = false;
                    slice.sort_by(|a, b| {
                        if !has_error {
                            if let Ok(r) = call_cmp_fn(
                                ctx,
                                cmp_fn,
                                JSValue::from(*a as f64),
                                JSValue::from(*b as f64),
                            ) {
                                r
                            } else {
                                has_error = true;
                                Ordering::Equal
                            }
                        } else {
                            Ordering::Equal
                        }
                    });

                    if has_error { Err(-1) } else { Ok(()) }
                } else {
                    sort_f32_slice(slice);
                    Ok(())
                }
            }),
            JS_CLASS_FLOAT64_ARRAY => abuf_content.with_bytes_mut(|buf| -> Result<(), i32> {
                let slice = unsafe {
                    std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<f64>(), buf.len() >> 3)
                };

                if let Some(cmp_fn) = cmp.as_object() {
                    let mut has_error = false;
                    slice.sort_by(|a, b| {
                        if !has_error {
                            if let Ok(r) =
                                call_cmp_fn(ctx, cmp_fn, JSValue::from(*a), JSValue::from(*b))
                            {
                                r
                            } else {
                                has_error = true;
                                Ordering::Equal
                            }
                        } else {
                            Ordering::Equal
                        }
                    });

                    if has_error { Err(-1) } else { Ok(()) }
                } else {
                    sort_f64_slice(slice);
                    Ok(())
                }
            }),
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                ctx.throw_type_error("BigInt64 is not supported");
                Err(-1)
            }
            _ => unreachable!(),
        };

        // restore buffer content
        ta.array_buffer().payload_mut().replace(abuf_content);

        r?
    }

    Ok(this_val)
}

fn js_typed_array_toSorted(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(p) = cc.this().as_object_TypedArray() else {
        ctx.throw_type_error("not a TypedArray");
        return Err(-1);
    };

    let arr = p.create_to_new(ctx, JSValue::UNDEFINED, p.class_id())?;
    js_typed_array_sort(ctx, CallContext::new_simple(arr.into(), cc.arguments()))
}

fn js_typed_array_join(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let to_locale = cc.magic() != 0;

    let this = cc.this();

    let n = js_typed_array_get_length_internal(ctx, this)?;
    let sep: String = if !to_locale && cc.length() > 0 && !cc.arg(0).is_undefined() {
        let s = cc.arg(0).to_string(ctx)?;
        s.content().encode_utf8(false).to_string()
    } else {
        ",".into() // default separator
    };

    let mut sbuf = StringBuf::with_capacity(n as _, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    // XXX: optimize with direct access
    for i in 0..n {
        if i > 0 {
            sbuf.push_str(&sep).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }

        let el = this.get_property_u32(ctx, i)?;
        // Can return undefined for example if the typed array is detached
        if !el.is_undefined_or_null() {
            let s = if to_locale {
                el.to_locale_string(ctx)?.to_string(ctx)?
            } else {
                el.to_string(ctx)?
            };

            if sbuf.capacity() - sbuf.len() < s.len() {
                sbuf.try_reserve((n - i) as usize * 2).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?
            }
            sbuf.push_content(s.content()).unwrap();
        }
    }

    sbuf.shrink_to_fit();
    ctx.create_string_with_buf(sbuf).map(JSValue::make_string)
}

fn js_typed_array_indexOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let special = cc.magic();

    let len = js_typed_array_get_length_internal(ctx, cc.this())? as i32;
    if len == 0 {
        return Ok(if special == special_includes {
            JSValue::from(false)
        } else {
            JSValue::from(-1)
        });
    }

    let mut k: i32;
    let mut d: f64;
    let inc: i32;

    if special == special_lastIndexOf {
        k = len - 1;
        if cc.length() > 1 {
            d = cc.arg(1).to_number(ctx)?.to_f64();
            if d.is_nan() {
                k = 0;
            } else if d >= 0.0 {
                if d < k as f64 {
                    k = d as i32;
                }
            } else {
                d += len as f64;
                if d < 0.0 {
                    return Ok(if special == special_includes {
                        JSValue::from(false)
                    } else {
                        JSValue::from(-1)
                    });
                }
                k = d as i32;
            }
        }
        inc = -1;
    } else {
        k = if cc.length() > 1 {
            cc.arg(1).to_number(ctx)?.to_i32_clamp(0, len, len)
        } else {
            0
        };
        inc = 1;
    }

    let ta = cc.this().as_object_TypedArray().unwrap();
    let mut res = -1;

    if ta.is_detached() {
        // if the array was detached, no need to go further (but no exception is raised)
        // "includes" scans all the properties, so "undefined" can match
        if special == special_includes && cc.arg(0).is_undefined() && len > 0 {
            res = 0;
        }
        return Ok(if special == special_includes {
            JSValue::from(res >= 0)
        } else {
            JSValue::from(res)
        });
    }

    let _is_bigint = false;
    let is_int: bool; // avoid warning
    let v64: i64;

    // check value to find
    let v = cc.arg(0);
    if let Some(i) = v.as_int() {
        is_int = true;
        v64 = i as i64;
        d = v64 as f64;
    } else if let Some(f) = v.as_float() {
        d = f;
        v64 = d as i64;
        is_int = v64 as f64 == d;
    } else {
        return Ok(if special == special_includes {
            JSValue::from(false)
        } else {
            JSValue::from(-1)
        });
    }

    macro_rules! find {
        ($ty:ident, $shift:expr, $v:expr) => {
            let found = ta.with_bytes(|buf| {
                let lst = unsafe {
                    std::slice::from_raw_parts(buf.as_ptr().cast::<$ty>(), buf.len() >> $shift)
                };
                let it = lst.iter().enumerate();

                if inc > 0 {
                    debug_assert!(k >= 0);
                    it.skip(k as usize)
                        .find_map(|(i, x)| if *x == $v { Some(i) } else { None })
                } else {
                    debug_assert!((0..lst.len() as i32).contains(&k));
                    it.take(k as usize + 1)
                        .rev()
                        .find_map(|(i, x)| if *x == $v { Some(i) } else { None })
                }
            });

            if let Some(i) = found {
                res = i as i32;
            }
        };
    }

    match ta.class_id() {
        JS_CLASS_INT8_ARRAY if is_int && (v64 as i8) as i64 == v64 => {
            let v: i8 = v64 as i8;
            find!(i8, 0, v);
        }
        JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY if is_int && (v64 as u8) as i64 == v64 => {
            let v: u8 = v64 as u8;
            find!(u8, 0, v);
        }
        JS_CLASS_INT16_ARRAY if is_int && (v64 as i16) as i64 == v64 => {
            let v = v64 as i16;
            find!(i16, 1, v);
        }
        JS_CLASS_UINT16_ARRAY if is_int && (v64 as u16) as i64 == v64 => {
            let v = v64 as u16;
            find!(u16, 1, v);
        }
        JS_CLASS_INT32_ARRAY if is_int && (v64 as i32) as i64 == v64 => {
            let v = v64 as i32;
            find!(i32, 2, v);
        }

        JS_CLASS_UINT32_ARRAY if is_int && (v64 as u32) as i64 == v64 => {
            let v = v64 as u32;
            find!(u32, 2, v);
        }
        JS_CLASS_FLOAT16_ARRAY => {
            if d.is_nan() {
                if special == special_includes {
                    // includes finds NaN
                    let found =
                        ta.with_elements_as::<u16, Option<usize>>(|lst| {
                            if inc > 0 {
                                lst.iter()
                                    .skip(k as usize)
                                    .position(|x| f16_to_f64(*x).is_nan())
                            } else {
                                lst.iter().enumerate().take(k as usize + 1).rev().find_map(
                                    |(i, x)| {
                                        if f16_to_f64(*x).is_nan() {
                                            Some(i)
                                        } else {
                                            None
                                        }
                                    },
                                )
                            }
                        });

                    if let Some(i) = found {
                        res = i as i32;
                    }
                } else {
                    // indexOf() returns -1
                    return Ok(JSValue::from(-1));
                }
            } else if d == 0.0 {
                // Zero: Both +0 (0x0000) and -0 (0x8000) should match
                // due to SameValueZero semantics: (+0, -0) are equal
                let found = ta.with_bytes(|buf| {
                    let lst = unsafe {
                        std::slice::from_raw_parts(buf.as_ptr().cast::<u16>(), buf.len() >> 1)
                    };
                    let it = lst.iter().enumerate();
                    if inc > 0 {
                        debug_assert!(k >= 0);
                        it.skip(k as usize).find_map(|(i, x)| {
                            if *x == 0x0000 || *x == 0x8000 {
                                Some(i)
                            } else {
                                None
                            }
                        })
                    } else {
                        debug_assert!((0..lst.len() as i32).contains(&k));
                        it.take(k as usize + 1).rev().find_map(|(i, x)| {
                            if *x == 0x0000 || *x == 0x8000 {
                                Some(i)
                            } else {
                                None
                            }
                        })
                    }
                });
                if let Some(i) = found {
                    res = i as i32;
                }
            } else {
                // Need to check exact match: convert d to f16 and back, compare
                let f16 = f64_to_f16(d);
                let f = f16_to_f64(f16);
                if f == d {
                    find!(u16, 1, f16);
                }
            }
        }
        JS_CLASS_FLOAT32_ARRAY => {
            if d.is_nan() {
                if special == special_includes {
                    // includes finds NaN
                    let found = ta.with_elements_as::<f32, Option<usize>>(|lst| {
                        if inc > 0 {
                            lst.iter().skip(k as usize).position(|x| x.is_nan())
                        } else {
                            lst.iter()
                                .enumerate()
                                .take(k as usize + 1)
                                .rev()
                                .find_map(|(i, x)| if x.is_nan() { Some(i) } else { None })
                        }
                    });

                    if let Some(i) = found {
                        res = i as i32;
                    }
                } else {
                    // indexOf() returns -1
                    return Ok(JSValue::from(-1));
                }
            } else {
                let f = d as f32;
                if f as f64 == d {
                    find!(f32, 2, f);
                }
            }
        }
        JS_CLASS_FLOAT64_ARRAY => {
            if d.is_nan() {
                if special == special_includes {
                    // includes finds NaN
                    let found = ta.with_elements_as::<f64, Option<usize>>(|lst| {
                        if inc > 0 {
                            lst.iter().skip(k as usize).position(|x| x.is_nan())
                        } else {
                            lst.iter()
                                .enumerate()
                                .take(k as usize + 1)
                                .rev()
                                .find_map(|(i, x)| if x.is_nan() { Some(i) } else { None })
                        }
                    });
                    if let Some(i) = found {
                        res = i as i32;
                    }
                } else {
                    // indexOf() returns -1
                    return Ok(JSValue::from(-1));
                }
            } else {
                find!(f64, 3, d);
            }
        }
        JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
            ctx.throw_type_error("BigInt64 is not supported");
            return Err(-1);
        }

        _ => {}
    }

    Ok(if special == special_includes {
        JSValue::from(res >= 0)
    } else {
        JSValue::from(res)
    })
}

pub const js_typed_array_base_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("length", Some(js_typed_array_get_length), None, 0),
    FieldDef::FUNC("at", 1, js_typed_array_at, 0),
    FieldDef::FUNC("with", 2, js_typed_array_with, 0),
    FieldDef::GETSET(
        "buffer",
        Some(js_typed_array_getter),
        None,
        TA_GET_BUFFER as _,
    ),
    FieldDef::GETSET(
        "byteLength",
        Some(js_typed_array_getter),
        None,
        TA_GET_BYTE_LENGTH as _,
    ),
    FieldDef::GETSET(
        "byteOffset",
        Some(js_typed_array_getter),
        None,
        TA_GET_BYTE_OFFSET as _,
    ),
    FieldDef::FUNC("set", 1, js_typed_array_set, 0),
    FieldDef::FUNC(
        "values",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16,
    ),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::ALIAS("[Symbol.iterator]", "values"),
    FieldDef::FUNC(
        "keys",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16,
    ),
    FieldDef::FUNC(
        "entries",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16,
    ),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::GETSET(
        "[Symbol.toStringTag]",
        Some(js_typed_array_get_toStringTag),
        None,
        0,
    ),
    FieldDef::FUNC("copyWithin", 2, js_typed_array_copyWithin, 0),
    FieldDef::FUNC(
        "every",
        1,
        js_array_every,
        (special_every | special_TA) as i16,
    ),
    FieldDef::FUNC(
        "some",
        1,
        js_array_every,
        (special_some | special_TA) as i16,
    ),
    FieldDef::FUNC(
        "forEach",
        1,
        js_array_every,
        (special_forEach | special_TA) as i16,
    ),
    FieldDef::FUNC("map", 1, js_array_every, (special_map | special_TA) as i16),
    FieldDef::FUNC(
        "filter",
        1,
        js_array_every,
        (special_filter | special_TA) as i16,
    ),
    FieldDef::FUNC(
        "reduce",
        1,
        js_array_reduce,
        (special_reduce | special_TA) as i16,
    ),
    FieldDef::FUNC(
        "reduceRight",
        1,
        js_array_reduce,
        (special_reduceRight | special_TA) as i16,
    ),
    FieldDef::FUNC("fill", 1, js_typed_array_fill, 0),
    FieldDef::FUNC("find", 1, js_typed_array_find, special_find as i16),
    FieldDef::FUNC(
        "findIndex",
        1,
        js_typed_array_find,
        special_findIndex as i16,
    ),
    FieldDef::FUNC("findLast", 1, js_typed_array_find, special_findLast as i16),
    FieldDef::FUNC(
        "findLastIndex",
        1,
        js_typed_array_find,
        special_findLastIndex as i16,
    ),
    FieldDef::FUNC("reverse", 0, js_typed_array_reverse, 0),
    FieldDef::FUNC("toReversed", 0, js_typed_array_toReversed, 0),
    FieldDef::FUNC("slice", 2, js_typed_array_slice, 0),
    FieldDef::FUNC("subarray", 2, js_typed_array_subarray, 0),
    FieldDef::FUNC("sort", 1, js_typed_array_sort, 0),
    FieldDef::FUNC("toSorted", 1, js_typed_array_toSorted, 0),
    FieldDef::FUNC("join", 1, js_typed_array_join, 0),
    FieldDef::FUNC("toLocaleString", 0, js_typed_array_join, 1),
    FieldDef::FUNC("indexOf", 1, js_typed_array_indexOf, special_indexOf as i16),
    FieldDef::FUNC(
        "lastIndexOf",
        1,
        js_typed_array_indexOf,
        special_lastIndexOf as i16,
    ),
    FieldDef::FUNC(
        "includes",
        1,
        js_typed_array_indexOf,
        special_includes as i16,
    ),
];
