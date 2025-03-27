use crate::{
    atom::{
        consts::JS_ATOM_length, JSAtom, JS_AtomIsArrayIndex, JS_FreeAtom, JS_NewAtomUInt32,
        JS_ATOM_NULL,
    },
    context::JSContext,
    error::{JSErrorEnum, JS_ThrowRangeError, JS_ThrowTypeErrorReadOnly},
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::{js_free, js_free_rt, js_realloc2},
    proxy::js_proxy_isArray,
    runtime::JSRuntime,
    types::{likely, unlikely},
    value::{
        object::{
            js_dup_shape, js_get_length64, JSObject, JSProperty, JSPropertyUnion,
            JS_CreateDataPropertyUint32, JS_DeletePropertyInt64, JS_NewObjectFromShape,
            JS_SetProperty, JS_SetPropertyInt64, JS_ToObject, JS_TryGetPropertyInt64,
            JS_PROP_CONFIGURABLE, JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE,
        },
        set_value, JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT,
        JS_IsException, JS_ToNumberFree, JS_ToUint32, JS_TAG_INT, JS_TAG_OBJECT,
        JS_VALUE_GET_FLOAT64, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG, MAX_SAFE_INTEGER,
    },
    JS_CLASS_ARRAY, JS_CLASS_PROXY,
};

impl JSValue {
    /// test if value is fast array
    pub fn is_fast_array(self) -> bool {
        if JS_VALUE_GET_TAG(self) == JS_TAG_OBJECT {
            let p = JS_VALUE_GET_OBJ(self);
            unsafe {
                if (*p).class_id() == JS_CLASS_ARRAY && (*p).detailed.fast_array() {
                    return true;
                }
            }
        }

        false
    }
}

impl JSContext {
    /// capacity <=0 for unknown length, equivalent to JS_NewArray in this case.
    /// returns EXCEPTION value if create fail, otherwise returns Array value
    pub unsafe fn create_array_with_capacity(&mut self, capacity: i64) -> JSValue {
        if capacity > i32::MAX as i64 {
            return self.JS_ThrowError(JSErrorEnum::Range, c"invalid array length");
        }

        let arr = JS_NewArray(self);
        if JS_IsException(arr) {
            return arr;
        }

        if capacity > 0 {
            let p = JS_VALUE_GET_OBJ(arr);
            if expand_fast_array(self, p, capacity as u32) < 0 {
                JS_FreeValue(self, arr);
                return JSValue::EXCEPTION;
            }
            (*p).u.array.count = capacity as u32;
        }

        arr
    }
}

pub unsafe fn JS_NewArray(ctx: *mut JSContext) -> JSValue {
    JS_NewObjectFromShape(ctx, js_dup_shape((*ctx).array_shape), JS_CLASS_ARRAY)
}

// return Err if exception (proxy case) or TRUE/FALSE
pub unsafe fn JS_IsArray(ctx: *mut JSContext, val: JSValueConst) -> Result<bool, i32> {
    if JS_VALUE_GET_TAG(val) == JS_TAG_OBJECT {
        let p = JS_VALUE_GET_OBJ(val);
        if unlikely((*p).class_id() == JS_CLASS_PROXY) {
            js_proxy_isArray(ctx, val)
        } else {
            Ok((*p).class_id() == JS_CLASS_ARRAY)
        }
    } else {
        Ok(false)
    }
}

