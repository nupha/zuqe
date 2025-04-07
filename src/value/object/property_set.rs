use std::ffi::CStr;

use crate::{
    array::{add_fast_array_element, set_array_length},
    atom::{
        JSAtom, JS_AtomIsNumericIndex, __JS_AtomToUInt32, consts::*, JS_FreeAtom, JS_ValueToAtom,
        JS_ATOM_NULL,
    },
    context::JSContext,
    error::JS_ThrowReferenceErrorNotDefined,
    function::call_setter,
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY,
    JS_CLASS_INT8_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT8_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        set_value, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_GetPrototypePrimitive,
        JS_ToFloat64Free, JS_ToNumberFree, JS_ToUint8ClampFree, JS_VALUE_GET_INT,
        JS_VALUE_GET_NORM_TAG, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    obj::JSObject,
    property::{
        find_own_property, JSProperty, JSPropertyDescriptor, JS_AutoInitProperty,
        JS_DefineProperty, JS_GetOwnPropertyInternal, JS_PROP_AUTOINIT, JS_PROP_C_W_E,
        JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_VALUE,
        JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_TMASK,
        JS_PROP_VARREF, JS_PROP_WRITABLE,
    },
};

impl JSValue {
    /// Note: `val` is consumed; `self` and `this_obj` keep ref count unchanged.
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    /// If this_obj != self, then self must be an object (Reflect.set case).
    pub(crate) unsafe fn set_property_internal(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValueConst,
        flags: u32,
    ) -> Result<bool, i32> {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum State {
            /// Set property value for fast path: when self==this_obj
            FastPath,
            /// Try set or define property value
            TrySetOrDefine,
            /// Try set property value using setter
            TrySetter,
            /// Property not found on object and ancestors
            GeneralSet,
            /// Create new property on object
            CreateProp,
            /// Read-only error
            ErrReadOnly,
            /// TypedArray out of bounds error
            ErrTypedArrayOob,
        }

        let p: *mut JSObject;
        let mut p1: *mut JSObject;
        let this_tag = JS_VALUE_GET_NORM_TAG(this_obj);

        let mut state = if this_tag != JSTag::Object {
            if JS_VALUE_GET_TAG(self) == JSTag::Object as i32 {
                p = std::ptr::null_mut();
                p1 = JS_VALUE_GET_OBJ(self);
                if p1.is_null() {
                    State::GeneralSet
                } else {
                    State::TrySetter
                }
            } else {
                match this_tag {
                    JSTag::Null => {
                        JS_FreeValue(ctx, val);
                        ctx.JS_ThrowTypeError(CStr::from_ptr(
                            format!("cannot set property '{}' of null\0", prop.to_str(ctx.rt()))
                                .as_ptr()
                                .cast(),
                        ));
                        return Err(-1);
                    }
                    JSTag::Undefined => {
                        JS_FreeValue(ctx, val);
                        ctx.JS_ThrowTypeError(CStr::from_ptr(
                            format!(
                                "cannot set property '{}' of undefined\0",
                                prop.to_str(ctx.rt())
                            )
                            .as_ptr()
                            .cast(),
                        ));
                        return Err(-1);
                    }
                    _ => {
                        // even on a primitive type we can have setters on the prototype
                        p = std::ptr::null_mut();
                        p1 = JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, self));
                        if p1.is_null() {
                            State::GeneralSet
                        } else {
                            State::TrySetter
                        }
                    }
                }
            }
        } else {
            p = JS_VALUE_GET_OBJ(this_obj);
            p1 = JS_VALUE_GET_OBJ(self);
            debug_assert!(!p1.is_null());
            debug_assert!(!p.is_null());
            if p != p1 {
                State::TrySetter
            } else {
                State::FastPath
            }
        };

        loop {
            match state {
                State::FastPath => {
                    // when self == this_obj
                    let mut pr: *mut JSProperty = std::ptr::null_mut();
                    let prs = find_own_property(&mut pr, p1, prop);
                    if !prs.is_null() {
                        match (*prs).flags() {
                            f if f & (JS_PROP_TMASK | JS_PROP_WRITABLE | JS_PROP_LENGTH)
                                == JS_PROP_WRITABLE =>
                            {
                                // fast case
                                set_value(ctx, &mut (*pr).u.value, val);
                                return Ok(true);
                            }
                            f if f & JS_PROP_LENGTH != 0 => {
                                debug_assert!((*p).class_id() == JS_CLASS_ARRAY);
                                debug_assert_eq!(prop, JS_ATOM_length);
                                return match set_array_length(ctx, p, val, flags) {
                                    0 => Ok(false),
                                    1 => Ok(true),
                                    -1 => Err(-1),
                                    _ => unreachable!(),
                                };
                            }
                            f if f & JS_PROP_TMASK == JS_PROP_GETSET => {
                                return match call_setter(
                                    ctx,
                                    (*pr).u.getset.setter,
                                    this_obj,
                                    val,
                                    flags,
                                ) {
                                    0 => Ok(false),
                                    1 => Ok(true),
                                    -1 => Err(-1),
                                    _ => unreachable!(),
                                };
                            }
                            f if f & JS_PROP_TMASK == JS_PROP_VARREF => {
                                /* JS_PROP_WRITABLE is always true for variable
                                references, but they are write protected in module name
                                spaces. */
                                if (*p).class_id() == JS_CLASS_MODULE_NS {
                                    state = State::ErrReadOnly;
                                    continue;
                                }
                                set_value(ctx, (*(*pr).u.var_ref).pvalue, val);
                                return Ok(true);
                            }
                            f if f & JS_PROP_TMASK == JS_PROP_AUTOINIT => {
                                if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                                    JS_FreeValue(ctx, val);
                                    return Err(-1);
                                }
                                continue; // State::Retry
                            }
                            _ => {
                                state = State::ErrReadOnly;
                                continue;
                            }
                        }
                    } else {
                        state = State::TrySetOrDefine;
                        continue;
                    }
                }
                State::TrySetter => {
                    let mut pr: *mut JSProperty = std::ptr::null_mut();
                    let prs = find_own_property(&mut pr, p1, prop);
                    if !prs.is_null() {
                        if (*prs).flags() & JS_PROP_TMASK == JS_PROP_GETSET {
                            return match call_setter(
                                ctx,
                                (*pr).u.getset.setter,
                                this_obj,
                                val,
                                flags,
                            ) {
                                0 => Ok(false),
                                1 => Ok(true),
                                -1 => Err(-1),
                                _ => unreachable!(),
                            };
                        } else if (*prs).flags() & JS_PROP_TMASK == JS_PROP_AUTOINIT {
                            /* Instantiate property and retry (potentially useless) */
                            if JS_AutoInitProperty(ctx, p1, prop, pr, prs) != 0 {
                                return Err(-1);
                            }
                            continue; // State::Retry2
                        } else if (*prs).flags() & JS_PROP_WRITABLE == 0 {
                            state = State::ErrReadOnly;
                            continue;
                        }
                    } else {
                        state = State::GeneralSet;
                    }
                }
                State::TrySetOrDefine => {
                    if (*p1).detailed.is_exotic() {
                        if (*p1).detailed.fast_array() {
                            if prop.is_int_type() {
                                let idx = __JS_AtomToUInt32(prop);
                                if idx < (*p1).array_length_unchecked() as u32 {
                                    if p == p1 {
                                        return this_obj.set_property_value(
                                            ctx,
                                            JSValue::from_i32(idx as _),
                                            val,
                                            flags,
                                        );
                                    } else {
                                        state = State::GeneralSet;
                                        continue;
                                    }
                                } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                                    .contains(&(*p1).class_id())
                                {
                                    state = State::ErrTypedArrayOob;
                                    continue;
                                }
                            } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                                .contains(&(*p1).class_id())
                            {
                                let ret = JS_AtomIsNumericIndex(ctx, prop);
                                if ret != 0 {
                                    if ret < 0 {
                                        JS_FreeValue(ctx, val);
                                        return Err(-1);
                                    }
                                    state = State::ErrTypedArrayOob;
                                    continue;
                                }
                            }
                        } else {
                            let em = (*ctx).rt().class_array[(*p1).class_id() as usize].exotic;
                            if !em.is_null() {
                                if let Some(cb) = (*em).set_property {
                                    /* set_property can free the prototype */
                                    let obj1 = JS_MKPTR!(JSTag::Object, p1.cast()).dup();
                                    let ret = cb(ctx, obj1, prop, val, this_obj, flags as _);
                                    JS_FreeValue(ctx, obj1);
                                    JS_FreeValue(ctx, val);
                                    return match ret {
                                        0 => Ok(false),
                                        1 => Ok(true),
                                        -1 => Err(-1),
                                        _ => unreachable!(),
                                    };
                                }
                                if let Some(cb) = (*em).get_own_property {
                                    /* get_own_property can free the prototype */
                                    let obj1 = JS_MKPTR!(JSTag::Object, p1.cast()).dup();
                                    let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                                    let mut ret = cb(ctx, &mut desc, obj1, prop);
                                    JS_FreeValue(ctx, obj1);
                                    if ret < 0 {
                                        JS_FreeValue(ctx, val);
                                        return Err(-1);
                                    }
                                    if ret != 0 {
                                        if desc.flags & JS_PROP_GETSET != 0 {
                                            let setter: *mut JSObject =
                                                if desc.setter.is_undefined() {
                                                    std::ptr::null_mut()
                                                } else {
                                                    JS_VALUE_GET_OBJ(desc.setter)
                                                };
                                            ret = call_setter(ctx, setter, this_obj, val, flags);
                                            JS_FreeValue(ctx, desc.getter);
                                            JS_FreeValue(ctx, desc.setter);
                                            return match ret {
                                                0 => Ok(false),
                                                1 => Ok(true),
                                                -1 => Err(-1),
                                                _ => unreachable!(),
                                            };
                                        } else {
                                            JS_FreeValue(ctx, desc.value);
                                            if desc.flags & JS_PROP_WRITABLE == 0 {
                                                state = State::ErrReadOnly;
                                                continue;
                                            }

                                            if p == p1 {
                                                let ret = JS_DefineProperty(
                                                    ctx,
                                                    this_obj,
                                                    prop,
                                                    val,
                                                    JSValue::UNDEFINED,
                                                    JSValue::UNDEFINED,
                                                    JS_PROP_HAS_VALUE,
                                                );
                                                JS_FreeValue(ctx, val);
                                                return match ret {
                                                    0 => Ok(false),
                                                    1 => Ok(true),
                                                    -1 => Err(-1),
                                                    _ => unreachable!(),
                                                };
                                            } else {
                                                state = State::GeneralSet;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // upward for protortype
                    p1 = (*p1).shape().unwrap().proto;

                    state = if p1.is_null() {
                        State::GeneralSet
                    } else {
                        State::TrySetter
                    };
                    continue;
                }

                State::GeneralSet => {
                    if flags & JS_PROP_NO_ADD != 0 {
                        JS_FreeValue(ctx, val);
                        JS_ThrowReferenceErrorNotDefined(ctx, prop);
                        return Err(-1);
                    }

                    if p.is_null() {
                        JS_FreeValue(ctx, val);
                        return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, c"not an object") {
                            Ok(false)
                        } else {
                            Err(-1)
                        };
                    }

                    if !(*p).detailed.extensible() {
                        JS_FreeValue(ctx, val);
                        return if ctx
                            .JS_ThrowTypeErrorOrFalse(flags as _, c"object is not extensible")
                        {
                            Ok(false)
                        } else {
                            Err(-1)
                        };
                    }

                    if p == JS_VALUE_GET_OBJ(self) {
                        if (*p).detailed.is_exotic() {
                            if (*p).class_id() == JS_CLASS_ARRAY
                                && (*p).detailed.fast_array()
                                && prop.is_int_type()
                            {
                                let idx = __JS_AtomToUInt32(prop);
                                if idx == (*p).array_length_unchecked() as u32 {
                                    return add_fast_array_element(ctx, p, val, flags);
                                } else {
                                    state = State::CreateProp;
                                    continue;
                                }
                            } else {
                                state = State::CreateProp;
                                continue;
                            }
                        } else {
                            match (*p).add_property(ctx, prop, JS_PROP_C_W_E) {
                                Ok(pr) => {
                                    (*pr).u.value = val;
                                    return Ok(true);
                                }
                                Err(_) => {
                                    JS_FreeValue(ctx, val);
                                    return Err(-1);
                                }
                            }
                        }
                    } else {
                        // generic case: modify the property in this_obj if it already exists
                        let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                        let ret = JS_GetOwnPropertyInternal(ctx, &mut desc, p, prop);
                        if ret < 0 {
                            JS_FreeValue(ctx, val);
                            return Err(-1);
                        }
                        if ret != 0 {
                            if desc.flags & JS_PROP_GETSET != 0 {
                                JS_FreeValue(ctx, desc.getter);
                                JS_FreeValue(ctx, desc.setter);
                                JS_FreeValue(ctx, val);
                                return if ctx
                                    .JS_ThrowTypeErrorOrFalse(flags as _, c"setter is forbidden")
                                {
                                    Ok(false)
                                } else {
                                    Err(-1)
                                };
                            } else {
                                JS_FreeValue(ctx, desc.value);
                                if desc.flags & JS_PROP_WRITABLE == 0
                                    || (*p).class_id() == JS_CLASS_MODULE_NS
                                {
                                    state = State::ErrReadOnly;
                                    continue;
                                }
                            }
                            let ret = JS_DefineProperty(
                                ctx,
                                this_obj,
                                prop,
                                val,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                JS_PROP_HAS_VALUE,
                            );
                            JS_FreeValue(ctx, val);
                            return match ret {
                                0 => Ok(false),
                                1 => Ok(true),
                                -1 => Err(-1),
                                _ => unreachable!(),
                            };
                        } else {
                            state = State::CreateProp;
                            continue;
                        }
                    }
                }
                State::CreateProp => {
                    let ret = (*p).create_property(
                        ctx,
                        prop,
                        val,
                        JSValue::UNDEFINED,
                        JSValue::UNDEFINED,
                        flags
                            | JS_PROP_HAS_VALUE
                            | JS_PROP_HAS_ENUMERABLE
                            | JS_PROP_HAS_WRITABLE
                            | JS_PROP_HAS_CONFIGURABLE
                            | JS_PROP_C_W_E,
                    );
                    JS_FreeValue(ctx, val);
                    return ret;
                }

                State::ErrReadOnly => {
                    JS_FreeValue(ctx, val);
                    return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                        Ok(false)
                    } else {
                        Err(-1)
                    };
                }
                State::ErrTypedArrayOob => {
                    // must convert the argument even if out of bound access
                    if matches!(
                        (*p1).class_id(),
                        JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY
                    ) {
                        unimplemented!()
                        // let mut v:i64;
                        // if JS_ToBigInt64Free(ctx, &v, val)!=0{
                        //     return -1;
                        // }
                    } else {
                        let val = JS_ToNumberFree(ctx, val);
                        JS_FreeValue(ctx, val);
                        if val.is_exception() {
                            return Err(-1);
                        }
                    }
                    return Ok(true);
                }
            }
        }
    }

    /// Note: `val` is consumed; `self` and `this_obj` keep ref count unchanged.
    #[inline(always)]
    pub unsafe fn set_property(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
    ) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, self, JS_PROP_THROW)
    }

    /// Note: `val` is consumed
    pub fn set_property_value(
        self,
        ctx: &mut JSContext,
        prop: JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if JS_VALUE_GET_TAG(self) == JSTag::Object as i32
            && JS_VALUE_GET_TAG(prop) == JSTag::Int as i32
        {
            let p = JS_VALUE_GET_OBJ(self);
            let idx = JS_VALUE_GET_INT(prop);
            let mut ta_out_of_bound = false;

            unsafe {
                // early return using fast path if possible
                match (*p).class_id() {
                    JS_CLASS_ARRAY => {
                        // fast path: is fast array
                        if (*p).detailed.fast_array() && (*p).detailed.extensible() {
                            let lst = (*p).fast_array_values_mut_unchecked();
                            if idx < lst.len() as i32 {
                                // update array element
                                set_value(ctx, lst.get_mut(idx as usize).unwrap(), val);
                                return Ok(true);
                            } else if idx == lst.len() as i32 {
                                // 检查原型链是否有数字属性
                                let mut has_int_prop = false;
                                let mut p1 = (*p).shape().unwrap().proto;
                                while !p1.is_null() && !has_int_prop {
                                    let sh1 = (*p1).shape().unwrap();
                                    if (*p1).class_id() == JS_CLASS_ARRAY {
                                        if !(*p1).detailed.fast_array() {
                                            has_int_prop = true;
                                        }
                                    } else if (*p1).class_id() == JS_CLASS_OBJECT {
                                        if sh1.has_small_array_index {
                                            has_int_prop = true;
                                        }
                                    } else {
                                        has_int_prop = true;
                                    }
                                    p1 = sh1.proto;
                                }

                                if !has_int_prop {
                                    // append array element
                                    return add_fast_array_element(ctx, p, val, flags);
                                }
                            }
                        }
                    }
                    JS_CLASS_ARGUMENTS => {
                        let lst = (*p).fast_array_values_mut_unchecked();
                        if idx < lst.len() as i32 {
                            set_value(ctx, lst.get_mut(idx as usize).unwrap(), val);
                            return Ok(true);
                        }
                    }
                    JS_CLASS_UINT8C_ARRAY => {
                        let mut v = 0;
                        if JS_ToUint8ClampFree(ctx, &mut v, val) != 0 {
                            return Err(-1);
                        }
                        if idx >= (*p).array_length_unchecked() as i32 {
                            ta_out_of_bound = true;
                        } else {
                            *(*p).u.array.data.uint8_ptr.offset(idx as _) = v as u8;
                            return Ok(true);
                        }
                    }
                    JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY => {
                        let v = if let Ok(n) = val.to_i32_free(ctx) {
                            n as u8
                        } else {
                            return Err(-1);
                        };
                        if idx >= (*p).array_length_unchecked() as i32 {
                            ta_out_of_bound = true;
                        } else {
                            *(*p).u.array.data.uint8_ptr.offset(idx as _) = v;
                            return Ok(true);
                        }
                    }
                    JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY => {
                        let v = if let Ok(n) = val.to_i32_free(ctx) {
                            n as u16
                        } else {
                            return Err(-1);
                        };
                        if idx >= (*p).array_length_unchecked() as i32 {
                            ta_out_of_bound = true;
                        } else {
                            *(*p).u.array.data.uint16_ptr.offset(idx as _) = v as u16;
                            return Ok(true);
                        }
                    }
                    JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY => {
                        let v = if let Ok(n) = val.to_i32_free(ctx) {
                            n as u32
                        } else {
                            return Err(-1);
                        };
                        if idx >= (*p).array_length_unchecked() as i32 {
                            ta_out_of_bound = true;
                        } else {
                            *(*p).u.array.data.uint32_ptr.offset(idx as _) = v as u32;
                            return Ok(true);
                        }
                    }
                    JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                        unimplemented!()
                    }
                    JS_CLASS_FLOAT32_ARRAY => {
                        let mut d = 0.0;
                        if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
                            return Err(-1);
                        }
                        if idx >= (*p).array_length_unchecked() as i32 {
                            ta_out_of_bound = true;
                        } else {
                            *(*p).u.array.data.float_ptr.offset(idx as _) = d as f32;
                            return Ok(true);
                        }
                    }
                    JS_CLASS_FLOAT64_ARRAY => {
                        let mut d = 0.0;
                        if JS_ToFloat64Free(ctx, &mut d, val) != 0 {
                            return Err(-1);
                        }
                        if idx >= (*p).array_length_unchecked() as i32 {
                            ta_out_of_bound = true;
                        } else {
                            *(*p).u.array.data.double_ptr.offset(idx as _) = d;
                            return Ok(true);
                        }
                    }
                    _ => {
                        unreachable!()
                    }
                }

                if ta_out_of_bound {
                    return Ok(true);
                }
            }
        }

        unsafe {
            // slow_path
            let atom = JS_ValueToAtom(ctx, prop);
            JS_FreeValue(ctx, prop);
            if atom == JS_ATOM_NULL {
                JS_FreeValue(ctx, val);
                return Err(-1);
            }

            let ret = self.set_property_internal(ctx, atom, val, self, flags);
            JS_FreeAtom(ctx, atom);
            ret
        }
    }

    #[inline(always)]
    pub fn set_property_u32(
        self,
        ctx: &mut JSContext,
        idx: u32,
        val: JSValue,
    ) -> Result<bool, i32> {
        self.set_property_value(ctx, JSValue::from_u32(idx), val, JS_PROP_THROW)
    }

    pub fn set_property_i64(
        self,
        ctx: &mut JSContext,
        idx: i64,
        val: JSValue,
    ) -> Result<bool, i32> {
        if (idx as u64) <= i32::MAX as u64 {
            // fast path for fast arrays
            return self.set_property_value(ctx, JSValue::from_i32(idx as i32), val, JS_PROP_THROW);
        }

        let prop = ctx.rt_mut().add_atom_i64(idx);
        if prop == JS_ATOM_NULL {
            unsafe {
                JS_FreeValue(ctx, val);
            }
            return Err(-1);
        }

        unsafe {
            let res = self.set_property(ctx, prop, val);
            JS_FreeAtom(ctx, prop);
            res
        }
    }

    pub fn set_property_str(
        self,
        ctx: &mut JSContext,
        prop: &str,
        val: JSValue,
    ) -> Result<bool, i32> {
        unsafe {
            let atom = ctx.create_atom(prop)?;
            let ret = self.set_property_internal(ctx, atom, val, self, JS_PROP_THROW);
            JS_FreeAtom(ctx, atom);
            ret
        }
    }
}

