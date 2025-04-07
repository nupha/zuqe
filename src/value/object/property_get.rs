use std::ffi::CStr;

use crate::{
    atom::{
        JSAtom, JS_AtomIsNumericIndex, JS_FreeAtom, JS_NewAtomInt64, JS_ValueToAtom,
        __JS_AtomToUInt32, consts::*, JS_ATOM_MAX_INT, JS_ATOM_NULL,
    },
    context::JSContext,
    error::{JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized},
    function::JS_CallFree,
    types::{likely, unlikely},
    value::{
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_GetPrototypePrimitive,
        JS_IsException, JS_IsUninitialized, JS_TAG_EXCEPTION, JS_TAG_NULL, JS_TAG_OBJECT,
        JS_TAG_STRING, JS_TAG_UNDEFINED, JS_VALUE_GET_INT, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY,
    JS_CLASS_INT8_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY,
    JS_CLASS_UINT8_ARRAY, JS_MKPTR,
};

use super::{
    find_own_property, JSProperty, JSPropertyDescriptor, JS_AutoInitProperty, JS_HasProperty,
    JS_PROP_AUTOINIT, JS_PROP_GETSET, JS_PROP_TMASK, JS_PROP_VARREF,
};

impl JSValue {
    /// returned value has its ref_count increased by 1.
    pub(crate) unsafe fn get_property_internal(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        this_obj: JSValueConst,
        throw_ref_error: bool,
    ) -> JSValue {
        let mut p = match JS_VALUE_GET_TAG(self) {
            JS_TAG_OBJECT => JS_VALUE_GET_OBJ(self),
            JS_TAG_NULL => {
                return ctx.JS_ThrowTypeError(CStr::from_ptr(
                    format!("cannot read property '{}' of null\0", prop.to_str(ctx.rt()))
                        .as_ptr()
                        .cast(),
                ));
            }
            JS_TAG_UNDEFINED => {
                return ctx.JS_ThrowTypeError(CStr::from_ptr(
                    format!(
                        "cannot read property '{}' of undefined\0",
                        prop.to_str(ctx.rt())
                    )
                    .as_ptr()
                    .cast(),
                ));
            }
            JS_TAG_EXCEPTION => {
                return JSValue::EXCEPTION;
            }
            JS_TAG_STRING => {
                let ss = self.string();
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < ss.len() as u32 {
                        let ch = ss.content().chars().nth(idx as usize).unwrap();
                        return ctx.create_string_from_char(ch);
                    }
                } else if prop == JS_ATOM_length {
                    return JSValue::from_i32(ss.len() as i32);
                }

                // cannot raise an exception
                JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, self))
            }
            _ => {
                // cannot raise an exception
                JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, self))
            }
        };

        if p.is_null() {
            return JSValue::UNDEFINED;
        }

        loop {
            let mut pr: *mut JSProperty = std::ptr::null_mut();
            let prs = find_own_property(&mut pr, p, prop);
            if !prs.is_null() {
                match (*prs).flags() & JS_PROP_TMASK {
                    JS_PROP_GETSET => {
                        return if (*pr).u.getset.getter.is_null() {
                            JSValue::UNDEFINED
                        } else {
                            let func: JSValue =
                                JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast());
                            // Note: the field could be removed in the getter
                            JS_CallFree(ctx, func.dup(), this_obj, 0, std::ptr::null_mut())
                        };
                    }
                    JS_PROP_VARREF => {
                        let val = *(*pr).u.var_ref.as_ref().unwrap().pvalue;
                        return if JS_IsUninitialized(val) {
                            JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom)
                        } else {
                            val.dup()
                        };
                    }
                    JS_PROP_AUTOINIT => {
                        // Instantiate property and retry
                        if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                            return JSValue::EXCEPTION;
                        } else {
                            continue;
                        }
                    }
                    0 => {
                        return (*pr).u.value.dup();
                    }
                    _ => unreachable!(),
                }
            }

            if (*p).detailed.is_exotic() {
                // exotic behaviors
                if (*p).detailed.fast_array() {
                    if prop.is_int_type() {
                        let idx = __JS_AtomToUInt32(prop);
                        if idx < (*p).array_length_unchecked() as u32 {
                            return JS_GetPropertyUint32(
                                ctx,
                                JS_MKPTR!(JSTag::Object, p.cast()),
                                idx,
                            );
                        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&(*p).class_id())
                        {
                            return JSValue::UNDEFINED;
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&(*p).class_id())
                    {
                        let ret = JS_AtomIsNumericIndex(ctx, prop);
                        if ret != 0 {
                            return if ret < 0 {
                                JSValue::EXCEPTION
                            } else {
                                JSValue::UNDEFINED
                            };
                        }
                    }
                } else {
                    let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
                    if !em.is_null() {
                        if let Some(cb) = (*em).get_property {
                            /* XXX: should pass throw_ref_error */
                            /* Note: if 'p' is a prototype, it can be
                            freed in the called function */
                            let obj1 = JS_MKPTR!(JSTag::Object, p.cast()).dup();
                            let retval = cb(ctx, obj1, prop, this_obj);
                            JS_FreeValue(ctx, obj1);
                            return retval;
                        }

                        if let Some(cb) = (*em).get_own_property {
                            let mut desc: JSPropertyDescriptor = std::mem::zeroed();

                            // Note: if 'p' is a prototype, it can be freed in the called function
                            let obj1 = JS_MKPTR!(JSTag::Object, p.cast()).dup();
                            let ret = cb(ctx, &mut desc, obj1, prop);
                            JS_FreeValue(ctx, obj1);
                            if ret < 0 {
                                return JSValue::EXCEPTION;
                            }
                            if ret != 0 {
                                if desc.flags & JS_PROP_GETSET != 0 {
                                    JS_FreeValue(ctx, desc.setter);
                                    return JS_CallFree(
                                        ctx,
                                        desc.getter,
                                        this_obj,
                                        0,
                                        std::ptr::null_mut(),
                                    );
                                } else {
                                    return desc.value;
                                }
                            }
                        }
                    }
                }
            }

            p = (*(*p).shape).proto;
            if p.is_null() {
                break;
            }
        }

        if throw_ref_error {
            JS_ThrowReferenceErrorNotDefined(ctx, prop)
        } else {
            JSValue::UNDEFINED
        }
    }

    #[inline(always)]
    pub fn get_property(self, ctx: &mut JSContext, prop: JSAtom) -> JSValue {
        unsafe { self.get_property_internal(ctx, prop, self, false) }
    }
}