/* set the array length and remove the array elements if necessary. */
pub unsafe fn set_array_length(
    ctx: *mut JSContext,
    p: *mut JSObject,
    val: JSValue,
    flags: u32,
) -> i32 {
    /* Note: this call can reallocate the properties of 'p' */
    let len = if let Ok(n) = JS_ToArrayLengthFree(ctx, val, false) {
        n
    } else {
        return -1;
    };

    /* JS_ToArrayLengthFree() must be done before the read-only test */
    if (*p)
        .shape()
        .and_then(|sh| sh.props.get_index(0))
        .map(|(_, p)| p.flags())
        .unwrap()
        & JS_PROP_WRITABLE
        == 0
    {
        return JS_ThrowTypeErrorReadOnly(ctx, flags, JS_ATOM_length);
    }

    if likely((*p).detailed.fast_array()) {
        let old_len = (*p).u.array.count;
        if len < old_len {
            // C2RS: C for-loop converted to Rust range
            for i in len..old_len {
                JS_FreeValue(ctx, *(*p).u.array.u.values.offset(i as isize));
            }
            (*p).u.array.count = len;
        }

        (*p).prop_values[0] = JSProperty {
            u: JSPropertyUnion {
                value: JSValue::from_u32(len),
            },
        };
    } else {
        /* Note: length is always a uint32 because the object is an array */
        let mut cur_len = (*p)
            .prop_values
            .first()
            .map(|pr| pr.u.value)
            .ok_or(-1)
            .and_then(|v| JS_ToUint32(ctx, v))
            .unwrap();

        if len < cur_len {
            let mut sh = (*p).shape_mut().unwrap();
            let d = cur_len - len;

            if d <= (*sh).props.len() as u32 {
                /* faster to iterate */
                while cur_len > len {
                    let atom = JS_NewAtomUInt32(ctx, cur_len - 1);
                    let ret = p.as_mut().unwrap().delete_property(ctx, atom);
                    JS_FreeAtom(ctx, atom);
                    if unlikely(ret == 0) {
                        /* unlikely case: property is not configurable */
                        break;
                    }
                    cur_len -= 1;
                }
            } else {
                /* faster to iterate thru all the properties. Need two
                passes in case one of the property is not
                configurable */
                cur_len = len;

                for pr in sh.props.values_mut() {
                    let mut idx = 0_u32;
                    if pr.atom != JS_ATOM_NULL && JS_AtomIsArrayIndex(ctx, &mut idx, pr.atom) {
                        if idx >= cur_len && pr.flags() & JS_PROP_CONFIGURABLE == 0 {
                            cur_len = idx + 1;
                        }
                    }
                }

                for i in 0..(*sh).props.len() {
                    let (_, pr) = (*sh).props.get_index(i).unwrap();
                    let mut idx = 0_u32;
                    if pr.atom != JS_ATOM_NULL && JS_AtomIsArrayIndex(ctx, &mut idx, (*pr).atom) {
                        if idx >= cur_len {
                            // remove the property
                            p.as_mut().unwrap().delete_property(ctx, pr.atom);
                            // WARNING: the shape may have been modified
                            sh = (*p).shape_mut().unwrap();
                        }
                    }
                }
            }
        } else {
            cur_len = len;
        }

        set_value(
            ctx,
            &mut (*p).prop_values.first_mut().unwrap().u.value, // #0 is the length
            JSValue::from_u32(cur_len),
        );

        if cur_len > len {
            return ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeErrorOrFalse(flags as _, c"not configurable");
        }
    }

    1
}

/* can be called on Array or Arguments objects. return < 0 if
memory alloc error. */
#[must_use]
pub unsafe fn convert_fast_array_to_array(ctx: *mut JSContext, p: *mut JSObject) -> i32 {
    if p.as_mut()
        .unwrap()
        .js_shape_prepare_update(ctx, std::ptr::null_mut())
        != 0
    {
        return -1;
    }

    let len = (*p).u.array.count;
    // /* resize the properties once to simplify the error handling */
    // let sh: *mut JSShape = (*p).shape;
    // let new_count = (*sh).prop_count as usize + len as usize;
    // if new_count > (*sh).prop_size as usize {
    //     if resize_properties(ctx.as_mut().unwrap(), &mut (*p).shape, p, new_count as u32) != 0 {
    //         return -1;
    //     }
    // }

    let tab = (*p).u.array.u.values; // list of array values
    for i in 0..len {
        /* add_property cannot fail here but
        __JS_AtomFromUInt32(i) fails for i > INT32_MAX */
        let pr = p
            .as_mut()
            .unwrap()
            .add_property(
                ctx.as_mut().unwrap(),
                JSAtom::from_u32(i), // int as property name
                JS_PROP_C_W_E,
            )
            .expect("FIXME: add_property error");

        (*pr).u.value = *tab.offset(i as _);
    }

    // free array list mem
    js_free(ctx, (*p).u.array.u.values.cast());

    (*p).u.array.count = 0;
    (*p).u.array.u.values = std::ptr::null_mut(); /* fail safe */
    (*p).u.array.u1.size = 0;
    (*p).detailed.set_fast_array(false);

    0
}

