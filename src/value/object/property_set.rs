use std::ffi::CStr;

use crate::{
    array::add_fast_array_element,
    atom::{
        JSAtom, JS_AtomIsNumericIndex, __JS_AtomToUInt32, consts::*, JS_FreeAtom, JS_ValueToAtom,
        JS_ATOM_NULL,
    },
    context::JSContext,
    error::JS_ThrowReferenceErrorNotDefined,
    function::{call_setter, JSVarRef},
    typed_array::{
        Float32Array, Float64Array, Int16Array, Int32Array, Int8Array, Uint16Array, Uint32Array,
        Uint8Array,
    },
    utils::UncheckedInto,
    value::{JS_TAG_INT, JS_TAG_OBJECT},
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY,
    JS_CLASS_INT8_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT8_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        set_value, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_GetPrototypePrimitive,
        JS_ToFloat64Free, JS_ToUint8ClampFree, JS_VALUE_GET_INT, JS_VALUE_GET_NORM_TAG,
        JS_VALUE_GET_OBJ,
    },
    obj::JSObject,
    JSPropertyDescriptor, JS_AutoInitProperty, JS_GetOwnPropertyInternal, JS_PROP_AUTOINIT,
    JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_LENGTH, JS_PROP_NO_ADD, JS_PROP_THROW,
    JS_PROP_TMASK, JS_PROP_VARREF, JS_PROP_WRITABLE,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetPropState {
    /// Try set or define property value
    ExoticAndGoUp,
    /// Property shape
    SetterOrAutoInit,
    /// Property not found on object and ancestors
    Add,
    /// Create new property on object
    CreateProp,
    /// Read-only error
    ErrReadOnly,
    /// TypedArray out of bounds error
    ErrTypedArrayOOB,
}

impl JSObject {
    #[inline(always)]
    unsafe fn set_property_fast(
        &mut self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValue,
        flags: u32,
    ) -> Result<bool, Option<SetPropState>> {
        let class_id = self.class_id();
        let p = self as *mut JSObject;

        loop {
            if let Some((prs, pr)) = self.find_own_property_mut(prop) {
                match prs.flags() {
                    f if f & (JS_PROP_TMASK | JS_PROP_WRITABLE | JS_PROP_LENGTH)
                        == JS_PROP_WRITABLE =>
                    {
                        // fast case
                        set_value(ctx, &mut pr.u.value, val);
                        return Ok(true);
                    }
                    f if f & JS_PROP_LENGTH != 0 => {
                        // set .length property of an array object
                        debug_assert!(self.class_id() == JS_CLASS_ARRAY);
                        debug_assert_eq!(prop, JS_ATOM_length);
                        return self.set_array_length(ctx, val, flags).map_err(|_| None);
                    }
                    f if f & JS_PROP_TMASK == JS_PROP_GETSET => {
                        return match call_setter(ctx, pr.u.getset.setter, this_obj, val, flags) {
                            0 => Ok(false),
                            1 => Ok(true),
                            -1 => Err(None),
                            _ => unreachable!(),
                        };
                    }
                    f if f & JS_PROP_TMASK == JS_PROP_VARREF => {
                        /* JS_PROP_WRITABLE is always true for variable
                        references, but they are write protected in module name
                        spaces. */
                        if class_id == JS_CLASS_MODULE_NS {
                            return Err(Some(SetPropState::ErrReadOnly));
                        }
                        set_value(ctx, (*pr.u.var_ref).cast::<JSVarRef>().as_mut().pvalue, val);
                        return Ok(true);
                    }
                    f if f & JS_PROP_TMASK == JS_PROP_AUTOINIT => {
                        if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                            JS_FreeValue(ctx, val);
                            return Err(None);
                        }
                        continue; // try again
                    }
                    _ => {
                        return Err(Some(SetPropState::ErrReadOnly));
                    }
                }
            } else {
                return Err(Some(SetPropState::ExoticAndGoUp));
            }
        }
    }

    #[inline(always)]
    unsafe fn set_property_setter(
        &mut self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValue,
        flags: u32,
    ) -> Result<bool, Option<SetPropState>> {
        let p = self as *mut JSObject;

        loop {
            if let Some((prs, pr)) = self.find_own_property_mut(prop) {
                let pr_flags = prs.flags();
                if pr_flags & JS_PROP_TMASK == JS_PROP_GETSET {
                    return match call_setter(ctx, pr.u.getset.setter, this_obj, val, flags) {
                        0 => Ok(false),
                        1 => Ok(true),
                        -1 => Err(None),
                        _ => unreachable!(),
                    };
                } else if pr_flags & JS_PROP_TMASK == JS_PROP_AUTOINIT {
                    // Instantiate property and retry (potentially useless)
                    if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                        return Err(None);
                    }
                    continue; // try again
                } else if pr_flags & JS_PROP_WRITABLE == 0 {
                    return Err(Some(SetPropState::ErrReadOnly));
                }
            }

            return Err(Some(SetPropState::ExoticAndGoUp));
        }
    }
}

