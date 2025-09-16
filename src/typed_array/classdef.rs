use std::cmp::Ordering;

use crate::{
    JS_ALIAS_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_ARRAY_BUFFER,
    JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_FLOAT32_ARRAY,
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY,
    JS_CLASS_SHARED_ARRAY_BUFFER, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    array::{
        js_array_every, js_array_reduce, special_TA, special_every, special_filter, special_find,
        special_findIndex, special_findLast, special_findLastIndex, special_forEach,
        special_includes, special_indexOf, special_lastIndexOf, special_map, special_reduce,
        special_reduceRight, special_some,
    },
    atom::consts::JS_ATOM_Symbol_iterator,
    class::JSClassID,
    context::JSContext,
    error::{JS_ThrowTypeErrorDetachedArrayBuffer, JSResult},
    function::{CallContext, FieldDef, JS_CallConstructor},
    iterator::{JSIteratorKindEnum, js_create_array_iterator},
    js_core::js_get_this,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_SpeciesConstructor, js_get_length64},
    string::StringBuf,
    typed_array::{Float32Array, Float64Array, typed_array_size_log2},
    value::{HINT_NUMBER, JSValue, JSValueConst},
};

use super::{TypedArray, js_typed_array_get_length_internal, js_typed_array_set_internal};

pub fn js_typed_array_base_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    ctx.throw_type_error("cannot be called");
    Err(-1)
}

pub fn js_typed_array___speciesCreate(
    ctx: &JSContext,
    this_val: &JSValueConst,
    args: &[JSValue],
) -> JSResult {
    let obj = &args[0];
    let p = obj.clone().typed_array_must(ctx, false)?;

    let ctor = JS_SpeciesConstructor(ctx, &obj, JSValue::UNDEFINED)?;
    let args_list = if args.len() > 1 { &args[1..] } else { &[] };
    if ctor.is_undefined() {
        js_typed_array_constructor(
            ctx,
            CallContext::new(ctx, JSValue::UNDEFINED, args_list, p.class_id() as _, None),
        )
    } else {
        js_typed_array_create(ctx, &ctor, args_list)
    }
}

pub fn js_typed_array_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let class_id = cc.magic();

    if let JSValue::Object(p) = cc.arg(0) {
        if matches!(
            p.class_id(),
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ) {
            let offset = cc.arg(1).to_array_index(ctx)? as usize;

            let a2 = cc.arg(2);
            let len = if !a2.is_undefined() {
                a2.to_array_index(ctx)? as usize
            } else {
                0
            };

            ctx.create_typed_array_from(new_target, cc.arg(0), offset, len, class_id as _)
        } else {
            ctx.create_typed_array_from(new_target, cc.arg(0), 0, 0, class_id as _)
        }
    } else {
        let len = cc.arg(0).to_array_index(ctx)? as usize;
        ctx.create_typed_array_from(new_target, &JSValue::UNDEFINED, 0, len, class_id as _)
    }
}

