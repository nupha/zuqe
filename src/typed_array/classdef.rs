use std::cmp::Ordering;

use crate::{
    array::{
        js_array_every, js_array_reduce, special_TA, special_every, special_filter, special_find,
        special_findIndex, special_findLast, special_findLastIndex, special_forEach,
        special_includes, special_indexOf, special_lastIndexOf, special_map, special_reduce,
        special_reduceRight, special_some, JS_NewArray,
    },
    atom::consts::JS_ATOM_Symbol_iterator,
    bytecode::js_for_of_start,
    class::JSClassID,
    context::JSContext,
    error::{JSResult, JS_ThrowRangeError, JS_ThrowTypeErrorDetachedArrayBuffer},
    function::{CallArgs, JSCFunctionListEntry, JS_Call, JS_CallConstructor},
    iterator::{js_create_array_iterator, JSIteratorKindEnum, JS_IteratorClose, JS_IteratorNext},
    js_core::js_get_this,
    typed_array::typed_array_size_log2,
    value::{
        object::{
            js_get_length64, JSObject, JS_SpeciesConstructor, JS_ToObject, JS_PROP_C_W_E,
            JS_PROP_THROW,
        },
        JSTag, JSValue, JSValueConst, JSValueRc, JS_FreeValue, JS_ToBoolFree, JS_ToFloat64,
        JS_ToIndex, JS_ToInt32Clamp, JS_ToInt64Sat, JS_ToLengthFree, JS_ToUint32,
        JS_ToUint8ClampFree, StringContent, HINT_NUMBER, JS_TAG_OBJECT, JS_VALUE_GET_FLOAT64,
        JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_TAG,
    },
    JS_ALIAS_DEF, JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF,
    JS_CGETSET_MAGIC_DEF, JS_CLASS_ARRAY_BUFFER, JS_CLASS_BIG_INT64_ARRAY,
    JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY,
    JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_SHARED_ARRAY_BUFFER,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT8_ARRAY,
    JS_MKPTR,
};

use super::{js_typed_array_get_length_internal, js_typed_array_set_internal};

pub unsafe fn js_typed_array_base_constructor(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    (*ctx).JS_ThrowTypeError(c"cannot be called")
}