// Preconditions: 'p' must be of class JS_CLASS_ARRAY, p->fast_array =
// TRUE and p->extensible = TRUE
pub unsafe fn add_fast_array_element(
    ctx: *mut JSContext,
    p: *mut JSObject,
    val: JSValue,
    flags: u32,
) -> i32 {
    // extend the array by one
    // XXX: convert to slow array if new_len > 2^31-1 elements
    let new_len = (*p).u.array.count + 1;

    // update the length if necessary. We assume that if the length is
    // not an integer, then if it >= 2^31.
    if likely(JS_VALUE_GET_TAG((*p).prop_values.first().unwrap().u.value) == JS_TAG_INT) {
        let array_len = JS_VALUE_GET_INT((*p).prop_values.first().unwrap().u.value) as u32;
        if new_len > array_len {
            if (*p)
                .shape()
                .and_then(|sh| sh.props.get_index(0))
                .map(|(_, p)| p.flags())
                .unwrap()
                & JS_PROP_WRITABLE
                == 0
            {
                JS_FreeValue(ctx, val);
                return JS_ThrowTypeErrorReadOnly(ctx, flags, JS_ATOM_length);
            }
            (*p).prop_values[0].u.value = JSValue::from_i32(new_len as i32);
        }
    }
    if unlikely(new_len > (*p).u.array.u1.size) {
        if expand_fast_array(ctx, p, new_len) != 0 {
            JS_FreeValue(ctx, val);
            return -1;
        }
    }
    (*p).u
        .array
        .u
        .values
        .offset((new_len - 1) as isize)
        .write(val);
    (*p).u.array.count = new_len;

    return 1;
}

/* return -1 if exception */
pub unsafe fn expand_fast_array(ctx: *mut JSContext, p: *mut JSObject, new_len: u32) -> i32 {
    let mut slack: usize = 0;

    /* XXX: potential arithmetic overflow */
    let mut new_size = std::cmp::max(new_len, (*p).u.array.u1.size * 3 / 2);
    let new_array_prop = js_realloc2(
        ctx,
        (*p).u.array.u.values.cast(),
        std::mem::size_of::<JSValue>() * new_size as usize,
        &mut slack,
    )
    .cast::<JSValue>();
    if new_array_prop.is_null() {
        return -1;
    }

    new_size += (slack / std::mem::size_of::<JSValue>()) as u32;

    (*p).u.array.u.values = new_array_prop;
    (*p).u.array.u1.size = new_size;

    return 0;
}