pub unsafe fn js_typed_array_constructor_obj(
    ctx: &JSContext,
    new_target: &JSValueConst,
    obj: &JSValueConst,
    classid: i32,
) -> JSValue {
    ctx.create_typed_array_from(new_target, obj, 0, 0, classid as _)
        .unwrap_or(JSValue::EXCEPTION)

    // let size_log2 = typed_array_size_log2(classid as _);

    // let ret = js_create_from_ctor(ctx, new_target, classid as _);
    // if ret.is_exception() {
    //     return JSValue::EXCEPTION;
    // }

    // let iter = obj.get_property(ctx, JS_ATOM_Symbol_iterator);
    // if iter.is_exception() {
    //     JS_FreeValue(ctx, ret);
    //     return JSValue::EXCEPTION;
    // }

    // let arr: JSValue;
    // let len: i64;

    // if !iter.is_undefined_or_null() {
    //     let mut len1: u32 = 0;
    //     arr = js_array_from_iterator(ctx, &mut len1 as *mut u32, obj, iter);
    //     JS_FreeValue(ctx, iter);
    //     if arr.is_exception() {
    //         JS_FreeValue(ctx, ret);
    //         return JSValue::EXCEPTION;
    //     }
    //     len = len1 as i64;
    // } else {
    //     len = if let Ok(l) = js_get_length64(ctx, obj) {
    //         l
    //     } else {
    //         JS_FreeValue(ctx, ret);
    //         return JSValue::EXCEPTION;
    //     };
    //     arr = obj.dup();
    // }

    // let buffer = js_array_buffer_constructor1(ctx, JSValue::UNDEFINED, (len as u64) << size_log2);
    // if buffer.is_exception() {
    //     JS_FreeValue(ctx, arr);
    //     JS_FreeValue(ctx, ret);
    //     return JSValue::EXCEPTION;
    // }

    // if typed_array_init(ctx, ret, buffer, 0, len as _) != 0 {
    //     JS_FreeValue(ctx, arr);
    //     JS_FreeValue(ctx, ret);
    //     JS_FreeValue(ctx, buffer);
    //     return JSValue::EXCEPTION;
    // }

    // for i in 0..len {
    //     let val = arr.get_property_u32(ctx, i as u32);
    //     if val.is_exception() {
    //         JS_FreeValue(ctx, arr);
    //         JS_FreeValue(ctx, ret);
    //         JS_FreeValue(ctx, buffer);
    //         return JSValue::EXCEPTION;
    //     }

    //     if ret
    //         .set_property_u32(ctx, i as u32, val)
    //         .is_err()
    //     {
    //         JS_FreeValue(ctx, arr);
    //         JS_FreeValue(ctx, ret);
    //         JS_FreeValue(ctx, buffer);
    //         return JSValue::EXCEPTION;
    //     }
    // }

    // JS_FreeValue(ctx, arr);
    // return ret;
}

pub fn js_typed_array_constructor_ta(
    ctx: &JSContext,
    new_target: &JSValue,
    src_obj: JSValue,
    classid: i32,
) -> JSResult {
    TryInto::<TypedArray>::try_into(src_obj)
        .and_then(|ta| ta.create_to_new(ctx, new_target, classid as JSClassID))
}

pub fn js_typed_array_create(ctx: &JSContext, ctor: &JSValue, args: &[JSValue]) -> JSResult {
    let ret = JS_CallConstructor(ctx, ctor, args).err()?;

    // validate the typed array
    let new_len = unsafe { js_typed_array_get_length_internal(ctx, &ret)? };

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

pub fn js_typed_array_from(ctx: &JSContext, args: CallContext) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)
    let items = args.arg(0);

    let mut mapping = false;
    let mut mapfn = &JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    let argc = args.length();
    if argc > 1 {
        mapfn = args.arg(1);
        if !mapfn.is_undefined() {
            mapfn.ensure_callable(ctx)?;
            mapping = true;
            if argc > 2 {
                this_arg = args.arg(2).clone();
            }
        }
    }

    let arr: JSValue = if !items
        .get_property(ctx, JS_ATOM_Symbol_iterator)?
        .is_undefined()
    {
        // target is enumerable
        let fa = ctx.create_array()?;
        for (i, v) in items.iter(ctx)?.enumerate() {
            fa.0.define_property_value_i64(ctx, i as _, v?, JS_PROP_C_W_E | JS_PROP_THROW)?;
        }
        fa.into()
    } else {
        items.clone().ensure_object(ctx)?.into()
    };

    let len = js_get_length64(ctx, &arr)?;
    let len_val = JSValue::from(len);
    let res_ta = js_typed_array_create(ctx, args.this(), &[len_val])?;

    for i in 0..len {
        let mut el = arr.get_property_i64(ctx, i)?;
        if mapping {
            el = mapfn.call(ctx, &this_arg, &[el, JSValue::Int(i as i32)])?;
        }
        res_ta.set_property_i64(ctx, i, el)?;
    }

    Ok(res_ta)
}

pub fn js_typed_array_of(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = js_typed_array_create(ctx, cc.this(), &[JSValue::Int(cc.length() as i32)])?;

    for i in 0..cc.length() {
        obj.set_property_u32(ctx, i as u32, cc.arg(i).clone())?;
    }

    Ok(obj)
}

pub static js_typed_array_base_funcs: [FieldDef; 3] = [
    JS_CFUNC_NATIVE_DEF!("from", 1, js_typed_array_from, 0),
    JS_CFUNC_NATIVE_DEF!("of", 0, js_typed_array_of, 0),
    JS_CGETSET_NATIVE_DEF!("[Symbol.species]", js_get_this, NONE, 0),
];