/// returned value has its ref_count increased by 1.
#[deprecated(note = "use JSValue::get_property_internal() instead")]
pub unsafe fn JS_GetPropertyInternal(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    this_obj: JSValueConst,
    throw_ref_error: bool,
) -> JSValue {
    obj.get_property_internal(ctx.as_mut().unwrap(), prop, this_obj, throw_ref_error)
}

#[deprecated(note = "use JSValue::get_property() instead")]
#[inline(always)]
pub unsafe fn JS_GetProperty(ctx: *mut JSContext, this_obj: JSValueConst, prop: JSAtom) -> JSValue {
    JS_GetPropertyInternal(ctx, this_obj, prop, this_obj, false)
}

/// NOTE: returned property value has increased ref_count by 1
pub unsafe fn JS_GetPropertyValue(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSValue,
) -> JSValue {
    let atom;
    let ret;

    if JS_VALUE_GET_TAG(this_obj) == JSTag::Object as i32
        && JS_VALUE_GET_TAG(prop) == JSTag::Int as i32
    {
        let idx = JS_VALUE_GET_INT(prop);
        let p = JS_VALUE_GET_OBJ(this_obj);

        // return early for fast array and index within slice bound.
        match (*p).class_id() {
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                if let Some(lst) = (*p).fast_array_values() {
                    if idx < lst.len() as i32 {
                        return lst[idx as usize].dup(); // use fast array value
                    }
                }
            }
            JS_CLASS_INT8_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_i32(
                        *(*p).u.array.data.int8_ptr.offset(idx as isize) as i32
                    );
                }
            }
            JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_i32(
                        *(*p).u.array.data.uint8_ptr.offset(idx as isize) as i32
                    );
                }
            }
            JS_CLASS_INT16_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_i32(
                        *(*p).u.array.data.int16_ptr.offset(idx as isize) as i32
                    );
                }
            }
            JS_CLASS_UINT16_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_i32(
                        *(*p).u.array.data.uint16_ptr.offset(idx as isize) as i32
                    );
                }
            }
            JS_CLASS_INT32_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_i32(*(*p).u.array.data.int32_ptr.offset(idx as isize));
                }
            }
            JS_CLASS_UINT32_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_u32(*(*p).u.array.data.uint32_ptr.offset(idx as isize));
                }
            }
            JS_CLASS_BIG_INT64_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    //return JS_NewBigInt64(ctx, *(*p).u.array.u.int64_ptr.offset(idx as isize));
                    unimplemented!()
                }
            }
            JS_CLASS_BIG_UINT64_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    //return JS_NewBigUint64(ctx, *(*p).u.array.u.uint64_ptr.offset(idx as isize));
                    unimplemented!()
                }
            }
            JS_CLASS_FLOAT32_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_f64_as_is(
                        *(*p).u.array.data.float_ptr.offset(idx as isize) as f64,
                    );
                }
            }
            JS_CLASS_FLOAT64_ARRAY => {
                if idx < (*p).array_length_unchecked() as i32 {
                    return JSValue::from_f64_as_is(
                        *(*p).u.array.data.double_ptr.offset(idx as isize),
                    );
                }
            }
            _ => {}
        }
    }

    // slow path:
    atom = JS_ValueToAtom(ctx, prop);
    JS_FreeValue(ctx, prop);
    if unlikely(atom == JS_ATOM_NULL) {
        return JSValue::EXCEPTION;
    }
    ret = JS_GetProperty(ctx, this_obj, atom);
    JS_FreeAtom(ctx, atom);
    ret
}

