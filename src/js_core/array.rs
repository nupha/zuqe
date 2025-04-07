use crate::{
    array::{js_get_fast_array, JS_CopySubArray, JS_IsArray, JS_NewArray, JS_ToArrayLengthFree},
    atom::consts::{
        JS_ATOM_Symbol_isConcatSpreadable, JS_ATOM_Symbol_iterator, JS_ATOM_Symbol_species,
        JS_ATOM_constructor, JS_ATOM_join, JS_ATOM_length, JS_ATOM_set,
    },
    bytecode::{
        arithmatic::{js_strict_eq2, JSStrictEqModeEnum},
        js_for_of_start,
    },
    class::js_create_from_ctor,
    context::JSContext,
    error::{JS_ThrowRangeError, JS_ThrowStackOverflow},
    function::{
        check_function, JSCFunctionListEntry, JS_Call, JS_CallConstructor, JS_CallFree,
        JS_GetFunctionRealm, JS_Invoke,
    },
    iterator::{
        js_array_iterator_next, js_create_array_iterator, JSIteratorKindEnum, JS_IteratorClose,
        JS_IteratorNext,
    },
    runtime::js_check_stack_overflow,
    value::{
        check_exception_free,
        object::{
            js_get_length64, JS_CreateDataPropertyUint32, JS_DefinePropertyValueInt64,
            JS_GetProperty, JS_GetPropertyInt64, JS_GetPropertyUint32, JS_GetPropertyValue,
            JS_IsConstructor, JS_IsFunction, JS_ToObject, JS_TryGetPropertyInt64,
            JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_THROW,
        },
        JSString, JSTag, JSValue, JSValueConst, JSValueRc, JS_FreeValue, JS_IsException,
        JS_IsNumber, JS_IsUndefined, JS_NewBool, JS_ToBoolFree, JS_ToInt32Sat, JS_ToInt64Clamp,
        JS_ToInt64Sat, JS_ToLocaleStringFree, JS_ToString, JS_ToStringFree, StringContent,
        JS_VALUE_GET_OBJ, JS_VALUE_GET_STRING, JS_VALUE_GET_TAG, MAX_SAFE_INTEGER,
    },
    JS_ALIAS_DEF, JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CGETSET_DEF, JS_CLASS_ARRAY,
    JS_ITERATOR_NEXT_DEF, JS_PROP_STRING_DEF,
};

use super::{
    js_get_this,
    object::js_object_toString,
    typed_array::{js_typed_array___speciesCreate, js_typed_array_get_length_internal},
};

const special_find: i16 = 0;
const special_findIndex: i16 = 1;
const special_findLast: i16 = 2;
const special_findLastIndex: i16 = 3;

const special_every: i16 = 0;
const special_some: i16 = 1;
const special_forEach: i16 = 2;
const special_map: i16 = 3;
const special_filter: i16 = 4;
const special_TA: i16 = 8;