//
// Prototype
//

pub fn js_typed_array_get_length(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.this()
        .clone()
        .typed_array_must(ctx, false)
        .map(|p| JSValue::Int(p.length() as i32))
}

pub fn js_typed_array_get_buffer(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_dataview = cc.magic() != 0;
    cc.this()
        .clone()
        .typed_array_must(ctx, is_dataview)
        .map(|p| JSValue::Object(p.with(|ta| ta.buffer.clone())))
}

pub fn js_typed_array_get_byteLength(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_dataview = cc.magic() != 0;

    let p = cc.this().clone().typed_array_must(ctx, is_dataview)?;
    if p.is_detached() {
        if is_dataview {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            Err(-1)
        } else {
            Ok(JSValue::Int(0))
        }
    } else {
        Ok(JSValue::Int(p.with(|ta| ta.byte_length) as i32))
    }
}

pub fn js_typed_array_get_byteOffset(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_dataview = cc.magic() != 0;
    let p = cc.this().clone().typed_array_must(ctx, is_dataview)?;
    p.array_buffer_byte_offset(ctx, is_dataview)
        .map(|n| JSValue::Int(n as i32))
}

pub fn js_typed_array_at(ctx: &JSContext, cc: CallContext) -> JSResult {
    let p = cc.this().clone().typed_array_must(ctx, false)?;
    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let mut idx: i64 = cc.arg(0).to_i64_saturating(ctx)?;
    let len = p.length() as i64;
    if idx < 0 {
        idx += len;
    }
    if idx < 0 || idx >= len {
        ctx.throw_range_error("invalid array index");
        return Err(-1);
    }

    cc.this().get_property_value(ctx, &JSValue::from(idx))
}

pub fn js_typed_array_with(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this().clone();
    let p = this_val.clone().typed_array_must(ctx, false)?;

    let mut idx: i64 = cc.arg(0).to_i64_saturating(ctx)?;
    let len = p.length() as i64;
    if idx < 0 {
        idx += len;
    }
    if idx < 0 || idx >= len {
        ctx.throw_range_error("out of bound");
        return Err(-1);
    }

    let val = cc.arg(1).to_primitive(ctx, HINT_NUMBER)?;
    let arr = js_typed_array_constructor_ta(ctx, &JSValue::UNDEFINED, this_val, p.class_id() as _)?;
    arr.set_property_i64(ctx, idx, val)?;
    Ok(arr)
}

fn js_typed_array_set(ctx: &JSContext, cc: CallContext) -> JSResult {
    let offset = if cc.length() > 1 {
        cc.arg(1)
    } else {
        &JSValue::UNDEFINED
    };
    js_typed_array_set_internal(ctx, cc.this(), cc.arg(0), offset)
}

fn js_create_typed_array_iterator(ctx: &JSContext, cc: CallContext) -> JSResult {
    let tao = cc.this().clone().typed_array_must(ctx, false)?;
    if tao.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        Err(-1)
    } else {
        js_create_array_iterator(ctx, cc)
    }
}