pub unsafe fn JS_ToArrayLengthFree(
    ctx: *mut JSContext,
    val: JSValue,
    is_array_ctor: bool,
) -> Result<u32, i32> {
    let len: u32;

    match JS_VALUE_GET_NORM_TAG(val) {
        JSTag::Int | JSTag::Bool | JSTag::Null => {
            let v = JS_VALUE_GET_INT(val);
            if v < 0 {
                // C2RS: goto fail replaced with direct return
                JS_ThrowRangeError(ctx, c"invalid array length");
                return Err(-1);
            }
            len = v as u32;
        }

        JSTag::Float64 => {
            let d = JS_VALUE_GET_FLOAT64(val);
            len = d as u32;
            if len as f64 != d {
                // f64 can't convert to u32
                JS_ThrowRangeError(ctx, c"invalid array length");
                return Err(-1);
            }
        }

        JSTag::BigInt => {
            // let p = JS_VALUE_GET_PTR(val) as *mut JSBigFloat;
            // let mut a = std::mem::MaybeUninit::<bf_t>::uninit();
            // let res;
            // // C2RS: pointer cast for len
            // bf_get_int32(
            //     &mut len as *mut u32 as *mut i32,
            //     &(*p).num,
            //     BF_GET_INT_MOD as i32,
            // );
            // bf_init((*ctx).bf_ctx, a.as_mut_ptr() as *mut _);
            // bf_set_ui(a.as_mut_ptr(), len as u64);
            // res = bf_cmp_eq(a.as_ptr(), &(*p).num);
            // bf_delete(a.as_mut_ptr());
            // JS_FreeValue(ctx, val);
            // if res == 0 {
            //     // C2RS: goto fail replaced with direct return
            //     JS_ThrowRangeError(ctx, b"invalid array length\0".as_ptr() as *const _);
            //     return -1;
            // }
            unimplemented!()
        }
        _ if is_array_ctor => {
            let v = JS_ToNumberFree(ctx, val);
            if JS_IsException(v) {
                return Err(-1);
            }

            // won't recurse because v is a number
            len = JS_ToArrayLengthFree(ctx, v, true)?;
        }
        _ => {
            // legacy behavior: must do the conversion twice and compare
            match JS_ToUint32(ctx, val) {
                Ok(n) => {
                    len = n;
                }
                Err(_) => {
                    JS_FreeValue(ctx, val);
                    return Err(-1);
                }
            }

            let v = JS_ToNumberFree(ctx, val);
            if JS_IsException(v) {
                return Err(-1);
            }
            if len != JS_ToArrayLengthFree(ctx, v, false)? {
                // C2RS: goto fail replaced with direct return
                JS_ThrowRangeError(ctx, c"invalid array length");
                return Err(-1);
            }
        }
    }

    Ok(len)
}

pub unsafe fn JS_CopySubArray(
    ctx: *mut JSContext,
    obj: JSValueConst,
    to_pos: i64,
    from_pos: i64,
    count: i64,
    dir: i32,
) -> Result<(), i32> {
    let p = if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let p0 = JS_VALUE_GET_OBJ(obj);
        if (*p0).class_id() != JS_CLASS_ARRAY || !(*p0).detailed.fast_array() {
            std::ptr::null_mut()
        } else {
            p0
        }
    } else {
        std::ptr::null_mut()
    };

    let mut from: i64;
    let mut to: i64;
    let mut len: i64;
    let mut i = 0;

    while i < count {
        if dir < 0 {
            from = from_pos + count - i - 1;
            to = to_pos + count - i - 1;
        } else {
            from = from_pos + i;
            to = to_pos + i;
        }
        len = (*p).u.array.count as i64;

        if !p.is_null()
            && (*p).detailed.fast_array()
            && from >= 0
            && from < len
            && to >= 0
            && to < len
        {
            // Fast path for fast arrays. Since we don't look at the
            // prototype chain, we can optimize only the cases where
            // all the elements are present in the array.

            let mut j: i64 = 0;
            let mut l = count - i;

            if dir < 0 {
                l = std::cmp::min(l, from + 1);
                l = std::cmp::min(l, to + 1);
                j = 0;
                while j < l {
                    set_value(
                        ctx,
                        (*p).u.array.u.values.offset((to - j) as isize),
                        JS_DupValue(ctx, *(*p).u.array.u.values.offset((from - j) as isize)),
                    );
                    j += 1;
                }
            } else {
                l = std::cmp::min(l, len - from);
                l = std::cmp::min(l, len - to);
                j = 0;
                while j < l {
                    set_value(
                        ctx,
                        (*p).u.array.u.values.offset((to + j) as isize),
                        JS_DupValue(ctx, *(*p).u.array.u.values.offset((from + j) as isize)),
                    );
                    j += 1;
                }
            }
            i += l;
        } else {
            match JS_TryGetPropertyInt64(ctx, obj, from)? {
                Some(val) => {
                    if JS_SetPropertyInt64(ctx, obj, to, val) < 0 {
                        return Err(-1);
                    }
                }
                None => {
                    JS_DeletePropertyInt64(ctx, obj, to, JS_PROP_THROW)?;
                }
            }
            i += 1;
        }
    }

    Ok(())
}