pub unsafe fn js_array_constructor(
    ctx: *mut JSContext,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = js_create_from_ctor(ctx, new_target, JS_CLASS_ARRAY);
    if JS_IsException(obj) {
        return obj;
    }

    if argc == 1 && JS_IsNumber(*argv.offset(0)) {
        let len = if let Ok(l) = JS_ToArrayLengthFree(ctx, (*argv.offset(0)).dup(), true) {
            l
        } else {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };

        if obj
            .set_property(
                ctx.as_mut().unwrap(),
                JS_ATOM_length,
                JSValue::from_u32(len),
            )
            .is_err()
        {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    } else {
        for i in 0..argc {
            if obj
                .set_property_u32(
                    ctx.as_mut().unwrap(),
                    i as u32,
                    (*argv.offset(i as isize)).dup(),
                )
                .is_err()
            {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
    }

    obj
}

pub unsafe fn JS_ArraySpeciesCreate(
    ctx: *mut JSContext,
    obj: JSValueConst,
    len_val: JSValueConst,
) -> JSValue {
    let mut realm: *mut JSContext = std::ptr::null_mut();

    let res = if let Ok(b) = JS_IsArray(ctx, obj) {
        if !b {
            return js_array_constructor(ctx, JSValue::UNDEFINED, 1, [len_val].as_mut_ptr());
        }
        b
    } else {
        return JSValue::EXCEPTION;
    };

    let mut ctor = JS_GetProperty(ctx, obj, JS_ATOM_constructor);
    if ctor.is_exception() {
        return ctor;
    }

    if JS_IsConstructor(ctx, ctor) {
        // legacy web compatibility
        let realm = JS_GetFunctionRealm(ctx, ctor);
        if realm.is_null() {
            JS_FreeValue(ctx, ctor);
            return JSValue::EXCEPTION;
        }
        if realm != ctx && ctor.is_same((*realm).array_ctor) {
            JS_FreeValue(ctx, ctor);
            ctor = JSValue::UNDEFINED;
        }
    }

    if ctor.is_object() {
        let species = JS_GetProperty(ctx, ctor, JS_ATOM_Symbol_species);
        JS_FreeValue(ctx, ctor);
        if species.is_exception() {
            return species;
        }
        ctor = species;
        if ctor.is_null() {
            ctor = JSValue::UNDEFINED;
        }
    }

    if ctor.is_undefined() {
        js_array_constructor(ctx, JSValue::UNDEFINED, 1, [len_val].as_mut_ptr())
    } else {
        let ret = JS_CallConstructor(ctx, ctor, 1, [len_val].as_mut_ptr());
        JS_FreeValue(ctx, ctor);
        ret
    }
}

pub unsafe fn JS_isConcatSpreadable(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    if !obj.is_object() {
        return Ok(false);
    }

    let tmp = JS_GetProperty(ctx, obj, JS_ATOM_Symbol_isConcatSpreadable);
    if tmp.is_exception() {
        return Err(-1);
    }

    if !tmp.is_undefined() {
        JS_ToBoolFree(ctx, tmp)
    } else {
        JS_IsArray(ctx, obj)
    }
}

pub unsafe fn js_array_at(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let mut idx: i64 = 0;
    let mut count: u32 = 0;

    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    if JS_ToInt64Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if idx < 0 {
        idx = len + idx;
    }

    let ret: JSValue = if idx < 0 {
        // bad index
        JSValue::UNDEFINED
    } else if let Some(lst) = obj.fast_array_values().and_then(|a| {
        if a.len() == len as usize {
            Some(a)
        } else {
            None
        }
    }) {
        // fast array
        lst[idx as usize].dup()
    } else {
        // slow array
        match JS_TryGetPropertyInt64(ctx, obj, idx) {
            Ok(Some(r)) => r,
            Ok(None) => JSValue::UNDEFINED,
            Err(_) => JSValue::EXCEPTION,
        }
    };

    JS_FreeValue(ctx, obj);
    ret
}

pub unsafe fn js_array_with(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut idx: i64 = 0;
    if JS_ToInt64Sat(ctx, &mut idx, *argv.offset(0)) != 0 {
        // C2RS: goto exception replaced with early return logic
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if idx < 0 {
        idx = len + idx;
    }

    if idx < 0 || idx >= len {
        JS_ThrowRangeError(ctx, c"out of bound");
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let arr = (*ctx).create_array(std::cmp::max(len, 0) as usize);
    if JS_IsException(arr) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_OBJ(arr);
    let dest = (*p).fast_array_values_mut_unchecked();

    if let Some(src) = obj.fast_array_values().and_then(|a| {
        if a.len() == len as usize {
            Some(a)
        } else {
            None
        }
    }) {
        // fast array
        for v in &src[0..(idx as usize)] {
            dest.push(v.dup());
        }

        // the changed element
        dest.push((*argv.offset(1)).dup());

        for v in &src[idx as usize..] {
            dest.push(v.dup());
        }
    } else {
        // slow array
        // 0..idx
        for i in 0..idx {
            match JS_TryGetPropertyInt64(ctx, obj, i) {
                Ok(Some(v)) => {
                    dest.push(v.dup());
                }
                Ok(None) => {
                    dest.push(JSValue::UNDEFINED);
                }
                Err(_) => {
                    // fill rest with undefined
                    for _ in i..len {
                        dest.push(JSValue::UNDEFINED);
                    }
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
        }

        // updated value
        dest.push((*argv.offset(1)).dup());

        // idx..end
        for i in idx..len {
            match JS_TryGetPropertyInt64(ctx, obj, i) {
                Ok(Some(v)) => {
                    dest.push(v.dup());
                }
                Ok(None) => {
                    dest.push(JSValue::UNDEFINED);
                }
                Err(_) => {
                    // fill rest with undefined
                    for _ in i..len {
                        dest.push(JSValue::UNDEFINED);
                    }
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
        }
    }

    // if js_get_fast_array(ctx, obj, &mut arrp, &mut count32) && count32 as i64 == len {
    //     let mut i = 0;
    //     while i < idx {
    //         *pval = (*arrp.offset(i as isize)).dup();
    //         i += 1;
    //         pval = pval.offset(1);
    //     }

    //     *pval = (*argv.offset(1)).dup();

    //     i += 1;
    //     pval = pval.offset(1);

    //     while i < len {
    //         *pval = (*arrp.offset(i as isize)).dup();
    //         i += 1;
    //         pval = pval.offset(1);
    //     }
    // } else {
    //     let mut i = 0;
    //     while i < idx {
    //         match JS_TryGetPropertyInt64(ctx, obj, i) {
    //             Ok(Some(r)) => {
    //                 *pval = r;
    //             }
    //             Ok(None) => {
    //                 *pval = JSValue::UNDEFINED;
    //             }
    //             Err(_) => {
    //                 while i < len {
    //                     *pval = JSValue::UNDEFINED;
    //                     i += 1;
    //                     pval = pval.offset(1);
    //                 }
    //                 JS_FreeValue(ctx, obj);
    //                 return JSValue::EXCEPTION;
    //             }
    //         }
    //     }

    //     *pval = (*argv.offset(1)).dup();
    //     i += 1;
    //     pval = pval.offset(1);
    //     while i < len {
    //         match JS_TryGetPropertyInt64(ctx, obj, i) {
    //             Ok(Some(r)) => {
    //                 *pval = r;
    //             }
    //             Ok(None) => {
    //                 *pval = JSValue::UNDEFINED;
    //             }
    //             Err(_) => {
    //                 while i < len {
    //                     *pval = JSValue::UNDEFINED;
    //                     i += 1;
    //                     pval = pval.offset(1);
    //                 }
    //                 JS_FreeValue(ctx, obj);
    //                 return JSValue::EXCEPTION;
    //             }
    //         }
    //     }
    // }

    if arr
        .set_property(
            ctx.as_mut().unwrap(),
            JS_ATOM_length,
            JSValue::from_i64(len),
        )
        .is_err()
    {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    arr
}

pub unsafe fn js_array_concat(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    if JS_IsException(obj) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let arr = JS_ArraySpeciesCreate(ctx, obj, JSValue::ZERO);
    if JS_IsException(arr) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut n = 0;

    for i in -1..argc {
        let e = if i < 0 { obj } else { *argv.offset(i as isize) };
        let spreadable = if let Ok(b) = JS_isConcatSpreadable(ctx, e) {
            b
        } else {
            JS_FreeValue(ctx, arr);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };

        if spreadable {
            let len = if let Ok(l) = js_get_length64(ctx, e) {
                l
            } else {
                JS_FreeValue(ctx, arr);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            };

            if n + len > MAX_SAFE_INTEGER {
                (*ctx).JS_ThrowTypeError(c"Array loo long");
                // C2RS: goto exception replaced with early return logic
                JS_FreeValue(ctx, arr);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            for k in 0..len {
                match JS_TryGetPropertyInt64(ctx, e, k) {
                    Ok(Some(val)) => {
                        if JS_DefinePropertyValueInt64(
                            ctx,
                            arr,
                            n,
                            val,
                            JS_PROP_C_W_E | JS_PROP_THROW,
                        ) < 0
                        {
                            JS_FreeValue(ctx, arr);
                            JS_FreeValue(ctx, obj);
                            return JSValue::EXCEPTION;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        JS_FreeValue(ctx, arr);
                        JS_FreeValue(ctx, obj);
                        return JSValue::EXCEPTION;
                    }
                };

                n += 1;
            }
        } else {
            if n >= MAX_SAFE_INTEGER {
                (*ctx).JS_ThrowTypeError(c"Array loo long");
                // C2RS: goto exception replaced with early return logic
                JS_FreeValue(ctx, arr);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            if JS_DefinePropertyValueInt64(ctx, arr, n, e.dup(), JS_PROP_C_W_E | JS_PROP_THROW) < 0
            {
                // C2RS: goto exception replaced with early return logic
                JS_FreeValue(ctx, arr);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            n += 1;
        }
    }

    if arr
        .set_property(ctx.as_mut().unwrap(), JS_ATOM_length, JSValue::from_i64(n))
        .is_err()
    {
        JS_FreeValue(ctx, arr);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    JS_FreeValue(ctx, obj);
    arr
}

pub unsafe fn js_array_every(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    special: i32,
) -> JSValue {
    let obj: JSValue;
    let len: i64;

    if special & special_TA as i32 != 0 {
        obj = this_val.dup();
        len = if let Ok(l) = js_typed_array_get_length_internal(ctx, obj) {
            l as i64
        } else {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    } else {
        obj = JS_ToObject(ctx, this_val);
        len = if let Ok(l) = js_get_length64(ctx, obj) {
            l
        } else {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };
    }

    let func = *argv.offset(0);
    let this_arg = if argc > 1 {
        *argv.offset(1)
    } else {
        JSValue::UNDEFINED
    };

    if check_function(ctx, func).is_err() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut ret: JSValue;

    match special as i16 {
        s if s == special_every || s == special_every | special_TA => {
            ret = JSValue::TRUE;
        }
        s if s == special_some || s == special_some | special_TA => {
            ret = JSValue::FALSE;
        }
        special_map => {
            ret = JS_ArraySpeciesCreate(ctx, obj, JSValue::from_i64(len));
            if JS_IsException(ret) {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
        special_filter => {
            ret = JS_ArraySpeciesCreate(ctx, obj, JSValue::ZERO);
            if JS_IsException(ret) {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
        s if s == special_map | special_TA => {
            ret = js_typed_array___speciesCreate(
                ctx,
                JSValue::UNDEFINED,
                2,
                [obj, JSValue::from_i32(len as i32)].as_mut_ptr(),
            );
            if JS_IsException(ret) {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
        s if s == special_filter | special_TA => {
            ret = JS_NewArray(ctx);
            if JS_IsException(ret) {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
        _ => {
            ret = JSValue::UNDEFINED;
        }
    }

    let mut n = 0;
    let mut present: bool;

    for k in 0..len {
        let mut val: JSValueRc = JSValue::UNDEFINED.into_rc((*ctx).rt());

        if special & special_TA as i32 != 0 {
            val = JS_GetPropertyInt64(ctx, obj, k).into_rc((*ctx).rt());
            if val.is_exception() {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            present = true;
        } else {
            match JS_TryGetPropertyInt64(ctx, obj, k) {
                Ok(Some(r)) => {
                    val = r.into_rc((*ctx).rt());
                    present = true;
                }
                Ok(None) => {
                    present = false;
                }
                Err(_) => {
                    JS_FreeValue(ctx, ret);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
        }

        if present {
            let index_val = JSValue::from_i64(k);
            if JS_IsException(index_val) {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            let mut args: [JSValue; 3] = [*val, index_val, obj];
            let res = JS_Call(ctx, func, this_arg, 3, args.as_mut_ptr());
            JS_FreeValue(ctx, index_val);
            if JS_IsException(res) {
                JS_FreeValue(ctx, ret);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            match special as i16 {
                s if s == special_every || s == special_every | special_TA => {
                    if JS_ToBoolFree(ctx, res) == Ok(false) {
                        ret = JSValue::FALSE;
                        break;
                    }
                }
                s if s == special_some || s == special_some | special_TA => {
                    if JS_ToBoolFree(ctx, res).unwrap_or(true) {
                        ret = JSValue::TRUE;
                        break;
                    }
                }
                special_map => {
                    if JS_DefinePropertyValueInt64(ctx, ret, k, res, JS_PROP_C_W_E | JS_PROP_THROW)
                        < 0
                    {
                        JS_FreeValue(ctx, ret);
                        JS_FreeValue(ctx, obj);
                        return JSValue::EXCEPTION;
                    }
                }
                s if s == special_map | special_TA => {
                    if ret
                        .set_property_value(
                            ctx.as_mut().unwrap(),
                            JSValue::from_i32(k as i32),
                            res,
                            JS_PROP_THROW,
                        )
                        .is_err()
                    {
                        JS_FreeValue(ctx, ret);
                        JS_FreeValue(ctx, obj);
                        return JSValue::EXCEPTION;
                    }
                }
                s if s == special_filter || s == special_filter | special_TA => {
                    if JS_ToBoolFree(ctx, res).unwrap_or(true) {
                        if JS_DefinePropertyValueInt64(
                            ctx,
                            ret,
                            n,
                            val.dup(),
                            JS_PROP_C_W_E | JS_PROP_THROW,
                        ) < 0
                        {
                            JS_FreeValue(ctx, ret);
                            JS_FreeValue(ctx, obj);
                            return JSValue::EXCEPTION;
                        }
                        n += 1;
                    }
                }
                _ => {
                    JS_FreeValue(ctx, res);
                }
            }
        }
    }

    if special == (special_filter | special_TA) as i32 {
        let mut args: [JSValue; 2] = [obj, JSValue::from_i32(n as i32)];
        let arr = js_typed_array___speciesCreate(ctx, JSValue::UNDEFINED, 2, args.as_mut_ptr());
        if JS_IsException(arr) {
            JS_FreeValue(ctx, ret);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        args[0] = ret;
        let res = JS_Invoke(ctx, arr, JS_ATOM_set, 1, args.as_mut_ptr());
        if check_exception_free(ctx, res) {
            JS_FreeValue(ctx, ret);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        JS_FreeValue(ctx, ret);
        ret = arr;
    }

    JS_FreeValue(ctx, obj);
    ret
}

const special_reduce: i32 = 0;
const special_reduceRight: i32 = 1;

pub unsafe fn js_array_reduce(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    special: i32,
) -> JSValue {
    let obj: JSValue;
    let len: i64;

    if special & special_TA as i32 != 0 {
        obj = this_val.dup();
        len = if let Ok(l) = js_typed_array_get_length_internal(ctx, obj) {
            l as _
        } else {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };
    } else {
        obj = JS_ToObject(ctx, this_val);
        len = if let Ok(l) = js_get_length64(ctx, obj) {
            l
        } else {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };
    }

    let func = *argv.offset(0);
    if check_function(ctx, func).is_err() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut acc = JSValue::UNDEFINED;
    let mut k = 0;

    if argc > 1 {
        acc = (*argv.offset(1)).dup();
    } else {
        loop {
            if k >= len {
                (*ctx).JS_ThrowTypeError(c"empty array");
                JS_FreeValue(ctx, acc);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            let k1 = if special & special_reduceRight != 0 {
                len - k - 1
            } else {
                k
            };
            k += 1;

            if special & special_TA as i32 != 0 {
                acc = JS_GetPropertyInt64(ctx, obj, k1);
                if JS_IsException(acc) {
                    JS_FreeValue(ctx, acc);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                break;
            } else {
                match JS_TryGetPropertyInt64(ctx, obj, k1) {
                    Ok(Some(r)) => {
                        acc = r;
                        break;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        JS_FreeValue(ctx, acc);
                        JS_FreeValue(ctx, obj);
                        return JSValue::EXCEPTION;
                    }
                }
            }
        }
    }

    let mut val = JSValue::UNDEFINED;
    while k < len {
        let k1 = if special & special_reduceRight != 0 {
            len - k - 1
        } else {
            k
        };
        k += 1;

        let present = if special & special_TA as i32 != 0 {
            val = JS_GetPropertyInt64(ctx, obj, k1);
            if JS_IsException(val) {
                JS_FreeValue(ctx, acc);
                JS_FreeValue(ctx, val);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            true
        } else {
            match JS_TryGetPropertyInt64(ctx, obj, k1) {
                Ok(Some(r)) => {
                    val = r;
                    true
                }
                Ok(None) => false,
                Err(_) => {
                    JS_FreeValue(ctx, acc);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
        };

        if present {
            let index_val = JSValue::from_i64(k1 as _);
            if JS_IsException(index_val) {
                JS_FreeValue(ctx, acc);
                JS_FreeValue(ctx, val);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            let acc1 = JS_Call(
                ctx,
                func,
                JSValue::UNDEFINED,
                4,
                [acc, val, index_val, obj].as_mut_ptr(),
            );
            JS_FreeValue(ctx, index_val);
            JS_FreeValue(ctx, val);
            val = JSValue::UNDEFINED;
            if JS_IsException(acc1) {
                JS_FreeValue(ctx, acc);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            JS_FreeValue(ctx, acc);
            acc = acc1;
        }
    }

    JS_FreeValue(ctx, obj);
    acc
}

pub unsafe fn js_array_fill(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l as i64
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut start = 0_i64;
    if argc > 1 && !JS_IsUndefined(*argv.offset(1)) {
        if JS_ToInt64Clamp(ctx, &mut start, *argv.offset(1), 0, len, len) != 0 {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    let mut end = len;
    if argc > 2 && !JS_IsUndefined(*argv.offset(2)) {
        if JS_ToInt64Clamp(ctx, &mut end, *argv.offset(2), 0, len, len) != 0 {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    // XXX: should special case fast arrays
    while start < end {
        if obj
            .set_property_i64(ctx.as_mut().unwrap(), start, (*argv.offset(0)).dup())
            .is_err()
        {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        start += 1;
    }

    return obj;
}

use std::mem;

pub unsafe fn js_array_find(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    mode: i32,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l as i64
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let func = *argv.offset(0);
    if check_function(ctx, func).is_err() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut this_arg = if argc > 1 {
        *argv.offset(1)
    } else {
        JSValue::UNDEFINED
    };

    let mut k: i64;
    let dir: i64;
    let end: i64;

    if matches!(mode as i16, special_findLast | special_findLastIndex) {
        k = len - 1;
        dir = -1;
        end = -1;
    } else {
        k = 0;
        dir = 1;
        end = len;
    }

    // TODO(bnoordhuis) add fast path for fast arrays
    while k != end {
        let index_val = JSValue::from_i64(k);
        if JS_IsException(index_val) {
            JS_FreeValue(ctx, index_val);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let val = JS_GetPropertyValue(ctx, obj, index_val);
        if JS_IsException(val) {
            JS_FreeValue(ctx, index_val);
            JS_FreeValue(ctx, val);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        let res = JS_Call(
            ctx,
            func,
            this_arg,
            3,
            [val, index_val, this_val].as_mut_ptr(),
        );
        if JS_IsException(res) {
            JS_FreeValue(ctx, index_val);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if JS_ToBoolFree(ctx, res).unwrap_or(true) {
            if matches!(mode as i16, special_findIndex | special_findLastIndex) {
                JS_FreeValue(ctx, val);
                JS_FreeValue(ctx, obj);
                return index_val;
            } else {
                JS_FreeValue(ctx, index_val);
                JS_FreeValue(ctx, obj);
                return val;
            }
        }
        JS_FreeValue(ctx, val);
        JS_FreeValue(ctx, index_val);
        k += dir;
    }
    JS_FreeValue(ctx, obj);

    if matches!(mode as i16, special_findIndex | special_findLastIndex) {
        JSValue::from_i32(-1)
    } else {
        JSValue::UNDEFINED
    }
}

pub unsafe fn js_array_indexOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut res = -1;
    if len > 0 {
        let mut n = 0;
        if argc > 1 {
            if JS_ToInt64Clamp(ctx, &mut n, *argv.offset(1), 0, len, len) != 0 {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }

        let mut arrp = std::ptr::null_mut::<JSValue>();
        let mut count = 0;
        if js_get_fast_array(ctx, obj, &mut arrp, &mut count) {
            let mut idx = n as usize;
            while (idx as i64) < (count as i64) {
                if js_strict_eq2(
                    ctx,
                    (*argv.offset(0)).dup(),
                    (*arrp.offset(idx as isize)).dup(),
                    JSStrictEqModeEnum::JS_EQ_STRICT,
                ) {
                    JS_FreeValue(ctx, obj);
                    return JSValue::from_i64(idx as i64);
                }
                idx += 1;
            }
        }
        while n < len {
            match JS_TryGetPropertyInt64(ctx, obj, n) {
                Ok(Some(v)) => {
                    if js_strict_eq2(
                        ctx,
                        (*argv.offset(0)).dup(),
                        v,
                        JSStrictEqModeEnum::JS_EQ_STRICT,
                    ) {
                        res = n;
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }

            n += 1;
        }
    }

    JS_FreeValue(ctx, obj);
    JSValue::from_i64(res)
}

pub unsafe fn js_array_lastIndexOf(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut res: i64 = -1;

    if len > 0 {
        let cnt = if argc > 1 {
            let mut i = 0;
            if JS_ToInt64Clamp(ctx, &mut i, *argv.offset(1), -1, len - 1, len) != 0 {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            i
        } else {
            len - 1
        };

        // XXX: should special case fast arrays
        for n in (0..=cnt).rev() {
            match JS_TryGetPropertyInt64(ctx, obj, n) {
                Ok(Some(v)) => {
                    if js_strict_eq2(
                        ctx,
                        (*argv.offset(0)).dup(),
                        v,
                        JSStrictEqModeEnum::JS_EQ_STRICT,
                    ) {
                        res = n;
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => {
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
        }
    }

    JS_FreeValue(ctx, obj);
    JSValue::from_i64(res)
}

pub unsafe fn js_array_includes(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut res = false;
    if len > 0 {
        let mut n = if argc > 1 {
            let mut i = 0;
            if JS_ToInt64Clamp(ctx, &mut i, *argv.offset(1), 0, len, len) != 0 {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
            i
        } else {
            0
        };

        let mut arrp = std::ptr::null_mut::<JSValue>();
        let mut count: u32 = 0;

        if js_get_fast_array(ctx, obj, &mut arrp, &mut count) {
            let mut idx = n;
            while idx < count as i64 {
                if js_strict_eq2(
                    ctx,
                    (*argv.offset(0)).dup(),
                    (*arrp.offset(idx as isize)).dup(),
                    JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO,
                ) {
                    res = true;
                    break;
                }
                idx += 1;
            }
        }

        while n < len {
            let val = JS_GetPropertyInt64(ctx, obj, n);
            if JS_IsException(val) {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            if js_strict_eq2(
                ctx,
                (*argv.offset(0)).dup(),
                val,
                JSStrictEqModeEnum::JS_EQ_SAME_VALUE_ZERO,
            ) {
                res = true;
                break;
            }
            n += 1;
        }
    }

    JS_FreeValue(ctx, obj);
    JS_NewBool(res)
}

pub unsafe fn js_array_join(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    toLocaleString: i32,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let n = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut c = ',' as i32; // default separator
    let mut sep = JSValue::UNDEFINED;
    let mut p: *mut JSString = std::ptr::null_mut();

    if toLocaleString == 0 && argc > 0 && !JS_IsUndefined(*argv.offset(0)) {
        sep = JS_ToString(ctx, *argv.offset(0));
        if JS_IsException(sep) {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        p = JS_VALUE_GET_STRING(sep);
        c = if (*p).len() == 1 && !(*p).is_wide_char() {
            (*p).content().as_bytes()[0] as i32
        } else {
            -1
        };
    }

    let mut content = StringContent::Empty;

    for i in 0..n {
        if i > 0 {
            // append separator
            if c >= 0 {
                content.push_u8(c as u8);
            } else {
                debug_assert!(!p.is_null());
                content.push((*p).content());
            }
        }

        let mut el = JS_GetPropertyUint32(ctx, obj, i as u32).into_rc((*ctx).rt());
        if el.is_exception() {
            JS_FreeValue(ctx, sep);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }

        if !el.is_undefined_or_null() {
            if toLocaleString != 0 {
                el = JS_ToLocaleStringFree(ctx, el.into()).into_rc((*ctx).rt());
            }

            if JS_VALUE_GET_TAG(*el) != JSTag::String as i32 {
                el = JS_ToStringFree(ctx, el.into()).into_rc((*ctx).rt());
                if el.is_exception() {
                    JS_FreeValue(ctx, sep);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }

            if content.push(el.string().content()).is_err() {
                JS_FreeValue(ctx, sep);
                JS_FreeValue(ctx, obj);
                (*ctx).JS_ThrowOutOfMemory();
                return JSValue::EXCEPTION;
            }
        }
    }

    JS_FreeValue(ctx, sep);
    JS_FreeValue(ctx, obj);

    (*ctx).create_string_from_content(content)
}

pub unsafe fn js_array_toString(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }

    let method = JS_GetProperty(ctx, obj, JS_ATOM_join);
    let ret = if JS_IsException(method) {
        JSValue::EXCEPTION
    } else if !JS_IsFunction(ctx, method) {
        // Use intrinsic Object.prototype.toString
        JS_FreeValue(ctx, method);
        js_object_toString(ctx, obj, 0, std::ptr::null_mut())
    } else {
        JS_CallFree(ctx, method, obj, 0, std::ptr::null_mut())
    };

    JS_FreeValue(ctx, obj);
    ret
}

pub unsafe fn js_array_pop(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    shift: i32,
) -> JSValue {
    let mut obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut res = JSValue::UNDEFINED;
    let mut new_len = 0;

    if len > 0 {
        new_len = len - 1;

        if let Some(lst) = obj.fast_array_values_mut().and_then(|a| {
            if a.len() == len as usize {
                Some(a)
            } else {
                None
            }
        }) {
            // fast array
            res = if shift != 0 {
                lst.remove(0)
            } else {
                lst.pop().unwrap()
            };
        } else {
            if shift != 0 {
                res = JS_GetPropertyInt64(ctx, obj, 0);
                if res.is_exception() {
                    JS_FreeValue(ctx, res);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                if JS_CopySubArray(ctx, obj, 0, 1, len as usize - 1, 1).is_err() {
                    JS_FreeValue(ctx, res);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            } else {
                res = JS_GetPropertyInt64(ctx, obj, new_len);
                if res.is_exception() {
                    JS_FreeValue(ctx, res);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
            if obj
                .delete_property_i64(ctx.as_mut().unwrap(), new_len, JS_PROP_THROW)
                .is_err()
            {
                JS_FreeValue(ctx, res);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
    }

    if obj
        .set_property(
            ctx.as_mut().unwrap(),
            JS_ATOM_length,
            JSValue::from_i64(new_len),
        )
        .is_err()
    {
        JS_FreeValue(ctx, res);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    JS_FreeValue(ctx, obj);
    res
}

pub unsafe fn js_array_push(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    unshift: i32,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let newLen = len + argc as i64;
    if newLen > MAX_SAFE_INTEGER {
        (*ctx).JS_ThrowTypeError(c"Array loo long");
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut from = len;
    if unshift != 0 && argc > 0 {
        if JS_CopySubArray(ctx, obj, argc as _, 0, len as usize, -1).is_err() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        from = 0;
    }

    for i in 0..argc {
        if obj
            .set_property_i64(
                ctx.as_mut().unwrap(),
                from + i as i64,
                (*argv.offset(i as isize)).dup(),
            )
            .is_err()
        {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    if obj
        .set_property(
            ctx.as_mut().unwrap(),
            JS_ATOM_length,
            JSValue::from_i64(newLen),
        )
        .is_err()
    {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    JS_FreeValue(ctx, obj);
    JSValue::from_i64(newLen)
}

pub unsafe fn js_array_reverse(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };
    if len == 0 {
        JS_FreeValue(ctx, obj);
        return obj;
    }

    let mut arrp: *mut JSValue = std::ptr::null_mut();
    let mut count32: u32 = 0;

    /* Special case fast arrays */
    if js_get_fast_array(ctx, obj, &mut arrp, &mut count32) && count32 == len as u32 {
        let mut ll: u32 = 0;
        let mut hh: u32 = count32 - 1;

        if count32 > 1 {
            while ll < hh {
                let v = *arrp.offset(ll as isize);
                *arrp.offset(ll as isize) = *arrp.offset(hh as isize);
                *arrp.offset(hh as isize) = v;
                ll += 1;
                hh -= 1;
            }
        }
        return obj;
    }

    let mut l = 0;
    let mut h = len - 1;
    let mut lval = JSValue::UNDEFINED;
    let mut hval = JSValue::UNDEFINED;

    while l < h {
        let l_present = match JS_TryGetPropertyInt64(ctx, obj, l) {
            Ok(Some(v)) => {
                lval = v;
                true
            }
            Ok(None) => false,
            Err(_) => {
                JS_FreeValue(ctx, lval);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        };

        let h_present = match JS_TryGetPropertyInt64(ctx, obj, h) {
            Ok(Some(v)) => {
                hval = v;
                true
            }
            Ok(None) => false,
            Err(_) => {
                JS_FreeValue(ctx, lval);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        };

        if h_present {
            if obj
                .set_property_i64(ctx.as_mut().unwrap(), l, hval)
                .is_err()
            {
                JS_FreeValue(ctx, lval);
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }

            if l_present {
                if obj
                    .set_property_i64(ctx.as_mut().unwrap(), h, lval)
                    .is_err()
                {
                    // XXX: why not free lval?
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                lval = JSValue::UNDEFINED;
            } else {
                if obj
                    .delete_property_i64(ctx.as_mut().unwrap(), h, JS_PROP_THROW)
                    .is_err()
                {
                    JS_FreeValue(ctx, lval);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
            }
        } else {
            if l_present {
                if obj
                    .delete_property_i64(ctx.as_mut().unwrap(), l, JS_PROP_THROW)
                    .is_err()
                {
                    JS_FreeValue(ctx, lval);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                if obj
                    .set_property_i64(ctx.as_mut().unwrap(), h, lval)
                    .is_err()
                {
                    // XXX: why not free lval?
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                lval = JSValue::UNDEFINED;
            }
        }
        l += 1;
        h -= 1;
    }

    return obj;
}

pub unsafe fn js_array_toReversed(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let new_len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let res_arr = (*ctx).create_array(std::cmp::max(new_len, 0) as _);
    if JS_IsException(res_arr) {
        JS_FreeValue(ctx, res_arr);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if new_len > 0 {
        let p = JS_VALUE_GET_OBJ(res_arr);
        let dest = (*p).fast_array_values_mut_unchecked();

        if let Some(src) = obj.fast_array_values().and_then(|a| {
            if a.len() == new_len as usize {
                Some(a)
            } else {
                None
            }
        }) {
            // Fast path: fast array values
            for v in src.iter().rev() {
                dest.push(v.dup());
            }
        } else {
            // slow array
            // Query order is observable; test262 expects descending order.
            let mut i = new_len - 1;
            while i >= 0 {
                if let Ok(v) = JS_TryGetPropertyInt64(ctx, obj, i) {
                    if let Some(v) = v {
                        dest.push(v);
                    }
                } else {
                    // Exception; initialize remaining elements.
                    while i >= 0 {
                        dest.push(JSValue::UNDEFINED);
                        i -= 1;
                    }

                    JS_FreeValue(ctx, res_arr);
                    JS_FreeValue(ctx, obj);
                    return JSValue::EXCEPTION;
                }
                i -= 1;
            }
        }

        if res_arr
            .set_property(
                ctx.as_mut().unwrap(),
                JS_ATOM_length,
                JSValue::from_i64(new_len),
            )
            .is_err()
        {
            JS_FreeValue(ctx, res_arr);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    JS_FreeValue(ctx, obj);
    res_arr
}

pub unsafe fn js_array_sort(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    (*ctx).JS_ThrowInternalError(c"sort is not implemented yet");
    JSValue::EXCEPTION
}

pub unsafe fn js_array_toSorted(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    (*ctx).JS_ThrowInternalError(c"toSorted is not implemented yet");
    JSValue::EXCEPTION
}

pub unsafe fn js_array_slice(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    splice: i32,
) -> JSValue {
    // let mut val: JSValue = mem::zeroed();
    // let mut len_val: JSValue = mem::zeroed();
    // let mut del_count: i64 = 0;
    // let mut new_len: i64 = 0;
    // let mut k_present: i32 = 0;
    // let mut count32: u32 = 0;
    // let mut i: u32 = 0;

    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut start = 0_i64;

    if JS_ToInt64Clamp(ctx, &mut start, *argv.offset(0), 0, len, len) != 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut item_count: u32 = 0;
    let mut del_count = 0;
    let mut final_ = 0;
    let mut count = 0;

    if splice != 0 {
        if argc == 0 {
            item_count = 0;
            del_count = 0;
        } else if argc == 1 {
            item_count = 0;
            del_count = len - start;
        } else {
            item_count = (argc - 2) as u32;
            if JS_ToInt64Clamp(ctx, &mut del_count, *argv.offset(1), 0, len - start, 0) != 0 {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
        if len + item_count as i64 - del_count > MAX_SAFE_INTEGER {
            (*ctx).JS_ThrowTypeError(c"Array loo long");
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        count = del_count;
    } else {
        item_count = 0; /* avoid warning */
        final_ = len;
        if !JS_IsUndefined(*argv.offset(1)) {
            if JS_ToInt64Clamp(ctx, &mut final_, *argv.offset(1), 0, len, len) != 0 {
                // C2RS: goto exception replaced with return logic
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
        count = std::cmp::max(final_ - start, 0);
    }

    let len_val = JSValue::from_i64(count);
    let arr = JS_ArraySpeciesCreate(ctx, obj, len_val);
    if JS_IsException(arr) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    final_ = start + count;

    let mut k = start;
    let mut n = 0;

    // The fast array test on arr ensures that
    // JS_CreateDataPropertyUint32() won't modify obj in case arr is
    // an exotic object
    // Special case fast arrays
    let mut arrp: *mut JSValue = std::ptr::null_mut();
    let mut count32 = 0;

    if js_get_fast_array(ctx, obj, &mut arrp, &mut count32) && arr.is_fast_array() {
        // XXX: should share code with fast array constructor
        while k < final_ && k < count32 as i64 {
            if JS_CreateDataPropertyUint32(
                ctx,
                arr,
                n as _,
                (*arrp.offset(k as isize)).dup(),
                JS_PROP_THROW,
            ) < 0
            {
                JS_FreeValue(ctx, obj);
                JS_FreeValue(ctx, arr);
                return JSValue::EXCEPTION;
            }

            k += 1;
            n += 1;
        }
    }

    // Copy the remaining elements if any (handle case of inherited properties)
    while k < final_ {
        match JS_TryGetPropertyInt64(ctx, obj, k) {
            Ok(Some(v)) => {
                if JS_CreateDataPropertyUint32(ctx, arr, n as _, v, JS_PROP_THROW) < 0 {
                    JS_FreeValue(ctx, obj);
                    JS_FreeValue(ctx, arr);
                    return JSValue::EXCEPTION;
                }
            }
            Ok(None) => {}
            Err(_) => {
                JS_FreeValue(ctx, obj);
                JS_FreeValue(ctx, arr);
                return JSValue::EXCEPTION;
            }
        }

        k += 1;
        n += 1;
    }

    if arr
        .set_property(ctx.as_mut().unwrap(), JS_ATOM_length, JSValue::from_i64(n))
        .is_err()
    {
        JS_FreeValue(ctx, obj);
        JS_FreeValue(ctx, arr);
        return JSValue::EXCEPTION;
    }

    if splice != 0 {
        let new_len = len + item_count as i64 - del_count;

        if item_count as i64 != del_count {
            if JS_CopySubArray(
                ctx,
                obj,
                start as usize + item_count as usize,
                start as usize + del_count as usize,
                len as usize - (start + del_count) as usize,
                if item_count as i64 <= del_count {
                    1
                } else {
                    -1
                },
            )
            .is_err()
            {
                JS_FreeValue(ctx, obj);
                JS_FreeValue(ctx, arr);
                return JSValue::EXCEPTION;
            }

            while k > new_len {
                k -= 1;
                if obj
                    .delete_property_i64(ctx.as_mut().unwrap(), k, JS_PROP_THROW)
                    .is_err()
                {
                    JS_FreeValue(ctx, obj);
                    JS_FreeValue(ctx, arr);
                    return JSValue::EXCEPTION;
                }
            }
        }

        for i in 0..item_count {
            if obj
                .set_property_i64(
                    ctx.as_mut().unwrap(),
                    start + i as i64,
                    (*argv.offset((i + 2) as isize)).dup(),
                )
                .is_err()
            {
                JS_FreeValue(ctx, obj);
                JS_FreeValue(ctx, arr);
                return JSValue::EXCEPTION;
            }
        }

        if obj
            .set_property(
                ctx.as_mut().unwrap(),
                JS_ATOM_length,
                JSValue::from_i64(new_len),
            )
            .is_err()
        {
            JS_FreeValue(ctx, obj);
            JS_FreeValue(ctx, arr);
            return JSValue::EXCEPTION;
        }
    }

    JS_FreeValue(ctx, obj);
    return arr;
}

pub unsafe fn js_array_toSpliced(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut start = 0;
    if argc > 0 {
        if JS_ToInt64Clamp(ctx, &mut start, *argv.offset(0), 0, len, len) != 0 {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    let mut del = 0;
    if argc > 0 {
        del = len - start;
    }
    if argc > 1 {
        if JS_ToInt64Clamp(ctx, &mut del, *argv.offset(1), 0, del, 0) != 0 {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    let mut add = 0_i64;
    if argc > 2 {
        add = argc as i64 - 2;
    }

    let new_len = len + add - del;
    if new_len > MAX_SAFE_INTEGER {
        (*ctx).JS_ThrowTypeError(c"invalid array length");
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    // create result array
    let res_arr = (*ctx).create_array(std::cmp::max(new_len, 0) as _);
    if res_arr.is_exception() {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }
    if new_len <= 0 {
        JS_FreeValue(ctx, obj);
        return res_arr;
    }

    let p = JS_VALUE_GET_OBJ(res_arr);
    let new_lst = (*p).fast_array_values_mut_unchecked();

    macro_rules! exception {
        ($i:expr) => {
            // fill rest with undefined, because array finalizer will free elements too.
            for _ in $i..new_len {
                new_lst.push(JSValue::UNDEFINED);
            }
            JS_FreeValue(ctx, res_arr);
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };
    }

    if let Some(src) = obj.fast_array_values().and_then(|a| {
        if a.len() == len as usize {
            Some(a)
        } else {
            None
        }
    }) {
        // src fast array values
        for v in &src[..start as usize] {
            new_lst.push(v.dup());
        }

        for j in 0..add {
            new_lst.push((*argv.offset((2 + j) as isize)).dup());
        }

        for v in &src[(start + del) as usize..] {
            new_lst.push(v.dup());
        }
    } else {
        // slow array
        for i in 0..start {
            if let Ok(val) = JS_TryGetPropertyInt64(ctx, obj, i) {
                if let Some(v) = val {
                    new_lst.push(v);
                }
            } else {
                exception!(i);
            }
        }

        for j in 0..add {
            new_lst.push((*argv.offset((2 + j) as isize)).dup());
        }

        for i in (start + del)..len {
            if let Ok(val) = JS_TryGetPropertyInt64(ctx, obj, i) {
                if let Some(v) = val {
                    new_lst.push(v);
                }
            } else {
                exception!(i);
            }
        }
    }

    if res_arr
        .set_property(
            ctx.as_mut().unwrap(),
            JS_ATOM_length,
            JSValue::from_i64(new_len),
        )
        .is_err()
    {
        exception!(new_len);
    }

    JS_FreeValue(ctx, obj);
    res_arr
}

pub unsafe fn js_array_copyWithin(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut from: i64 = 0;
    let mut to: i64 = 0;
    if JS_ToInt64Clamp(ctx, &mut to, *argv.offset(0), 0, len, len) != 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }
    if JS_ToInt64Clamp(ctx, &mut from, *argv.offset(1), 0, len, len) != 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut final_ = len;
    if argc > 2 && !JS_IsUndefined(*argv.offset(2)) {
        if JS_ToInt64Clamp(ctx, &mut final_, *argv.offset(2), 0, len, len) != 0 {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    let count = std::cmp::min(final_ - from, len - to);

    if JS_CopySubArray(
        ctx,
        obj,
        to as _,
        from as _,
        count as _,
        if from < to && to < from + count {
            -1
        } else {
            1
        },
    )
    .is_err()
    {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    return obj;
}

pub unsafe fn js_array_flatten(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    map: i32,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let sourceLen = if let Ok(l) = js_get_length64(ctx, obj) {
        l
    } else {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let mut depthNum = 1;
    let mut mapperFunction = JSValue::UNDEFINED;
    let mut thisArg = JSValue::UNDEFINED;

    if map != 0 {
        mapperFunction = *argv.offset(0);
        if argc > 1 {
            thisArg = *argv.offset(1);
        }
        if check_function(ctx, mapperFunction).is_err() {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    } else {
        if argc > 0 && !JS_IsUndefined(*argv.offset(0)) {
            if JS_ToInt32Sat(ctx, &mut depthNum, *argv.offset(0)) < 0 {
                JS_FreeValue(ctx, obj);
                return JSValue::EXCEPTION;
            }
        }
    }

    let arr = JS_ArraySpeciesCreate(ctx, obj, JSValue::ZERO);
    if JS_IsException(arr) {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    if JS_FlattenIntoArray(
        ctx,
        arr,
        obj,
        sourceLen,
        0,
        depthNum as _,
        mapperFunction,
        thisArg,
    ) < 0
    {
        JS_FreeValue(ctx, arr);
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    JS_FreeValue(ctx, obj);
    arr
}

unsafe fn JS_FlattenIntoArray(
    ctx: *mut JSContext,
    target: JSValueConst,
    source: JSValueConst,
    sourceLen: i64,
    mut targetIndex: i64,
    depth: i32,
    mapperFunction: JSValueConst,
    thisArg: JSValueConst,
) -> i64 {
    if js_check_stack_overflow((*ctx).rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        return -1;
    }

    let mut sourceIndex: i64 = 0;

    while sourceIndex < sourceLen {
        match JS_TryGetPropertyInt64(ctx, source, sourceIndex) {
            Ok(Some(mut element)) => {
                if !JS_IsUndefined(mapperFunction) {
                    let mut args = [element, JSValue::from_i64(sourceIndex), source];
                    element = JS_Call(ctx, mapperFunction, thisArg, 3, args.as_mut_ptr());
                    JS_FreeValue(ctx, args[0]);
                    JS_FreeValue(ctx, args[1]);
                    if JS_IsException(element) {
                        return -1;
                    }
                }

                if depth > 0 {
                    if let Ok(is_array) = JS_IsArray(ctx, element) {
                        if is_array {
                            let elementLen = if let Ok(l) = js_get_length64(ctx, element) {
                                l
                            } else {
                                JS_FreeValue(ctx, element);
                                return -1;
                            };

                            let new_targetIndex = JS_FlattenIntoArray(
                                ctx,
                                target,
                                element,
                                elementLen,
                                targetIndex,
                                depth - 1,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                            );
                            if new_targetIndex < 0 {
                                JS_FreeValue(ctx, element);
                                return -1;
                            }

                            targetIndex = new_targetIndex;
                            JS_FreeValue(ctx, element);
                            sourceIndex += 1;
                            continue;
                        }
                    } else {
                        JS_FreeValue(ctx, element);
                        return -1;
                    }
                }

                if targetIndex >= MAX_SAFE_INTEGER {
                    (*ctx).JS_ThrowTypeError(c"Array too long");
                    JS_FreeValue(ctx, element);
                    return -1;
                }

                if JS_DefinePropertyValueInt64(
                    ctx,
                    target,
                    targetIndex,
                    element,
                    JS_PROP_C_W_E | JS_PROP_THROW,
                ) < 0
                {
                    return -1;
                }

                targetIndex += 1;
                sourceIndex += 1;
            }

            Ok(None) => {
                sourceIndex += 1;
                continue;
            }
            Err(_) => {
                return -1;
            }
        }
    }

    targetIndex
}

pub const js_array_proto_funcs: [JSCFunctionListEntry; 39] = [
    JS_CFUNC_DEF!(c"at", 1, js_array_at),
    JS_CFUNC_DEF!(c"with", 2, js_array_with),
    JS_CFUNC_DEF!(c"concat", 1, js_array_concat),
    JS_CFUNC_MAGIC_DEF!(c"every", 1, js_array_every, special_every),
    JS_CFUNC_MAGIC_DEF!(c"some", 1, js_array_every, special_some),
    JS_CFUNC_MAGIC_DEF!(c"forEach", 1, js_array_every, special_forEach),
    JS_CFUNC_MAGIC_DEF!(c"map", 1, js_array_every, special_map),
    JS_CFUNC_MAGIC_DEF!(c"filter", 1, js_array_every, special_filter),
    JS_CFUNC_MAGIC_DEF!(c"reduce", 1, js_array_reduce, special_reduce as i16),
    JS_CFUNC_MAGIC_DEF!(
        c"reduceRight",
        1,
        js_array_reduce,
        special_reduceRight as i16
    ),
    JS_CFUNC_DEF!(c"fill", 1, js_array_fill),
    JS_CFUNC_MAGIC_DEF!(c"find", 1, js_array_find, special_find),
    JS_CFUNC_MAGIC_DEF!(c"findIndex", 1, js_array_find, special_findIndex),
    JS_CFUNC_MAGIC_DEF!(c"findLast", 1, js_array_find, special_findLast),
    JS_CFUNC_MAGIC_DEF!(c"findLastIndex", 1, js_array_find, special_findLastIndex),
    JS_CFUNC_DEF!(c"indexOf", 1, js_array_indexOf),
    JS_CFUNC_DEF!(c"lastIndexOf", 1, js_array_lastIndexOf),
    JS_CFUNC_DEF!(c"includes", 1, js_array_includes),
    JS_CFUNC_MAGIC_DEF!(c"join", 1, js_array_join, 0),
    JS_CFUNC_DEF!(c"toString", 0, js_array_toString),
    JS_CFUNC_MAGIC_DEF!(c"toLocaleString", 0, js_array_join, 1),
    JS_CFUNC_MAGIC_DEF!(c"pop", 0, js_array_pop, 0),
    JS_CFUNC_MAGIC_DEF!(c"push", 1, js_array_push, 0),
    JS_CFUNC_MAGIC_DEF!(c"shift", 0, js_array_pop, 1),
    JS_CFUNC_MAGIC_DEF!(c"unshift", 1, js_array_push, 1),
    JS_CFUNC_DEF!(c"reverse", 0, js_array_reverse),
    JS_CFUNC_DEF!(c"toReversed", 0, js_array_toReversed),
    JS_CFUNC_DEF!(c"sort", 1, js_array_sort),
    JS_CFUNC_DEF!(c"toSorted", 1, js_array_toSorted),
    JS_CFUNC_MAGIC_DEF!(c"slice", 2, js_array_slice, 0),
    JS_CFUNC_MAGIC_DEF!(c"splice", 2, js_array_slice, 1),
    JS_CFUNC_DEF!(c"toSpliced", 2, js_array_toSpliced),
    JS_CFUNC_DEF!(c"copyWithin", 2, js_array_copyWithin),
    JS_CFUNC_MAGIC_DEF!(c"flatMap", 1, js_array_flatten, 1),
    JS_CFUNC_MAGIC_DEF!(c"flat", 0, js_array_flatten, 0),
    JS_CFUNC_MAGIC_DEF!(
        c"values",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE as i16
    ),
    JS_ALIAS_DEF!(c"[Symbol.iterator]", c"values"),
    JS_CFUNC_MAGIC_DEF!(
        c"keys",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY as i16
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"entries",
        0,
        js_create_array_iterator,
        JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE as i16
    ),
];

pub const js_array_iterator_proto_funcs: [JSCFunctionListEntry; 2] = [
    JS_ITERATOR_NEXT_DEF!(c"next", 0, js_array_iterator_next, 0),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"Array Iterator",
        JS_PROP_CONFIGURABLE
    ),
];

//
// static array functions
//

pub unsafe fn js_array_from(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    // from(items, mapfn = void 0, this_arg = void 0)

    let items = *argv.offset(0);
    let mut mapping = false;
    let mut mapfn = JSValue::UNDEFINED;
    let mut this_arg = JSValue::UNDEFINED;

    if argc > 1 {
        mapfn = *argv.offset(1);
        if !JS_IsUndefined(mapfn) {
            if check_function(ctx, mapfn).is_err() {
                return JSValue::EXCEPTION;
            }

            mapping = true;
            if argc > 2 {
                this_arg = *argv.offset(2);
            }
        }
    }

    let iter = JS_GetProperty(ctx, items, JS_ATOM_Symbol_iterator);
    if JS_IsException(iter) {
        return JSValue::EXCEPTION;
    }

    let mut stack = [JSValue::UNDEFINED, JSValue::UNDEFINED];
    let r: JSValueRc;
    let mut k: i64 = 0;

    macro_rules! exception_close {
        () => {
            if !JS_IsUndefined(stack[0]) {
                JS_IteratorClose(ctx, stack[0], true);
            }
            exception!();
        };
    }
    macro_rules! exception {
        () => {
            JS_FreeValue(ctx, stack[0]);
            JS_FreeValue(ctx, stack[1]);
            return JSValue::EXCEPTION;
        };
    }

    if !JS_IsUndefined(iter) {
        JS_FreeValue(ctx, iter);
        r = if JS_IsConstructor(ctx, this_val) {
            JS_CallConstructor(ctx, this_val, 0, std::ptr::null_mut())
        } else {
            JS_NewArray(ctx)
        }
        .into_rc((*ctx).rt());

        if r.is_exception() {
            exception!();
        }

        stack[0] = items.dup();

        if js_for_of_start(ctx, &mut stack[1], false) != 0 {
            exception!();
        }

        loop {
            let mut done = false;
            let mut v =
                JS_IteratorNext(ctx, stack[0], stack[1], 0, std::ptr::null_mut(), &mut done);
            if JS_IsException(v) {
                exception_close!();
            }

            if done {
                break;
            }

            if mapping {
                let v2 = JS_Call(
                    ctx,
                    mapfn,
                    this_arg,
                    2,
                    [v, JSValue::from_i32(k as i32)].as_mut_ptr(),
                );
                JS_FreeValue(ctx, v);
                v = v2;
                if JS_IsException(v) {
                    exception_close!();
                }
            }

            if JS_DefinePropertyValueInt64(ctx, *r, k, v, JS_PROP_C_W_E | JS_PROP_THROW) < 0 {
                exception_close!();
            }

            k += 1;
        }
    } else {
        let arrayLike = JS_ToObject(ctx, items).into_rc((*ctx).rt());
        if arrayLike.is_exception() {
            exception!();
        }

        let len = if let Ok(n) = js_get_length64(ctx, *arrayLike) {
            n
        } else {
            exception!();
        };

        let mut args = [JSValue::from_i64(len)];
        r = if JS_IsConstructor(ctx, this_val) {
            JS_CallConstructor(ctx, this_val, 1, args.as_mut_ptr())
        } else {
            js_array_constructor(ctx, JSValue::UNDEFINED, 1, args.as_mut_ptr())
        }
        .into_rc((*ctx).rt());

        JS_FreeValue(ctx, args[0]);
        if r.is_exception() {
            exception!();
        }

        for i in 0..len {
            let mut v = JS_GetPropertyInt64(ctx, *arrayLike, i);
            if JS_IsException(v) {
                exception!();
            }

            if mapping {
                let v2 = JS_Call(
                    ctx,
                    mapfn,
                    this_arg,
                    2,
                    [v, JSValue::from_i32(i as i32)].as_mut_ptr(),
                );
                JS_FreeValue(ctx, v);
                v = v2;
                if v.is_exception() {
                    exception!();
                }
            }

            if JS_DefinePropertyValueInt64(ctx, *r, i, v, JS_PROP_C_W_E | JS_PROP_THROW) < 0 {
                exception!();
            }
        }

        k = len;
    }

    if (*r)
        .set_property(
            ctx.as_mut().unwrap(),
            JS_ATOM_length,
            JSValue::from_u32(k as u32),
        )
        .is_err()
    {
        exception!();
    }

    JS_FreeValue(ctx, stack[0]);
    JS_FreeValue(ctx, stack[1]);

    JSValue::from(r)
}

pub unsafe fn js_array_of(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let obj = if JS_IsConstructor(ctx, this_val) {
        JS_CallConstructor(ctx, this_val, 1, [JSValue::from_i32(argc)].as_mut_ptr())
    } else {
        JS_NewArray(ctx)
    };
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    for i in 0..argc {
        if JS_CreateDataPropertyUint32(
            ctx,
            obj,
            i as _,
            (*argv.offset(i as isize)).dup(),
            JS_PROP_THROW,
        ) < 0
        {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    if obj
        .set_property(
            ctx.as_mut().unwrap(),
            JS_ATOM_length,
            JSValue::from_u32(argc as u32),
        )
        .is_err()
    {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    obj
}

pub unsafe fn js_array_isArray(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    if let Ok(ret) = JS_IsArray(ctx, *argv.offset(0)) {
        JS_NewBool(ret)
    } else {
        JSValue::EXCEPTION
    }
}

pub const js_array_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_DEF!(c"isArray", 1, js_array_isArray),
    JS_CFUNC_DEF!(c"from", 1, js_array_from),
    JS_CFUNC_DEF!(c"of", 0, js_array_of),
    JS_CGETSET_DEF!(c"[Symbol.species]", js_get_this, NONE),
];