/// Note: `val` is consumed
#[deprecated(note = "use JSValue::set_property_internal() instead")]
pub unsafe fn JS_SetPropertyInternal(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
    this_obj: JSValueConst,
    flags: u32,
) -> i32 {
    obj.set_property_internal(ctx.as_mut().unwrap(), prop, val, this_obj, flags)
        .map_or(-1, |b| b as i32)
}

/// returns 0,1,-1
/// Note: `val` is consumed
#[deprecated(note = "use JSValue::set_property_value() instead")]
pub unsafe fn JS_SetPropertyValue(
    ctx: &mut JSContext,
    this_obj: JSValue,
    prop: JSValue,
    val: JSValue,
    flags: u32,
) -> i32 {
    this_obj
        .set_property_value(ctx, prop, val, flags)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property() instead")]
#[inline(always)]
pub unsafe fn JS_SetProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property(ctx.as_mut().unwrap(), prop, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_u32() instead")]
pub unsafe fn JS_SetPropertyUint32(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: u32,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_u32(ctx.as_mut().unwrap(), idx, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_i64() instead")]
pub unsafe fn JS_SetPropertyInt64(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    idx: i64,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_i64(ctx.as_mut().unwrap(), idx, val)
        .map_or(-1, |b| b as i32)
}

#[deprecated(note = "use JSValue::set_property_str() instead")]
pub unsafe fn JS_SetPropertyStr(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: &CStr,
    val: JSValue,
) -> i32 {
    this_obj
        .set_property_str(ctx.as_mut().unwrap(), prop.to_string_lossy().as_ref(), val)
        .map_or(-1, |b| b as i32)

    // let atom = JS_NewAtom(ctx, prop);
    // let ret = JS_SetPropertyInternal(ctx, this_obj, atom, val, this_obj, JS_PROP_THROW);
    // JS_FreeAtom(ctx, atom);
    // ret
}