pub unsafe fn js_create_array(ctx: *mut JSContext, len: i32, tab: *const JSValueConst) -> JSValue {
    let obj = JS_NewArray(ctx);
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }

    for i in 0..len {
        if JS_CreateDataPropertyUint32(
            ctx,
            obj,
            i as _,
            JS_DupValue(ctx, *tab.offset(i as isize)),
            0,
        ) < 0
        {
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    obj
}

pub unsafe fn js_array_push(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    unshift: i32,
) -> JSValue {
    let obj = JS_ToObject(ctx, this_val);
    let len = if let Ok(n) = js_get_length64(ctx, obj) {
        n
    } else {
        // C2RS: goto exception replaced with early return logic
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    };

    let newLen = len + argc as i64;
    if newLen > MAX_SAFE_INTEGER {
        ctx.as_mut().unwrap().JS_ThrowTypeError(c"Array loo long");
        // C2RS: goto exception replaced with early return logic
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    let mut from = len;
    if unshift != 0 && argc > 0 {
        if JS_CopySubArray(ctx, obj, argc as _, 0, len, -1).is_err() {
            // C2RS: goto exception replaced with early return logic
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
        from = 0;
    }

    for i in 0..argc {
        if JS_SetPropertyInt64(
            ctx,
            obj,
            from + i as i64,
            JS_DupValue(ctx, *argv.offset(i as isize)),
        ) < 0
        {
            // C2RS: goto exception replaced with early return logic
            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        }
    }

    if JS_SetProperty(ctx, obj, JS_ATOM_length, JSValue::from_i64(newLen)) < 0 {
        // C2RS: goto exception replaced with early return logic
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }

    JS_FreeValue(ctx, obj);
    JSValue::from_i64(newLen)
}

/// Access an Array's internal JSValue array if available
pub unsafe fn js_get_fast_array(
    ctx: *mut JSContext,
    obj: JSValueConst,
    arrpp: *mut *mut JSValue,
    countp: *mut u32,
) -> bool {
    // Try and handle fast arrays explicitly
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        let p: *mut JSObject = JS_VALUE_GET_OBJ(obj);
        if (*p).class_id() == JS_CLASS_ARRAY && (*p).detailed.fast_array() {
            *countp = (*p).u.array.count;
            *arrpp = (*p).u.array.u.values;
            return true;
        }
    }
    return false;
}

#[deprecated(note = "use JSContext::create_array_with_capacity() instead")]
pub unsafe fn js_allocate_fast_array(ctx: *mut JSContext, len: i64) -> JSValue {
    (*ctx).create_array_with_capacity(len)
}

#[deprecated(note = "use JSValue::is_fast_array() instead")]
pub unsafe fn js_is_fast_array(ctx: *const JSContext, obj: JSValueConst) -> bool {
    obj.is_fast_array()
}

pub unsafe fn js_array_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = JS_VALUE_GET_OBJ(val);
    for i in 0..(*p).u.array.count {
        JS_FreeValueRT(rt, *(*p).u.array.u.values.add(i as usize));
    }
    js_free_rt(rt, (*p).u.array.u.values.cast());
}

pub unsafe fn js_array_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p: *mut JSObject = JS_VALUE_GET_OBJ(val);
    for i in 0..(*p).u.array.count {
        JS_MarkValue(rt, *(*p).u.array.u.values.offset(i as isize), mark_func);
    }
}