impl JSValue {
    /// Note: `val` is consumed; `self` and `this_obj` are not consumed.
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    /// If this_obj != self, then self must be an object (Reflect.set case).
    pub(crate) unsafe fn set_property_internal(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let mut p_self: *mut JSObject;
        let p_this: *mut JSObject;
        let this_tag = JS_VALUE_GET_NORM_TAG(this_obj);

        let mut state = if this_tag != JSTag::Object {
            if self.tag() == JS_TAG_OBJECT {
                p_self = self.object_unchecked() as _;
                p_this = std::ptr::null_mut();
                SetPropState::SetterOrAutoInit
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
                        p_this = std::ptr::null_mut();
                        p_self = JS_VALUE_GET_OBJ(JS_GetPrototypePrimitive(ctx, self));
                        if p_self.is_null() {
                            SetPropState::Add
                        } else {
                            SetPropState::SetterOrAutoInit
                        }
                    }
                }
            }
        } else {
            p_self = self.object_unchecked() as _;
            p_this = this_obj.object_unchecked() as _;

            if p_this == p_self {
                match (*p_this).set_property_fast(ctx, prop, val, this_obj, flags) {
                    Ok(b) => {
                        return Ok(b);
                    }
                    Err(None) => {
                        return Err(-1);
                    }
                    Err(Some(st)) => st,
                }
            } else {
                SetPropState::SetterOrAutoInit
            }
        };

        loop {
            match state {
                SetPropState::ExoticAndGoUp => {
                    if (*p_self).detailed.is_exotic() {
                        if (*p_self).detailed.fast_array() {
                            // set array element
                            if prop.is_int_type() {
                                let idx = __JS_AtomToUInt32(prop);
                                if idx < (*p_self).array_length_unchecked() as u32 {
                                    if p_this == p_self {
                                        return this_obj.set_property_value(
                                            ctx,
                                            JSValue::from_i32(idx as _),
                                            val,
                                            flags,
                                        );
                                    } else {
                                        state = SetPropState::Add;
                                        continue;
                                    }
                                } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                                    .contains(&(*p_self).class_id())
                                {
                                    state = SetPropState::ErrTypedArrayOOB;
                                    continue;
                                }
                            } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                                .contains(&(*p_self).class_id())
                            {
                                let ret = JS_AtomIsNumericIndex(ctx, prop);
                                if ret != 0 {
                                    if ret < 0 {
                                        JS_FreeValue(ctx, val);
                                        return Err(-1);
                                    }
                                    state = SetPropState::ErrTypedArrayOOB;
                                    continue;
                                }
                            }
                        } else {
                            // use exotic handler
                            let em = ctx.rt().class_array[(*p_self).class_id() as usize].exotic;
                            if !em.is_null() {
                                if let Some(cb) = (*em).set_property {
                                    // set_property can free the prototype
                                    let obj1 = JS_MKPTR!(JSTag::Object, p_self.cast()).dup();
                                    let ret = cb(ctx, obj1, prop, val, this_obj, flags as _);
                                    JS_FreeValue(ctx, obj1);
                                    JS_FreeValue(ctx, val);
                                    return match ret {
                                        0 => Ok(false),
                                        1 => Ok(true),
                                        -1 => Err(-1),
                                        _ => unreachable!(),
                                    };
                                } else if let Some(cb) = (*em).get_own_property {
                                    // get_own_property can free the prototype
                                    let obj1 = JS_MKPTR!(JSTag::Object, p_self.cast()).dup();
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
                                                state = SetPropState::ErrReadOnly;
                                                continue;
                                            }

                                            if p_this == p_self {
                                                let ret = this_obj.define_property(
                                                    ctx,
                                                    prop,
                                                    val,
                                                    JSValue::UNDEFINED,
                                                    JSValue::UNDEFINED,
                                                    JS_PROP_HAS_VALUE,
                                                );
                                                JS_FreeValue(ctx, val);
                                                return ret;
                                            } else {
                                                state = SetPropState::Add;
                                                continue;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } // is_exotic()

                    // walk up proto chain
                    p_self = (*p_self).shape().unwrap().proto;
                    state = if p_self.is_null() {
                        SetPropState::Add
                    } else {
                        SetPropState::SetterOrAutoInit
                    };
                    continue;
                }
                SetPropState::SetterOrAutoInit => {
                    match (*p_self).set_property_setter(ctx, prop, val, this_obj, flags) {
                        Ok(b) => {
                            return Ok(b);
                        }
                        Err(Some(st)) => {
                            state = st;
                            continue;
                        }
                        Err(None) => {
                            return Err(-1);
                        }
                    }
                }

                SetPropState::Add => {
                    if flags & JS_PROP_NO_ADD != 0 {
                        JS_FreeValue(ctx, val);
                        JS_ThrowReferenceErrorNotDefined(ctx, prop);
                        return Err(-1);
                    }
                    if p_this.is_null() {
                        JS_FreeValue(ctx, val);
                        return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, c"not an object") {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }
                    if !(*p_this).detailed.extensible() {
                        JS_FreeValue(ctx, val);
                        return if ctx
                            .JS_ThrowTypeErrorOrFalse(flags as _, c"object is not extensible")
                        {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }

                    if p_this == JS_VALUE_GET_OBJ(self) {
                        if (*p_this).detailed.is_exotic() {
                            if (*p_this).class_id() == JS_CLASS_ARRAY
                                && (*p_this).detailed.fast_array()
                                && prop.is_int_type()
                            {
                                let idx = __JS_AtomToUInt32(prop);
                                if idx == (*p_this).array_length_unchecked() as u32 {
                                    return add_fast_array_element(ctx, p_this, val, flags);
                                } else {
                                    state = SetPropState::CreateProp;
                                    continue;
                                }
                            } else {
                                state = SetPropState::CreateProp;
                                continue;
                            }
                        } else {
                            match (*p_this).add_property(ctx, prop, JS_PROP_C_W_E) {
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
                        let ret = JS_GetOwnPropertyInternal(ctx, &mut desc, p_this, prop);
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
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            } else {
                                JS_FreeValue(ctx, desc.value);
                                if desc.flags & JS_PROP_WRITABLE == 0
                                    || (*p_this).class_id() == JS_CLASS_MODULE_NS
                                {
                                    state = SetPropState::ErrReadOnly;
                                    continue;
                                }
                            }
                            let ret = this_obj.define_property(
                                ctx,
                                prop,
                                val,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                JS_PROP_HAS_VALUE,
                            );
                            JS_FreeValue(ctx, val);
                            return ret;
                        } else {
                            state = SetPropState::CreateProp;
                            continue;
                        }
                    }
                }
                SetPropState::CreateProp => {
                    let ret = (*p_this).create_property(
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

                SetPropState::ErrReadOnly => {
                    JS_FreeValue(ctx, val);
                    return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }
                SetPropState::ErrTypedArrayOOB => {
                    // must convert the argument even if out of bound access
                    if matches!(
                        (*p_self).class_id(),
                        JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY
                    ) {
                        unimplemented!()
                        // let mut v:i64;
                        // if JS_ToBigInt64Free(ctx, &v, val)!=0{
                        //     return -1;
                        // }
                    } else {
                        let v = val.coerce_to_number_free(ctx)?; // consume val
                        JS_FreeValue(ctx, v);
                    }
                    return Ok(true);
                }
            }
        }
    }

    /// Note: `val` is consumed; `self` and `this_obj` keep ref count unchanged.
    #[inline(always)]
    pub fn set_property(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
    ) -> Result<bool, i32> {
        unsafe { self.set_property_internal(ctx, prop, val, self, JS_PROP_THROW) }
    }

    /// Note: `val` is consumed
    pub fn set_property_value(
        self,
        ctx: &mut JSContext,
        prop: JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if self.tag() == JS_TAG_OBJECT && prop.tag() == JS_TAG_INT {
            let p = unsafe { self.object_unchecked() };
            let idx = JS_VALUE_GET_INT(prop);
            let mut ta_out_of_bound = false;

            // early return using fast path if possible
            match p.class_id() {
                JS_CLASS_ARRAY => {
                    // fast path: is fast array
                    if p.detailed.fast_array() && p.detailed.extensible() {
                        let lst = unsafe { p.fast_array_values_mut_unchecked() };

                        match idx.cmp(&(lst.len() as i32)) {
                            std::cmp::Ordering::Less => {
                                // update array element
                                unsafe {
                                    set_value(ctx, lst.get_mut(idx as usize).unwrap(), val);
                                }
                                return Ok(true);
                            }
                            std::cmp::Ordering::Equal => {
                                // 检查原型链是否有数字属性
                                let mut has_int_prop = false;
                                let mut p1 = p.shape().unwrap().proto;
                                unsafe {
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
                            std::cmp::Ordering::Greater => {}
                        }
                    }
                }
                JS_CLASS_ARGUMENTS => {
                    let lst = unsafe { p.fast_array_values_mut_unchecked() };
                    if idx < lst.len() as i32 {
                        unsafe {
                            set_value(ctx, lst.get_mut(idx as usize).unwrap(), val);
                        }
                        return Ok(true);
                    }
                }

                //
                // typed array
                //
                JS_CLASS_UINT8C_ARRAY => {
                    let mut v = 0;
                    if unsafe { JS_ToUint8ClampFree(ctx, &mut v, val) } != 0 {
                        return Err(-1);
                    }

                    if unsafe { UncheckedInto::<Uint8Array>::unchecked_into(self) }
                        .set_element(idx as _, v as _)
                        .is_err()
                    {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }

                    // original code
                    // if idx >= p.array_length_unchecked() as i32 {
                    //     ta_out_of_bound = true;
                    // } else {
                    //     p.as_typed_array_obj_unchecked()
                    //         .with_data_mut_ptr::<u8, _>(|ptr| {
                    //             *ptr.add(idx as usize) = v as u8;
                    //         });
                    //     return Ok(true);
                    // }
                }
                cid @ (JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY) => {
                    let v = if let Ok(n) = val.coerce_to_i32_free(ctx) {
                        n as u8
                    } else {
                        return Err(-1);
                    };

                    let r = unsafe {
                        if cid == JS_CLASS_INT8_ARRAY {
                            UncheckedInto::<Int8Array>::unchecked_into(self)
                                .set_element(idx as _, v as _)
                        } else {
                            UncheckedInto::<Uint8Array>::unchecked_into(self)
                                .set_element(idx as _, v as _)
                        }
                    };

                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }

                    // original code
                    // if idx >= p.array_length_unchecked() as i32 {
                    //     ta_out_of_bound = true;
                    // } else {
                    //     p.as_typed_array_obj_unchecked()
                    //         .with_data_mut_ptr::<u8, _>(|ptr| {
                    //             *ptr.add(idx as usize) = v;
                    //         });
                    //     return Ok(true);
                    // }
                }
                cid @ (JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY) => {
                    let v = if let Ok(n) = val.coerce_to_i32_free(ctx) {
                        n as u16
                    } else {
                        return Err(-1);
                    };

                    let r = unsafe {
                        if cid == JS_CLASS_INT16_ARRAY {
                            UncheckedInto::<Int16Array>::unchecked_into(self)
                                .set_element(idx as _, v as _)
                        } else {
                            UncheckedInto::<Uint16Array>::unchecked_into(self)
                                .set_element(idx as _, v as _)
                        }
                    };

                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }

                    // if idx >= p.array_length_unchecked() as i32 {
                    //     ta_out_of_bound = true;
                    // } else {
                    //     p.as_typed_array_obj_unchecked()
                    //         .with_data_mut_ptr::<u16, _>(|ptr| {
                    //             *ptr.add(idx as usize) = v;
                    //         });
                    //     return Ok(true);
                    // }
                }
                cid @ (JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY) => {
                    let v = if let Ok(n) = val.coerce_to_i32_free(ctx) {
                        n as u32
                    } else {
                        return Err(-1);
                    };

                    let r = unsafe {
                        if cid == JS_CLASS_INT32_ARRAY {
                            UncheckedInto::<Int32Array>::unchecked_into(self)
                                .set_element(idx as _, v as _)
                        } else {
                            UncheckedInto::<Uint32Array>::unchecked_into(self)
                                .set_element(idx as _, v as _)
                        }
                    };

                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }

                    // if idx >= p.array_length_unchecked() as i32 {
                    //     ta_out_of_bound = true;
                    // } else {
                    //     p.as_typed_array_obj_unchecked()
                    //         .with_data_mut_ptr::<u32, _>(|ptr| {
                    //             *ptr.add(idx as usize) = v;
                    //         });
                    //     return Ok(true);
                    // }
                }
                JS_CLASS_FLOAT32_ARRAY => {
                    let mut d = 0.0;
                    if unsafe { JS_ToFloat64Free(ctx, &mut d, val) } != 0 {
                        return Err(-1);
                    }

                    let r = unsafe {
                        UncheckedInto::<Float32Array>::unchecked_into(self)
                            .set_element(idx as _, d as _)
                    };

                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }

                    // original code
                    // if idx >= p.array_length_unchecked() as i32 {
                    //     ta_out_of_bound = true;
                    // } else {
                    //     p.as_typed_array_obj_unchecked()
                    //         .with_data_mut_ptr::<f32, _>(|ptr| {
                    //             *ptr.add(idx as usize) = d as f32;
                    //         });
                    //     return Ok(true);
                    // }
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    let mut d = 0.0;
                    if unsafe { JS_ToFloat64Free(ctx, &mut d, val) } != 0 {
                        return Err(-1);
                    }

                    let r = unsafe {
                        UncheckedInto::<Float64Array>::unchecked_into(self).set_element(idx as _, d)
                    };
                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }

                    // original code
                    // if idx >= p.array_length_unchecked() as i32 {
                    //     ta_out_of_bound = true;
                    // } else {
                    //     p.as_typed_array_obj_unchecked()
                    //         .with_data_mut_ptr::<f64, _>(|ptr| {
                    //             *ptr.add(idx as usize) = d;
                    //         });
                    //     return Ok(true);
                    // }
                }
                JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                    unimplemented!()
                }

                _ => {
                    unreachable!()
                }
            }

            if ta_out_of_bound {
                return Ok(true);
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