pub unsafe fn js_typed_array___speciesCreate(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = *argv.offset(0);
    if let Ok(p) = obj.typed_array_obj(ctx, false) {
        let ctor = JS_SpeciesConstructor(ctx, obj, JSValue::UNDEFINED);
        if ctor.is_exception() {
            return ctor;
        }

        let argc1 = std::cmp::max(argc - 1, 0);
        if ctor.is_undefined() {
            js_typed_array_constructor(
                ctx,
                JSValue::UNDEFINED,
                argc1,
                argv.offset(1),
                p.class_id() as _,
            )
        } else {
            let ret = js_typed_array_create(ctx, ctor, argc1, argv.offset(1));
            JS_FreeValue(ctx, ctor);
            ret
        }
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    classid: i32,
) -> JSValue {
    let res = if JS_VALUE_GET_TAG(*argv.offset(0)) != JS_TAG_OBJECT {
        let mut len = 0;
        if JS_ToIndex(ctx, &mut len, *argv.offset(0)) != 0 {
            return JSValue::EXCEPTION;
        }
        (*ctx).create_typed_array_from(
            new_target,
            JSValue::UNDEFINED,
            0,
            len as usize,
            classid as _,
        )
    } else {
        let p = (*argv.offset(0)).object_unchecked();
        if matches!(
            (*p).class_id(),
            JS_CLASS_ARRAY_BUFFER | JS_CLASS_SHARED_ARRAY_BUFFER
        ) {
            let mut offset = 0;
            if JS_ToIndex(ctx, &mut offset, *argv.offset(1)) != 0 {
                return JSValue::EXCEPTION;
            }

            let mut len = 0;
            let a2 = *argv.offset(2);
            if !a2.is_undefined() {
                if JS_ToIndex(ctx, &mut len, *argv.offset(2)) != 0 {
                    return JSValue::EXCEPTION;
                }
            }
            (*ctx).create_typed_array_from(
                new_target,
                *argv,
                offset as usize,
                len as usize,
                classid as _,
            )
        } else {
            (*ctx).create_typed_array_from(new_target, *argv, 0, 0, classid as _)
        }
    };

    res.unwrap_or(JSValue::EXCEPTION)
}

pub unsafe fn js_typed_array_constructor_obj(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    obj: JSValueConst,
    classid: i32,
) -> JSValue {
    (*ctx)
        .create_typed_array_from(new_target, obj, 0, 0, classid as _)
        .unwrap_or(JSValue::EXCEPTION)

    // let size_log2 = typed_array_size_log2(classid as _);

    // let ret = js_create_from_ctor(ctx, new_target, classid as _);
    // if ret.is_exception() {
    //     return JSValue::EXCEPTION;
    // }

    // let iter = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_iterator);
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
    //     let val = arr.get_property_u32(ctx.as_mut().unwrap(), i as u32);
    //     if val.is_exception() {
    //         JS_FreeValue(ctx, arr);
    //         JS_FreeValue(ctx, ret);
    //         JS_FreeValue(ctx, buffer);
    //         return JSValue::EXCEPTION;
    //     }

    //     if ret
    //         .set_property_u32(ctx.as_mut().unwrap(), i as u32, val)
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

pub unsafe fn js_typed_array_constructor_ta(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    src_obj: JSValueConst,
    classid: i32,
) -> JSValue {
    src_obj
        .typed_array_obj_unchecked()
        .create_new(ctx.as_mut().unwrap(), new_target, classid as JSClassID)
        .unwrap_or(JSValue::EXCEPTION)
}

pub unsafe fn js_typed_array_create(
    ctx: *mut JSContext,
    ctor: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let ret = JS_CallConstructor(ctx.as_mut().unwrap(), ctor, argc, argv);
    if ret.is_exception() {
        return ret;
    }

    // validate the typed array
    let new_len = if let Ok(n) = js_typed_array_get_length_internal(ctx.as_mut().unwrap(), ret) {
        n
    } else {
        JS_FreeValue(ctx, ret);
        return JSValue::EXCEPTION;
    };

    if argc == 1 {
        // ensure that it is large enough
        let len = if let Ok(n) = JS_ToLengthFree(ctx, (*argv.offset(0)).dup()) {
            n
        } else {
            JS_FreeValue(ctx, ret);
            return JSValue::EXCEPTION;
        };

        if new_len < len as u32 {
            JS_FreeValue(ctx, ret);
            (*ctx).JS_ThrowTypeError(c"TypedArray length is too small");
            return JSValue::EXCEPTION;
        }
    }

    ret
}

pub fn js_typed_array_from(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    // from(items, mapfn = void 0, this_arg = void 0)
    let items = args.get_raw(0);

    let mut mapping = false;
    let mut mapfn = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    let argc = args.length();
    if argc > 1 {
        mapfn = args.get_raw(1);
        if !mapfn.is_undefined() {
            mapfn.ensure_callable(ctx)?;
            mapping = true;
            if argc > 2 {
                this_arg = args.get_raw(2);
            }
        }
    }

    let iter = items
        .get_property(ctx, JS_ATOM_Symbol_iterator)
        .check_exception()?
        .into_rc(ctx.rt());

    let arr: JSValueRc;

    if !iter.is_undefined() {
        arr = unsafe { JS_NewArray(ctx).check_exception()?.into_rc(ctx.rt()) };

        // call iterator() of `items` to get enum object and next() method.
        let mut stack = [items.dup(), JSValue::UNDEFINED]; // [enum_obj, next]
        if unsafe { js_for_of_start(ctx, stack.as_mut_ptr().add(1), false) } != 0 {
            return Err(-1);
        }

        let mut enum_obj = stack[0].into_rc(ctx.rt());
        let cx = ctx as *mut JSContext;
        enum_obj.on_drop(move |_, v| unsafe {
            JS_IteratorClose(cx, v, true);
        });

        let next = stack[1].into_rc(ctx.rt());
        let mut k = 0;

        loop {
            unsafe {
                let mut done = false;
                let v = JS_IteratorNext(ctx, *enum_obj, *next, 0, std::ptr::null_mut(), &mut done)
                    .check_exception()?;

                if done {
                    break;
                } else {
                    arr.define_property_value_i64(ctx, k, v, JS_PROP_C_W_E | JS_PROP_THROW)?;
                }
            }

            k += 1;
        }
    } else {
        arr = unsafe { JS_ToObject(ctx, items).check_exception()?.into_rc(ctx.rt()) };
    }

    let len = unsafe { js_get_length64(ctx, *arr)? };
    let len_val = JSValue::from_i64(len).into_rc(ctx.rt());
    let res_ta = unsafe {
        js_typed_array_create(ctx, args.this_raw(), 1, [*len_val].as_mut_ptr())
            .check_exception()?
            .into_rc(ctx.rt())
    };

    for k in 0..len {
        let mut nth_val = arr.get_property_i64(ctx, k).check_exception()?;

        if mapping {
            unsafe {
                let v2 = JS_Call(
                    ctx,
                    mapfn,
                    this_arg,
                    2,
                    [nth_val, (k as i32).into()].as_mut_ptr(),
                );
                JS_FreeValue(ctx, nth_val);
                if v2.is_exception() {
                    return Err(-1);
                }
                nth_val = v2;
            }
        }

        if res_ta.set_property_i64(ctx, k, nth_val).is_err() {
            unsafe {
                JS_FreeValue(ctx, nth_val);
            }
            return Err(-1);
        };
    }

    Ok(res_ta.into())
}

pub fn js_typed_array_of(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let obj = unsafe {
        js_typed_array_create(
            ctx,
            args.this_raw(),
            1,
            [JSValue::from_i32(args.length() as i32)].as_mut_ptr(),
        )
        .check_exception()?
    };

    for i in 0..args.length() {
        if obj
            .set_property_u32(ctx, i as u32, args.get_raw(i as usize).dup())
            .is_err()
        {
            unsafe {
                JS_FreeValue(ctx, obj);
            }
            return Err(-1);
        }
    }

    Ok(obj)
}

pub const js_typed_array_base_funcs: [JSCFunctionListEntry; 3] = [
    JS_CFUNC_NATIVE_DEF!(c"from", 1, js_typed_array_from, 0),
    JS_CFUNC_NATIVE_DEF!(c"of", 0, js_typed_array_of, 0),
    JS_CGETSET_DEF!(c"[Symbol.species]", js_get_this, NONE),
];

//
// Prototype
//

pub unsafe fn js_typed_array_get_length(ctx: *mut JSContext, this_val: JSValueConst) -> JSValue {
    if let Ok(p) = this_val.typed_array_obj(ctx, false) {
        (p.length() as i32).into()
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_get_buffer(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    is_dataview: i32,
) -> JSValue {
    if let Ok(p) = this_val.typed_array_obj(ctx, is_dataview != 0) {
        JS_MKPTR!(
            JSTag::Object,
            (p.array_buffer_obj() as *const JSObject as *mut JSObject).cast()
        )
        .dup()
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_get_byteLength(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    is_dataview: i32,
) -> JSValue {
    if let Ok(p) = this_val.typed_array_obj(ctx, is_dataview != 0) {
        if p.is_detached() {
            if is_dataview != 0 {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx)
            } else {
                JSValue::ZERO
            }
        } else {
            (p.typed_array().byte_length as i32).into()
        }
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_get_byteOffset(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    is_dataview: i32,
) -> JSValue {
    if let Ok(p) = this_val.typed_array_obj(ctx, is_dataview != 0) {
        p.array_buffer_byte_offset(ctx.as_mut().unwrap(), is_dataview != 0)
            .map_or(JSValue::EXCEPTION, |n| JSValue::from(n as i32))
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_at(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if let Ok(p) = this_val.typed_array_obj(ctx, false) {
        if p.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return JSValue::EXCEPTION;
        }

        let mut idx: i64 = 0;
        if JS_ToInt64Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
            return JSValue::EXCEPTION;
        }

        let len = p.length() as i64;
        if idx < 0 {
            idx = len + idx;
        }
        if idx < 0 || idx >= len {
            return JSValue::UNDEFINED;
        }

        this_val.get_property_i64(ctx.as_mut().unwrap(), idx)
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_with(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if let Ok(p) = this_val.typed_array_obj(ctx, false) {
        let mut idx: i64 = 0;
        if JS_ToInt64Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
            return JSValue::EXCEPTION;
        }

        let len = p.length() as i64;
        if idx < 0 {
            idx = len + idx;
        }
        if idx < 0 || idx >= len {
            return JS_ThrowRangeError(ctx, c"out of bound");
        }

        let val = if let Ok(v) =
            (*argv.offset(1)).coerce_to_primitive(ctx.as_mut().unwrap(), HINT_NUMBER)
        {
            v
        } else {
            return JSValue::EXCEPTION;
        };

        let arr =
            js_typed_array_constructor_ta(ctx, JSValue::UNDEFINED, this_val, p.class_id() as _);
        if arr.is_exception() {
            JS_FreeValue(ctx, val);
            return JSValue::EXCEPTION;
        }
        if arr
            .set_property_i64(ctx.as_mut().unwrap(), idx, val)
            .is_err()
        {
            JS_FreeValue(ctx, arr);
            return JSValue::EXCEPTION;
        }

        arr
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_set(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
) -> JSValue {
    let offset = if argc > 1 {
        *argv.offset(1)
    } else {
        JSValue::UNDEFINED
    };

    js_typed_array_set_internal(ctx, this_val, *argv.offset(0), offset)
}

pub unsafe fn js_create_typed_array_iterator(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    if let Ok(tao) = this_val.typed_array_obj(ctx, false) {
        if tao.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return JSValue::EXCEPTION;
        }
        js_create_array_iterator(ctx, this_val, argc, argv, magic)
    } else {
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_typed_array_get_toStringTag(
    ctx: *mut JSContext,
    this_val: JSValueConst,
) -> JSValue {
    if let Some(tao) = this_val.object().and_then(|x| x.as_typed_array_obj()) {
        let class_name = (*ctx).rt().class_array[tao.class_id() as usize].class_name;
        class_name.to_value(ctx.as_mut().unwrap(), true)
    } else {
        JSValue::UNDEFINED
    }
}

pub fn js_typed_array_copyWithin(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    unsafe {
        let len = js_typed_array_get_length_internal(ctx, args.this_raw())? as i32;

        let mut to: i32 = 0;
        let mut from: i32 = 0;

        if JS_ToInt32Clamp(ctx, &mut to, args.get_raw(0), 0, len, len) != 0 {
            return Err(-1);
        }
        if JS_ToInt32Clamp(ctx, &mut from, args.get_raw(1), 0, len, len) != 0 {
            return Err(-1);
        }

        let mut final_ = len;
        if args.length() > 2 && !args.get_raw(2).is_undefined() {
            if JS_ToInt32Clamp(ctx, &mut final_, args.get_raw(2), 0, len, len) != 0 {
                return Err(-1);
            }
        }

        let count = std::cmp::min(final_ - from, len - to);
        if count > 0 {
            let mut tao = args.this_raw().typed_array_obj_unchecked();
            if tao.is_detached() {
                JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
                return Err(-1);
            }

            let shift = typed_array_size_log2(tao.class_id());

            tao.with_data_mut::<u8, ()>(|buf| {
                let ptr = buf.as_mut_ptr();
                std::ptr::copy(
                    ptr.add((from << shift) as usize),
                    ptr.add((to << shift) as usize),
                    (count << shift) as usize,
                );
            });
        }

        Ok(args.this_raw().dup())
    }
}

pub fn js_typed_array_fill(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };
    let mut p = unsafe { this_val.typed_array_obj_unchecked() };

    let v64: u64;

    match p.class_id() {
        JS_CLASS_UINT8C_ARRAY => {
            let mut v: i32 = 0;
            if unsafe { JS_ToUint8ClampFree(ctx, &mut v, args.get_raw(0).dup()) } != 0 {
                return Err(-1);
            }
            v64 = v as u64;
        }
        JS_CLASS_UINT32_ARRAY => {
            v64 = unsafe { JS_ToUint32(ctx, args.get_raw(0))? as u64 };
        }
        JS_CLASS_BIG_UINT64_ARRAY => {
            //JS_ToBigInt64(ctx, &mut v64 as *mut u64 as *mut i64, (*argv.offset(0)))
            ctx.JS_ThrowTypeError(c"BigInt64 is not supported");
            return Err(-1);
        }
        cid => {
            let mut d: f64 = 0.0;
            if unsafe { JS_ToFloat64(ctx, &mut d, args.get_raw(0)) } != 0 {
                return Err(-1);
            }

            if cid == JS_CLASS_FLOAT32_ARRAY {
                #[repr(C)]
                union FloatUnion {
                    f: f32,
                    u32_: u32,
                }
                v64 = unsafe {
                    let u = FloatUnion { f: d as f32 };
                    u.u32_ as u64
                };
            } else {
                #[repr(C)]
                union DoubleUnion {
                    d: f64,
                    u64_: u64,
                };
                v64 = unsafe {
                    let u = DoubleUnion { d };
                    u.u64_
                };
            }
        }
    }

    let mut k = 0;
    if args.length() > 1 {
        if unsafe { JS_ToInt32Clamp(ctx, &mut k, args.get_raw(1), 0, len, len) } != 0 {
            return Err(-1);
        }
    }

    let mut final_ = len;
    if args.length() > 2 && !args.get_raw(2).is_undefined() {
        if unsafe { JS_ToInt32Clamp(ctx, &mut final_, args.get_raw(2), 0, len, len) } != 0 {
            return Err(-1);
        }
    }

    if p.is_detached() {
        JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
        return Err(-1);
    }

    let shift = typed_array_size_log2(p.class_id());
    match shift {
        0 => {
            if k < final_ {
                p.with_data_mut::<u8, _>(|buf| unsafe {
                    std::ptr::write_bytes(
                        buf.as_mut_ptr().offset(k as isize),
                        v64 as u8,
                        (final_ - k) as usize,
                    );
                });
            }
        }
        1 => {
            p.with_data_mut::<u16, _>(|buf| {
                for i in k..final_ {
                    buf[i as usize] = v64 as u16;
                }
            });
        }
        2 => {
            p.with_data_mut::<u32, _>(|buf| {
                for i in k..final_ {
                    buf[i as usize] = v64 as u32;
                }
            });
        }
        3 => {
            p.with_data_mut::<u64, _>(|buf| {
                for i in k..final_ {
                    buf[i as usize] = v64;
                }
            });
        }
        _ => unreachable!(),
    }

    Ok(this_val.dup())
}

pub fn js_typed_array_find(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();

    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };
    let func = args.get_raw(0).ensure_callable(ctx)?;

    let this_arg = if args.length() > 1 {
        args.get_raw(0)
    } else {
        JSValue::UNDEFINED
    };

    let mut k: i32;
    let end: i32;
    let dir: i32;

    if matches!(args.magic, special_findLast | special_findLastIndex) {
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

        let val = this_val
            .get_property_value(ctx, index_val)
            .check_exception()?
            .into_rc(ctx.rt());

        let res = unsafe {
            JS_Call(
                ctx,
                func,
                this_arg,
                3,
                [*val, index_val, this_val].as_mut_ptr(),
            )
            .check_exception()?
        };

        if unsafe { JS_ToBoolFree(ctx, res).unwrap_or(true) } {
            return if matches!(args.magic, special_findIndex | special_findLastIndex) {
                Ok(index_val)
            } else {
                Ok(val.into())
            };
        }

        k += dir;
    }

    if matches!(args.magic, special_findIndex | special_findLastIndex) {
        Ok(JSValue::from(-1))
    } else {
        Ok(JSValue::UNDEFINED)
    }
}

pub fn js_typed_array_reverse(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };

    if len > 0 {
        unsafe {
            let mut tao = this_val.typed_array_obj_unchecked();

            macro_rules! rev {
                ($t:ident) => {
                    tao.with_data_mut::<$t, _>(|lst| {
                        let n = lst.len();
                        for i in 0..(n >> 2) {
                            lst.swap(i, n - i - 1);
                        }
                    });
                };
            }

            match typed_array_size_log2(tao.class_id()) {
                0 => {
                    rev!(u8);
                }
                1 => {
                    rev!(u16);
                }
                2 => {
                    rev!(u32);
                }
                3 => {
                    rev!(u64);
                }
                _ => unreachable!(),
            }
        }
    }

    Ok(this_val.dup())
}

pub fn js_typed_array_toReversed(ctx: &mut JSContext, mut args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let mut tao = this_val.typed_array_obj(ctx.handle(), false)?;
    let arr = tao
        .create_new(ctx, JSValue::UNDEFINED, tao.class_id())?
        .into_rc(ctx.rt());

    args.set_this_raw(*arr);
    js_typed_array_reverse(ctx, args)
}

pub fn js_typed_array_slice(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();

    let (src_len, shift) = {
        let p = this_val.typed_array_obj(ctx.handle(), false)?;
        (p.length() as i32, typed_array_size_log2(p.class_id()))
    };

    let mut start: i32 = 0;
    if unsafe { JS_ToInt32Clamp(ctx, &mut start, args.get_raw(0), 0, src_len, src_len) } != 0 {
        return Err(-1);
    }

    let mut final_ = src_len;
    if !args.get(1).is_undefined() {
        if unsafe { JS_ToInt32Clamp(ctx, &mut final_, args.get_raw(1), 0, src_len, src_len) } != 0 {
            return Err(-1);
        }
    }

    let count = std::cmp::max(final_ - start, 0);

    // create result typed array
    let arr = unsafe {
        js_typed_array___speciesCreate(
            ctx,
            JSValue::UNDEFINED,
            2,
            [this_val, JSValue::from(count)].as_mut_ptr(),
        )
        .check_exception()?
        .into_rc(ctx.rt())
    };

    if count > 0 {
        // copy values from src to result array
        let cxh = ctx.handle();

        let src = this_val.typed_array_obj(cxh, false)?;
        if src.is_detached() {
            JS_ThrowTypeErrorDetachedArrayBuffer(ctx);
            return Err(-1);
        }

        if let Ok(mut dst) = arr.typed_array_obj(cxh, false) {
            if src.class_id() == dst.class_id()
                && dst.capacity() as i32 >= count
                && src.capacity() as i32 >= start + count
            {
                // fast path using memcpy
                src.with_data::<u8, ()>(|src_buf| {
                    dst.with_data_mut::<u8, ()>(|dst_buf| unsafe {
                        std::ptr::copy_nonoverlapping(
                            src_buf.as_ptr().offset((start << shift) as isize),
                            dst_buf.as_mut_ptr(),
                            (count << shift) as usize,
                        );
                    });
                });

                return Ok(arr.into());
            }
        }

        // slow path using property set
        for n in 0..count {
            let val = this_val
                .get_property_value(ctx, (start + n).into())
                .check_exception()?;
            arr.set_property_value(ctx, n.into(), val, JS_PROP_THROW)?;
        }
    }

    Ok(arr.into())
}

pub fn js_typed_array_subarray(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let len = this_val.typed_array_obj(ctx.handle(), false)?.length() as i32;

    let mut start: i32 = 0;
    if unsafe { JS_ToInt32Clamp(ctx, &mut start, args.get_raw(0), 0, len, len) } != 0 {
        return Err(-1);
    }

    let mut final_ = len;
    if !args.get_raw(1).is_undefined() {
        if unsafe { JS_ToInt32Clamp(ctx, &mut final_, args.get_raw(1), 0, len, len) } != 0 {
            return Err(-1);
        }
    }

    let count = std::cmp::max(final_ - start, 0);

    let p = unsafe { this_val.typed_array_obj_unchecked() };
    let byteOffset = p.array_buffer_byte_offset(ctx, false)?;
    let shift = typed_array_size_log2(p.class_id());
    let offset = byteOffset + (start as usize) << shift;

    unsafe {
        let ta_buffer = js_typed_array_get_buffer(ctx, this_val, 0).check_exception()?;

        let a = js_typed_array___speciesCreate(
            ctx,
            JSValue::UNDEFINED,
            4,
            [
                this_val,
                ta_buffer,
                JSValue::from(offset as i32),
                JSValue::from(count as i32),
            ]
            .as_mut_ptr(),
        );

        JS_FreeValue(ctx, ta_buffer);
        Ok(a)
    }
}

pub fn js_typed_array_sort(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let cmp = args.get_raw(0);

    if !cmp.is_undefined() && cmp.ensure_callable(ctx).is_err() {
        return Err(-1);
    }

    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? };

    if len > 1 {
        let mut p = unsafe { this_val.typed_array_obj_unchecked() };

        macro_rules! sort {
            ($ty:ident, $cvt:expr) => {
                p.with_data_mut::<$ty, _>(|slice| {
                    if !cmp.is_undefined() {
                        let mut has_error = false;
                        slice.sort_by(|a, b| {
                            if !has_error {
                                let r = unsafe {
                                    JS_Call(
                                        ctx,
                                        cmp,
                                        JSValue::UNDEFINED,
                                        2,
                                        [$cvt(*a), $cvt(*b)].as_mut_ptr(),
                                    )
                                };
                                if let Ok(n) =
                                    r.check_exception().and_then(|v| v.coerce_to_i32_free(ctx))
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
                sort!(i8, |v| JSValue::from_i32(v as i32));
            }
            JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                sort!(u8, |v| JSValue::from_i32(v as i32));
            }
            JS_CLASS_INT16_ARRAY => {
                sort!(i16, |v| JSValue::from_i32(v as i32));
            }
            JS_CLASS_UINT16_ARRAY => {
                sort!(u16, |v| JSValue::from_i32(v as i32));
            }
            JS_CLASS_INT32_ARRAY => {
                sort!(i32, |v| JSValue::from_i32(v as i32));
            }
            JS_CLASS_UINT32_ARRAY => {
                sort!(u32, |v| JSValue::from_u32(v as u32));
            }
            JS_CLASS_FLOAT32_ARRAY => {
                sort!(f32, |v| JSValue::from_f64(v as f64));
            }
            JS_CLASS_FLOAT64_ARRAY => {
                sort!(f64, |v| JSValue::from_f64(v as f64));
            }
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                ctx.JS_ThrowTypeError(c"BigInt64 is not supported");
                return Err(-1);
            }
            _ => unreachable!(),
        }
    }

    Ok(this_val.dup())
}

pub fn js_typed_array_toSorted(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let cx = ctx.handle();
    let mut p = this_val.typed_array_obj(cx, false)?;
    let arr = p.create_new(ctx, JSValue::UNDEFINED, p.class_id())?;

    let ret = js_typed_array_sort(ctx, CallArgs::new(cx, arr, &[], 0, None));
    unsafe {
        JS_FreeValue(ctx, arr);
    }

    ret
}

pub fn js_typed_array_join(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let toLocaleString = args.magic;
    let this_val = args.this_raw();

    let n = unsafe { js_typed_array_get_length_internal(ctx, this_val)? };
    let sep: std::borrow::Cow<'_, str> =
        if toLocaleString == 0 && args.length() > 0 && !args.get_raw(0).is_undefined() {
            args.get_raw(0).coerce_to_utf8(ctx.handle())?
        } else {
            ",".into() // default separator
        };

    let mut txt = StringContent::with_capacity(n as usize * 2, false).map_err(|_| {
        ctx.JS_ThrowOutOfMemory();
        -1
    })?;

    // XXX: optimize with direct access
    for i in 0..n {
        if i > 0 {
            // add separator
            if txt.push_str(sep.as_ref()).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            };
        }

        let el = this_val.get_property_u32(ctx, i).check_exception()?;
        if !el.is_undefined_or_null() {
            let v = if toLocaleString != 0 {
                el.to_locale_string(ctx)
            } else {
                el.coerce_to_string_value(ctx)
            };

            match v {
                Ok(s) => {
                    unsafe {
                        if txt.push(s.string().content()).is_err() {
                            JS_FreeValue(ctx, s);
                            ctx.JS_ThrowOutOfMemory();
                            return Err(-1);
                        }

                        JS_FreeValue(ctx, s);
                    }

                    if txt.capacity() - txt.len() < 16 {
                        if txt.reserve((n - i) as usize * 2).is_err() {
                            ctx.JS_ThrowOutOfMemory();
                            return Err(-1);
                        }
                    }
                }
                Err(e) => {
                    unsafe {
                        JS_FreeValue(ctx, el);
                    }
                    ctx.JS_ThrowTypeError(c"toString failed");
                    return Err(e);
                }
            }
        }
    }

    ctx.create_string_from_content(txt).check_exception()
}

pub fn js_typed_array_indexOf(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let this_val = args.this_raw();
    let special = args.magic;
    let len = unsafe { js_typed_array_get_length_internal(ctx, this_val)? as i32 };

    if len == 0 {
        return Ok(if special == special_includes {
            JSValue::FALSE
        } else {
            JSValue::from(-1)
        });
    }

    let mut k: i32 = 0;
    let stop: i32;
    let inc: i32;
    let mut d = 0.0_f64;

    if special == special_lastIndexOf {
        k = len - 1;
        if args.length() > 1 {
            if unsafe { JS_ToFloat64(ctx, &mut d, args.get_raw(1)) } != 0 {
                return Err(-1);
            }

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
                            JSValue::FALSE
                        } else {
                            JSValue::from(-1)
                        });
                    }
                    k = d as i32;
                }
            }
        }
        stop = -1;
        inc = -1;
    } else {
        k = 0;
        if args.length() > 1 {
            if unsafe { JS_ToInt32Clamp(ctx, &mut k, args.get_raw(1), 0, len, len) } != 0 {
                return Err(-1);
            }
        }
        stop = len;
        inc = 1;
    }

    let p = unsafe { this_val.typed_array_obj_unchecked() };
    let mut res = -1;

    if p.is_detached() {
        // if the array was detached, no need to go further (but no exception is raised)
        // "includes" scans all the properties, so "undefined" can match
        if special == special_includes && args.get_raw(0).is_undefined() && len > 0 {
            res = 0;
        }
        return Ok(if special == special_includes {
            JSValue::from(res >= 0)
        } else {
            JSValue::from(res)
        });
    }

    let mut is_bigint = false;
    let mut is_int = false; // avoid warning
    let mut v64 = 0_i64; // avoid warning

    // check value to find
    let a0 = args.get_raw(0);
    match JS_VALUE_GET_NORM_TAG(a0) {
        JSTag::Int => {
            is_int = true;
            v64 = JS_VALUE_GET_INT(a0) as i64;
            d = v64 as f64;
        }
        JSTag::Float64 => {
            d = JS_VALUE_GET_FLOAT64(a0);
            v64 = d as i64;
            is_int = v64 as f64 == d;
        }
        JSTag::BigInt => {
            ctx.JS_ThrowTypeError(c"BigInt is not supported");
            return Err(-1);
        }
        JSTag::BigDecimal => {
            ctx.JS_ThrowTypeError(c"BigDecimal is not supported");
            return Err(-1);
        }
        _ => {
            return Ok(if special == special_includes {
                JSValue::FALSE
            } else {
                JSValue::from(-1)
            });
        }
    }

    macro_rules! find {
        ($ty:ident, $v:expr) => {
            let found = p.with_data::<$ty, _>(|lst| {
                if inc > 0 {
                    debug_assert!(k >= 0);
                    lst.iter().skip(k as usize).position(|x| *x == $v)
                } else {
                    debug_assert!(k >= 0 && k < lst.len() as i32);
                    lst.iter()
                        .enumerate()
                        .take(k as usize + 1)
                        .rev()
                        .find(|(_, x)| **x == $v)
                        .map(|(i, _)| i)
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
                find!(i8, v);
            }
        }
        JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
            if is_int && (v64 as u8) as i64 == v64 {
                let v: u8 = v64 as u8;
                find!(u8, v);
            }
        }
        JS_CLASS_INT16_ARRAY => {
            if is_int && (v64 as i16) as i64 == v64 {
                let v = v64 as i16;
                find!(i16, v);
            }
        }
        JS_CLASS_UINT16_ARRAY => {
            if is_int && (v64 as u16) as i64 == v64 {
                let v = v64 as u16;
                find!(u16, v);
            }
        }
        JS_CLASS_INT32_ARRAY => {
            if is_int && (v64 as i32) as i64 == v64 {
                let v = v64 as i32;
                find!(i32, v);
            }
        }
        JS_CLASS_UINT32_ARRAY => {
            if is_int && (v64 as u32) as i64 == v64 {
                let v = v64 as u32;
                find!(u32, v);
            }
        }
        JS_CLASS_FLOAT32_ARRAY => {
            if d.is_nan() {
                if special == special_includes {
                    // includes finds NaN
                    let found = p.with_data::<f32, _>(|lst| {
                        if inc > 0 {
                            lst.iter().skip(k as usize).position(|x| x.is_nan())
                        } else {
                            lst.iter()
                                .enumerate()
                                .take(k as usize + 1)
                                .rev()
                                .find(|(_, x)| x.is_nan())
                                .map(|(i, _)| i)
                        }
                    });
                    if let Some(i) = found {
                        res = i as i32;
                    }
                } else {
                    // indexOf returns -1
                    return Ok(JSValue::from(-1));
                }
            } else {
                let f = d as f32;
                if f as f64 == d {
                    find!(f32, f);
                }
            }
        }
        JS_CLASS_FLOAT64_ARRAY => {
            if d.is_nan() {
                if special == special_includes {
                    // includes finds NaN
                    let found = p.with_data::<f64, _>(|lst| {
                        if inc > 0 {
                            lst.iter().skip(k as usize).position(|x| x.is_nan())
                        } else {
                            lst.iter()
                                .enumerate()
                                .take(k as usize + 1)
                                .rev()
                                .find(|(_, x)| x.is_nan())
                                .map(|(i, _)| i)
                        }
                    });
                    if let Some(i) = found {
                        res = i as i32;
                    }
                } else {
                    // indexOf returns -1
                    return Ok(JSValue::from(-1));
                }
            } else {
                find!(f64, d);
            }
        }
        JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
            ctx.JS_ThrowTypeError(c"BigInt64 is not supported");
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

pub const js_typed_array_base_proto_funcs: [JSCFunctionListEntry; 36] = [
    JS_CGETSET_DEF!(c"length", js_typed_array_get_length, NONE),
    JS_CFUNC_DEF!(c"at", 1, js_typed_array_at),
    JS_CFUNC_DEF!(c"with", 2, js_typed_array_with),
    JS_CGETSET_MAGIC_DEF!(c"buffer", js_typed_array_get_buffer, NONE, 0),
    JS_CGETSET_MAGIC_DEF!(c"byteLength", js_typed_array_get_byteLength, NONE, 0),
    JS_CGETSET_MAGIC_DEF!(c"byteOffset", js_typed_array_get_byteOffset, NONE, 0),
    JS_CFUNC_DEF!(c"set", 1, js_typed_array_set),
    JS_CFUNC_MAGIC_DEF!(
        c"values",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16
    ),
    JS_ALIAS_DEF!(c"[Symbol.iterator]", c"values"),
    JS_CFUNC_MAGIC_DEF!(
        c"keys",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"entries",
        0,
        js_create_typed_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16
    ),
    JS_CGETSET_DEF!(
        c"[Symbol.toStringTag]",
        js_typed_array_get_toStringTag,
        NONE
    ),
    JS_CFUNC_NATIVE_DEF!(c"copyWithin", 2, js_typed_array_copyWithin, 0),
    JS_CFUNC_MAGIC_DEF!(
        c"every",
        1,
        js_array_every,
        (special_every | special_TA) as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"some",
        1,
        js_array_every,
        (special_some | special_TA) as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"forEach",
        1,
        js_array_every,
        (special_forEach | special_TA) as i16
    ),
    JS_CFUNC_MAGIC_DEF!(c"map", 1, js_array_every, (special_map | special_TA) as i16),
    JS_CFUNC_MAGIC_DEF!(
        c"filter",
        1,
        js_array_every,
        (special_filter | special_TA) as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"reduce",
        1,
        js_array_reduce,
        (special_reduce | special_TA) as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"reduceRight",
        1,
        js_array_reduce,
        (special_reduceRight | special_TA) as i16
    ),
    JS_CFUNC_NATIVE_DEF!(c"fill", 1, js_typed_array_fill, 0),
    JS_CFUNC_NATIVE_DEF!(c"find", 1, js_typed_array_find, special_find as i16),
    JS_CFUNC_NATIVE_DEF!(
        c"findIndex",
        1,
        js_typed_array_find,
        special_findIndex as i16
    ),
    JS_CFUNC_NATIVE_DEF!(c"findLast", 1, js_typed_array_find, special_findLast as i16),
    JS_CFUNC_NATIVE_DEF!(
        c"findLastIndex",
        1,
        js_typed_array_find,
        special_findLastIndex as i16
    ),
    JS_CFUNC_NATIVE_DEF!(c"reverse", 0, js_typed_array_reverse, 0),
    JS_CFUNC_NATIVE_DEF!(c"toReversed", 0, js_typed_array_toReversed, 0),
    JS_CFUNC_NATIVE_DEF!(c"slice", 2, js_typed_array_slice, 0),
    JS_CFUNC_NATIVE_DEF!(c"subarray", 2, js_typed_array_subarray, 0),
    JS_CFUNC_NATIVE_DEF!(c"sort", 1, js_typed_array_sort, 0),
    JS_CFUNC_NATIVE_DEF!(c"toSorted", 1, js_typed_array_toSorted, 0),
    JS_CFUNC_NATIVE_DEF!(c"join", 1, js_typed_array_join, 0),
    JS_CFUNC_NATIVE_DEF!(c"toLocaleString", 0, js_typed_array_join, 1),
    JS_CFUNC_NATIVE_DEF!(
        c"indexOf",
        1,
        js_typed_array_indexOf,
        special_indexOf as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"lastIndexOf",
        1,
        js_typed_array_indexOf,
        special_lastIndexOf as i16
    ),
    JS_CFUNC_NATIVE_DEF!(
        c"includes",
        1,
        js_typed_array_indexOf,
        special_includes as i16
    ),
];