fn js_typed_array_get_toStringTag(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let Ok(tao) = TypedArray::try_from(cc.this().clone()) {
        let class_name = ctx.rt().class_defs[tao.class_id() as usize].class_name;
        class_name.to_string(ctx).map(JSValue::String)
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

fn js_typed_array_copyWithin(ctx: &JSContext, cc: CallContext) -> JSResult {
    let len = unsafe { js_typed_array_get_length_internal(ctx, cc.this())? as i32 };

    let to: i32 = cc.arg(0).to_i32_clamp(ctx, 0, len, len)?;
    let from: i32 = cc.arg(1).to_i32_clamp(ctx, 0, len, len)?;
    let final_ = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_i32_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    let count = std::cmp::min(final_ - from, len - to);
    if count > 0 {
        let tao: TypedArray = cc.this().clone().try_into()?;
        if tao.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        let shift = typed_array_size_log2(tao.class_id());

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

    Ok(cc.this().clone())
}

pub fn js_typed_array_fill(ctx: &JSContext, cc: CallContext) -> JSResult {
    let ta = TypedArray::try_from(cc.this().clone())?;

    if ta.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }
    let len = ta.length();

    let start = if cc.length() > 1 {
        cc.arg(1).to_i32_clamp(ctx, 0, len as _, len as _)?
    } else {
        0
    };
    let final_ = if cc.length() > 2 && !cc.arg(2).is_undefined() {
        cc.arg(2).to_i32_clamp(ctx, 0, len as _, len as _)?
    } else {
        len as _
    };

    let shift = typed_array_size_log2(ta.class_id());
    let mut data = [0u8; 8];

    match ta.class_id() {
        c @ (JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY | JS_CLASS_UINT8C_ARRAY) => {
            data[0] = if c == JS_CLASS_INT8_ARRAY {
                cc.arg(0).to_i32(ctx)? as i8 as u8
            } else {
                cc.arg(0).coerce_to_u8_clamp(ctx)?
            };
        }
        c @ (JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY) => {
            let bb: [u8; 2] = if c == JS_CLASS_INT16_ARRAY {
                (cc.arg(0).to_i32(ctx)? as i16).to_ne_bytes()
            } else {
                (cc.arg(0).to_i32(ctx)? as u16).to_ne_bytes()
            };
            data[..2].copy_from_slice(&bb);
        }
        JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY => {
            data[..4].copy_from_slice(&cc.arg(0).to_i32(ctx)?.to_ne_bytes());
        }
        JS_CLASS_FLOAT32_ARRAY => {
            let d: f64 = cc.arg(0).to_f64(ctx)?;
            let bb = unsafe {
                #[repr(C)]
                union Float32Union {
                    f: f32,
                    uint32: u32,
                }
                let u = Float32Union { f: d as f32 };
                u.uint32
            }
            .to_ne_bytes();
            data[..4].copy_from_slice(&bb);
        }
        JS_CLASS_FLOAT64_ARRAY => {
            let d: f64 = cc.arg(0).to_f64(ctx)?;
            data.copy_from_slice(&d.to_ne_bytes());
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

    match shift {
        0 => {
            if start < final_ {
                ta.with_bytes_mut(|buf| unsafe {
                    std::ptr::write_bytes(
                        buf.as_mut_ptr().offset(start as isize),
                        data[0],
                        (final_ - start) as usize,
                    );
                });
            }
        }
        1 => {
            ta.with_bytes_mut(|buf| unsafe {
                let v = data.as_ptr().cast::<u16>().read();
                let mut ptr = buf.as_mut_ptr().cast::<u16>();
                for _ in start..final_ {
                    ptr.write(v);
                    ptr = ptr.add(1);
                }
            });
        }
        2 => {
            ta.with_bytes_mut(|buf| unsafe {
                let v = data.as_ptr().cast::<u32>().read();
                let mut ptr = buf.as_mut_ptr().cast::<u32>();
                for _ in start..final_ {
                    ptr.write(v);
                    ptr = ptr.add(1);
                }
            });
        }
        3 => {
            ta.with_bytes_mut(|buf| unsafe {
                let v = data.as_ptr().cast::<u64>().read();
                let mut ptr = buf.as_mut_ptr().cast::<u64>();
                for _ in start..final_ {
                    ptr.write(v);
                    ptr = ptr.add(1);
                }
            });
        }
        _ => unreachable!(),
    }

    Ok(ta.into())
}

pub fn js_typed_array_find(ctx: &JSContext, args: CallContext) -> JSResult {
    let this_val = args.this();

    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };
    let func = args.arg(0).ensure_callable(ctx)?;

    let this_arg = if args.length() > 1 {
        args.arg(0)
    } else {
        &JSValue::UNDEFINED
    };

    let mut k: i32;
    let end: i32;
    let dir: i32;

    if matches!(args.magic(), special_findLast | special_findLastIndex) {
        k = len - 1;
        dir = -1;
        end = -1;
    } else {
        k = 0;
        dir = 1;
        end = len;
    }

    while k != end {
        let index_val = JSValue::Int(k);
        let val = this_val.get_property_value(ctx, &index_val)?;

        let res = func.call(
            ctx,
            this_arg,
            &[val.clone(), index_val.clone(), this_val.clone()],
        )?;

        if res.to_bool().unwrap_or(true) {
            return if matches!(args.magic(), special_findIndex | special_findLastIndex) {
                Ok(index_val)
            } else {
                Ok(val)
            };
        }

        k += dir;
    }

    if matches!(args.magic(), special_findIndex | special_findLastIndex) {
        Ok(JSValue::Int(-1))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub fn js_typed_array_reverse(ctx: &JSContext, args: CallContext) -> JSResult {
    let this_val = args.this();
    // let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };
    let tao: TypedArray = this_val.clone().try_into().unwrap();

    unsafe {
        macro_rules! swap_slice {
            ($ty:ident, $shift:expr) => {
                tao.with_bytes_mut(|buf| {
                    if !buf.is_empty() {
                        let lst = std::slice::from_raw_parts_mut(
                            buf.as_mut_ptr().cast::<$ty>(),
                            buf.len() >> $shift,
                        );
                        let n = lst.len() >> 1;
                        for i in 0..(n >> 2) {
                            lst.swap(i, n - i - 1);
                        }
                    }
                });
            };
        }

        match typed_array_size_log2(tao.class_id()) {
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
    }

    Ok(tao.into())
}

pub fn js_typed_array_toReversed(ctx: &JSContext, mut args: CallContext) -> JSResult {
    let this_val = args.this_raw();
    let tao = this_val.typed_array_must(ctx, false)?;
    let arr = tao.create_to_new(ctx, &JSValue::UNDEFINED, tao.class_id())?;

    args.set_this(arr);
    js_typed_array_reverse(ctx, args)
}

pub fn js_typed_array_slice(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this_raw();

    let (src_len, shift) = {
        let p = this_val.clone().typed_array_must(ctx, false)?;
        (p.length() as i32, typed_array_size_log2(p.class_id()))
    };

    let start: i32 = cc.arg(0).to_i32_clamp(ctx, 0, src_len, src_len)?;
    let final_ = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(ctx, 0, src_len, src_len)?
    } else {
        src_len
    };

    let count = std::cmp::max(final_ - start, 0);

    // create result typed array
    let arr = js_typed_array___speciesCreate(
        ctx,
        &JSValue::UNDEFINED,
        &[this_val.clone(), JSValue::Int(count)],
    )?;

    if count > 0 {
        // copy values from src to result array
        let src = this_val.clone().typed_array_must(ctx, false)?;
        if src.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        if let Ok(dst) = arr.clone().typed_array_must(ctx, false) {
            if src.class_id() == dst.class_id()
                && dst.capacity() as i32 >= count
                && src.capacity() as i32 >= start + count
            {
                // fast path using memcpy
                src.with_bytes(|src_buf| {
                    dst.with_bytes_mut(|dst_buf| unsafe {
                        std::ptr::copy_nonoverlapping(
                            src_buf.as_ptr().offset((start << shift) as isize),
                            dst_buf.as_mut_ptr(),
                            (count << shift) as usize,
                        );
                    });
                });

                return Ok(arr);
            }
        }

        // slow path using property set
        for n in 0..count {
            let val = this_val.get_property_value(ctx, &JSValue::Int(start + n))?;
            arr.set_property_value(ctx, &JSValue::Int(n), val, JS_PROP_THROW)?;
        }
    }

    Ok(arr)
}

pub fn js_typed_array_subarray(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this().clone();

    let ta = this_val.clone().typed_array_must(ctx, false)?;
    let len = ta.length() as i32;

    let start: i32 = cc.arg(0).to_i32_clamp(ctx, 0, len, len)?;
    let final_ = if !cc.arg(1).is_undefined() {
        cc.arg(1).to_i32_clamp(ctx, 0, len, len)?
    } else {
        len
    };

    let count = std::cmp::max(final_ - start, 0);

    let byte_offset = ta.array_buffer_byte_offset(ctx, false)?;
    let log2_shift = typed_array_size_log2(ta.class_id());
    let offset = byte_offset + ((start as usize) << log2_shift);

    js_typed_array___speciesCreate(
        ctx,
        &JSValue::UNDEFINED,
        &[
            this_val,
            JSValue::Object(ta.with(|ta| ta.buffer.clone())), // array buffer
            JSValue::Int(offset as i32),
            JSValue::Int(count as i32),
        ],
    )
}

pub fn js_typed_array_sort(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    let cmp = cc.arg(0);

    if !cmp.is_undefined() && cmp.ensure_callable(ctx).is_err() {
        return Err(-1);
    }

    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? };
    if len > 1 {
        let p: TypedArray = this_val.clone().try_into()?;

        macro_rules! sort {
            ($ty:ident, $shift:expr, $cvt:expr) => {
                p.with_bytes_mut(|buf| {
                    let slice = unsafe {
                        std::slice::from_raw_parts_mut(
                            buf.as_mut_ptr().cast::<$ty>(),
                            buf.len() >> $shift,
                        )
                    };

                    if !cmp.is_undefined() {
                        let mut has_error = false;

                        slice.sort_by(|a, b| {
                            if !has_error {
                                if let Ok(n) = cmp
                                    .call(ctx, &JSValue::UNDEFINED, &[$cvt(*a), $cvt(*b)])
                                    .and_then(|v| v.to_i32(ctx))
                                {
                                    if n < 0 {
                                        Ordering::Less
                                    } else if n > 0 {
                                        Ordering::Greater
                                    } else {
                                        Ordering::Equal
                                    }
                                } else {
                                    has_error = true;
                                    Ordering::Equal
                                }
                            } else {
                                Ordering::Equal
                            }
                        });
                    } else {
                        slice.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    }
                });
            };
        }

        match p.class_id() {
            JS_CLASS_INT8_ARRAY => {
                sort!(i8, 0, |v| JSValue::Int(v as i32));
            }
            JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                sort!(u8, 0, |v| JSValue::Int(v as i32));
            }
            JS_CLASS_INT16_ARRAY => {
                sort!(i16, 1, |v| JSValue::Int(v as i32));
            }
            JS_CLASS_UINT16_ARRAY => {
                sort!(u16, 1, |v| JSValue::Int(v as i32));
            }
            JS_CLASS_INT32_ARRAY => {
                sort!(i32, 2, JSValue::Int);
            }
            JS_CLASS_UINT32_ARRAY => {
                sort!(u32, 2, JSValue::from);
            }
            JS_CLASS_FLOAT32_ARRAY => {
                sort!(f32, 2, |v| JSValue::from(v as f64));
            }
            JS_CLASS_FLOAT64_ARRAY => {
                sort!(f64, 3, JSValue::from);
            }
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                ctx.throw_type_error("BigInt64 is not supported");
                return Err(-1);
            }
            _ => unreachable!(),
        }
    }

    Ok(this_val.clone())
}

pub fn js_typed_array_toSorted(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    let p = this_val.clone().typed_array_must(ctx, false)?;
    let arr = p.create_to_new(ctx, &JSValue::UNDEFINED, p.class_id())?;

    let cc = CallContext::new(ctx, arr, &[], 0, None);
    js_typed_array_sort(ctx, cc)
}

pub fn js_typed_array_join(ctx: &JSContext, cc: CallContext) -> JSResult {
    let to_locale = cc.magic() != 0;
    let this_val = cc.this();

    let n = unsafe { js_typed_array_get_length_internal(ctx, this_val)? };
    let sep: std::borrow::Cow<'_, str> =
        if !to_locale && cc.length() > 0 && !cc.arg(0).is_undefined() {
            cc.arg(0).to_utf8(ctx)?
        } else {
            ",".into() // default separator
        };

    let mut txt = StringBuf::with_capacity(n as usize * 2, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    // XXX: optimize with direct access
    for i in 0..n {
        if i > 0 {
            // add separator
            if txt.push_str(sep.as_ref()).is_err() {
                ctx.throw_out_of_memory();
                return Err(-1);
            };
        }

        let el = this_val.get_property_u32(ctx, i)?;
        if !el.is_undefined_or_null() {
            let s = if to_locale {
                el.to_locale_string(ctx)?.to_string(ctx)?
            } else {
                el.to_string(ctx)?
            };

            if txt.capacity() - txt.len() < s.len() {
                if txt.try_reserve((n - i) as usize * 2).is_err() {
                    ctx.throw_out_of_memory();
                    return Err(-1);
                }
            }
            txt.push_content(s.content()).unwrap();
        }
    }

    txt.shrink_to_fit();
    ctx.create_string_with_buf(txt).map(JSValue::String)
}

pub fn js_typed_array_indexOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    let special = cc.magic();

    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };
    if len == 0 {
        return Ok(if special == special_includes {
            JSValue::Bool(false)
        } else {
            JSValue::Int(-1)
        });
    }

    let mut k: i32;
    let stop: i32;
    let inc: i32;
    let mut d = 0.0_f64;

    if special == special_lastIndexOf {
        k = len - 1;
        if cc.length() > 1 {
            d = cc.arg(1).to_f64(ctx)?;
            if d.is_nan() {
                k = 0;
            } else {
                if d >= 0.0 {
                    if d < k as f64 {
                        k = d as i32;
                    }
                } else {
                    d += len as f64;
                    if d < 0.0 {
                        return Ok(if special == special_includes {
                            JSValue::Bool(false)
                        } else {
                            JSValue::Int(-1)
                        });
                    }
                    k = d as i32;
                }
            }
        }
        stop = -1;
        inc = -1;
    } else {
        k = if cc.length() > 1 {
            cc.arg(1).to_i32_clamp(ctx, 0, len, len)?
        } else {
            0
        };
        stop = len;
        inc = 1;
    }

    let p: TypedArray = this_val.clone().try_into()?;
    let mut res = -1;

    if p.is_detached() {
        // if the array was detached, no need to go further (but no exception is raised)
        // "includes" scans all the properties, so "undefined" can match
        if special == special_includes && cc.arg(0).is_undefined() && len > 0 {
            res = 0;
        }
        return Ok(if special == special_includes {
            JSValue::Bool(res >= 0)
        } else {
            JSValue::Int(res)
        });
    }

    let is_bigint = false;
    let mut is_int = false; // avoid warning
    let mut v64 = 0_i64; // avoid warning

    // check value to find
    match cc.arg(0) {
        JSValue::Int(i) => {
            is_int = true;
            v64 = *i as i64;
            d = v64 as f64;
        }
        JSValue::Float64(f) => {
            d = *f;
            v64 = d as i64;
            is_int = v64 as f64 == d;
        }
        _ => {
            return Ok(if special == special_includes {
                JSValue::Bool(false)
            } else {
                JSValue::Int(-1)
            });
        }
    }

    macro_rules! find {
        ($ty:ident, $shift:expr, $v:expr) => {
            let found = p.with_bytes(|buf| {
                let lst = unsafe {
                    std::slice::from_raw_parts(buf.as_ptr().cast::<$ty>(), buf.len() >> $shift)
                };

                if inc > 0 {
                    debug_assert!(k >= 0);
                    lst.iter().skip(k as usize).position(|x| *x == $v)
                } else {
                    debug_assert!(k >= 0 && k < lst.len() as i32);
                    lst.iter()
                        .enumerate()
                        .take(k as usize + 1)
                        .rev()
                        .find_map(|(i, x)| if *x == $v { Some(i) } else { None })
                }
            });

            if let Some(i) = found {
                res = i as i32;
            }
        };
    }

    match p.class_id() {
        JS_CLASS_INT8_ARRAY => {
            if is_int && (v64 as i8) as i64 == v64 {
                let v: i8 = v64 as i8;
                find!(i8, 0, v);
            }
        }
        JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
            if is_int && (v64 as u8) as i64 == v64 {
                let v: u8 = v64 as u8;
                find!(u8, 0, v);
            }
        }
        JS_CLASS_INT16_ARRAY => {
            if is_int && (v64 as i16) as i64 == v64 {
                let v = v64 as i16;
                find!(i16, 1, v);
            }
        }
        JS_CLASS_UINT16_ARRAY => {
            if is_int && (v64 as u16) as i64 == v64 {
                let v = v64 as u16;
                find!(u16, 1, v);
            }
        }
        JS_CLASS_INT32_ARRAY => {
            if is_int && (v64 as i32) as i64 == v64 {
                let v = v64 as i32;
                find!(i32, 2, v);
            }
        }
        JS_CLASS_UINT32_ARRAY => {
            if is_int && (v64 as u32) as i64 == v64 {
                let v = v64 as u32;
                find!(u32, 2, v);
            }
        }
        JS_CLASS_FLOAT32_ARRAY => {
            if d.is_nan() {
                if special == special_includes {
                    // includes finds NaN
                    let found = Float32Array::try_from(p)?.with_elements(|lst| {
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
                    // indexOf returns -1
                    return Ok(JSValue::Int(-1));
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
                    let found = Float64Array::try_from(p)?.with_elements(|lst| {
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
                    // indexOf returns -1
                    return Ok(JSValue::Int(-1));
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
        JSValue::Bool(res >= 0)
    } else {
        JSValue::Int(res)
    })
}

pub static js_typed_array_base_proto_funcs: [FieldDef; 36] = [
    JS_CGETSET_NATIVE_DEF!("length", js_typed_array_get_length, NONE, 0),
    JS_CFUNC_NATIVE_DEF!("at", 1, js_typed_array_at, 0),
    JS_CFUNC_NATIVE_DEF!("with", 2, js_typed_array_with, 0),
    JS_CGETSET_NATIVE_DEF!("buffer", js_typed_array_get_buffer, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("byteLength", js_typed_array_get_byteLength, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("byteOffset", js_typed_array_get_byteOffset, NONE, 0),
    JS_CFUNC_NATIVE_DEF!("set", 1, js_typed_array_set, 0),
    JS_CFUNC_NATIVE_DEF!(
        "values",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16
    ),
    JS_ALIAS_DEF!("[Symbol.iterator]", "values"),
    JS_CFUNC_NATIVE_DEF!(
        "keys",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "entries",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16
    ),
    JS_CGETSET_NATIVE_DEF!(
        "[Symbol.toStringTag]",
        js_typed_array_get_toStringTag,
        NONE,
        0
    ),
    JS_CFUNC_NATIVE_DEF!("copyWithin", 2, js_typed_array_copyWithin, 0),
    JS_CFUNC_NATIVE_DEF!(
        "every",
        1,
        js_array_every,
        (special_every | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "some",
        1,
        js_array_every,
        (special_some | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "forEach",
        1,
        js_array_every,
        (special_forEach | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!("map", 1, js_array_every, (special_map | special_TA) as i16),
    JS_CFUNC_NATIVE_DEF!(
        "filter",
        1,
        js_array_every,
        (special_filter | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "reduce",
        1,
        js_array_reduce,
        (special_reduce | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "reduceRight",
        1,
        js_array_reduce,
        (special_reduceRight | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!("fill", 1, js_typed_array_fill, 0),
    JS_CFUNC_NATIVE_DEF!("find", 1, js_typed_array_find, special_find as i16),
    JS_CFUNC_NATIVE_DEF!(
        "findIndex",
        1,
        js_typed_array_find,
        special_findIndex as i16
    ),
    JS_CFUNC_NATIVE_DEF!("findLast", 1, js_typed_array_find, special_findLast as i16),
    JS_CFUNC_NATIVE_DEF!(
        "findLastIndex",
        1,
        js_typed_array_find,
        special_findLastIndex as i16
    ),
    JS_CFUNC_NATIVE_DEF!("reverse", 0, js_typed_array_reverse, 0),
    JS_CFUNC_NATIVE_DEF!("toReversed", 0, js_typed_array_toReversed, 0),
    JS_CFUNC_NATIVE_DEF!("slice", 2, js_typed_array_slice, 0),
    JS_CFUNC_NATIVE_DEF!("subarray", 2, js_typed_array_subarray, 0),
    JS_CFUNC_NATIVE_DEF!("sort", 1, js_typed_array_sort, 0),
    JS_CFUNC_NATIVE_DEF!("toSorted", 1, js_typed_array_toSorted, 0),
    JS_CFUNC_NATIVE_DEF!("join", 1, js_typed_array_join, 0),
    JS_CFUNC_NATIVE_DEF!("toLocaleString", 0, js_typed_array_join, 1),
    JS_CFUNC_NATIVE_DEF!("indexOf", 1, js_typed_array_indexOf, special_indexOf as i16),
    JS_CFUNC_NATIVE_DEF!(
        "lastIndexOf",
        1,
        js_typed_array_indexOf,
        special_lastIndexOf as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        "includes",
        1,
        js_typed_array_indexOf,
        special_includes as i16
    ),
];