pub unsafe fn JS_GetPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
) -> JSValue {
    JS_GetPropertyValue(ctx, this_obj, JSValue::from_u32(idx))
}

pub unsafe fn JS_GetPropertyInt64(ctx: *mut JSContext, obj: JSValueConst, idx: i64) -> JSValue {
    if (idx as u64) <= i32::MAX as u64 {
        // fast path for fast arrays
        return JS_GetPropertyValue(ctx, obj, JSValue::from_i32(idx as i32));
    }
    let prop = JS_NewAtomInt64(ctx, idx);
    if prop == JS_ATOM_NULL {
        return JSValue::EXCEPTION;
    }

    let val = JS_GetProperty(ctx, obj, prop);
    JS_FreeAtom(ctx, prop);
    val
}

/// Check if an object has a generalized numeric property `idx`. Returns:
/// Err for exception,
/// Ok(Some(_)) if property exists,
/// Ok(None) if proprty does not exist.
pub unsafe fn JS_TryGetPropertyInt64(
    ctx: *mut JSContext,
    obj: JSValueConst,
    idx: i64,
) -> Result<Option<JSValue>, i32> {
    if likely((idx as u64) <= JS_ATOM_MAX_INT as u64) {
        // fast path
        if JS_HasProperty(ctx, obj, JSAtom::from_u32(idx as u32))? {
            let val = JS_GetPropertyValue(ctx, obj, JSValue::from_i32(idx as i32));
            if JS_IsException(val) {
                Err(-1)
            } else {
                Ok(Some(val))
            }
        } else {
            Ok(None)
        }
    } else {
        let prop = JS_NewAtomInt64(ctx, idx);
        let ret = if likely(prop != JS_ATOM_NULL) {
            JS_HasProperty(ctx, obj, prop).and_then(|b| {
                if b {
                    let val = JS_GetPropertyValue(ctx, obj, JSValue::from_i32(idx as i32));
                    if unlikely(JS_IsException(val)) {
                        Err(-1)
                    } else {
                        Ok(Some(val))
                    }
                } else {
                    Ok(None) // not exists
                }
            })
        } else {
            Err(-1) // can't create prop atom
        };
        JS_FreeAtom(ctx, prop);
        ret
    }
}
