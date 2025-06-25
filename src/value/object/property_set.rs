use std::{ffi::CStr, ops::Deref};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::{
        __JS_AtomToUInt32, JS_ATOM_NULL, JS_AtomIsNumericIndex, JS_FreeAtom, JS_ValueToAtom,
        JSAtom, consts::JS_ATOM_length,
    },
    context::{JSContext, is_strict_mode},
    error::JS_ThrowReferenceErrorNotDefined,
    function::JS_Call,
    typed_array::{
        Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Uint8Array, Uint16Array,
        Uint32Array,
    },
    utils::UncheckedInto,
    value::{
        JS_ToFloat64, JS_ToUint8Clamp,
        object::{JS_PROP_AUTOINIT, JS_PROP_LENGTH, JS_PROP_THROW_STRICT, JSProperty},
    },
};

use super::{
    super::{JS_FreeValue, JS_GetPrototypePrimitive, JSValue},
    JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_WRITABLE,
    obj::JSObject,
};

#[derive(Debug, Clone, PartialEq)]
enum SetPropState {
    /// Try exotic set handler, and walk up through proto chain
    ExoticAndGoUp,
    /// Set property value or instantiate property
    SetOrInit,
    /// Add or update property
    Add,
    /// Create new property on object
    CreateProp,
    /// Read-only error
    ErrReadOnly,
    /// TypedArray out of bounds error
    ErrTypedArrayOOB,
}

pub(crate) fn call_setter(
    ctx: &JSContext,
    setter: Option<&JSObject>,
    this_obj: &JSValue,
    val: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    if let Some(func) = setter {
        JS_Call(
            ctx,
            &JSValue::Object(func.clone()),
            this_obj,
            std::slice::from_ref(&val),
        )
        .check_exception()?;
        Ok(true)
    } else {
        if flags & JS_PROP_THROW != 0 || (flags & JS_PROP_THROW_STRICT != 0 && is_strict_mode(ctx))
        {
            ctx.JS_ThrowTypeError("no setter for property");
            return Err(-1);
        }
        Ok(false)
    }
}

impl JSObject {
    unsafe fn set_property_exotic(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValue,
        flags: u32,
    ) -> Result<bool, Option<SetPropState>> {
        debug_assert!(self.flags().is_exotic());

        let mut new_state = SetPropState::ExoticAndGoUp;

        if self.flags().fast_array() {
            // set array element
            if prop.is_int_type() {
                let idx = __JS_AtomToUInt32(prop);
                if idx < self.array_length_unchecked() as u32 {
                    if this_obj.object().map_or(false, |o| o == self) {
                        return this_obj
                            .set_property_value(ctx, &JSValue::from_i32(idx as _), val, flags)
                            .map_err(|_| None);
                    } else {
                        new_state = SetPropState::Add;
                    }
                } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                    .contains(&self.class_id())
                {
                    new_state = SetPropState::ErrTypedArrayOOB;
                }
            } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&self.class_id()) {
                let ret = JS_AtomIsNumericIndex(ctx, prop);
                if ret != 0 {
                    if ret < 0 {
                        return Err(None);
                    }
                    new_state = SetPropState::ErrTypedArrayOOB;
                }
            }
        } else {
            // use exotic handler
            let em = ctx.rt().class_array[self.class_id() as usize].exotic;
            if !em.is_null() {
                if let Some(cb) = (*em).set_property {
                    return cb(
                        ctx,
                        JSValue::Object(self.clone()),
                        prop,
                        val,
                        this_obj,
                        flags as _,
                    )
                    .map_err(|_| None);
                } else if let Some(cb) = (*em).get_own_property {
                    // get_own_property can free the prototype
                    if let Some(desc) =
                        cb(ctx, JSValue::Object(self.clone()), prop).map_err(|_| None)?
                    {
                        if desc.flags & JS_PROP_GETSET != 0 {
                            return call_setter(ctx, desc.setter.object(), &this_obj, val, flags)
                                .map_err(|_| None);
                        } else {
                            if desc.flags & JS_PROP_WRITABLE == 0 {
                                new_state = SetPropState::ErrReadOnly;
                            }

                            if this_obj.object().map_or(false, |o| o == self) {
                                return this_obj
                                    .define_property(
                                        ctx,
                                        prop,
                                        &val,
                                        &JSValue::UNDEFINED,
                                        &JSValue::UNDEFINED,
                                        JS_PROP_HAS_VALUE,
                                    )
                                    .map_err(|_| None);
                            }

                            new_state = SetPropState::Add;
                        }
                    }
                }
            }
        }

        Err(Some(new_state))
    }

    /// assign property value at specified slot idx.
    /// if the property is auto init type, do instantiate instead.
    pub(super) fn internal_assign_prop_value(
        &self,
        ctx: &JSContext,
        prs_flags: u32,
        prop: JSAtom,
        idx: usize,
        value: JSValue,
        this_val: &JSObject,
        flags: u32,
    ) -> Result<bool, i32> {
        if prs_flags & JS_PROP_LENGTH != 0 {
            // special prop: length
            // to set .length property of an array object
            debug_assert_eq!(self.class_id(), JS_CLASS_ARRAY);
            debug_assert_eq!(prop, JS_ATOM_length);
            debug_assert_eq!(idx, 0, "length property must be indexed at 0");
            return self.set_array_length(ctx, value, flags);
        }

        if prs_flags & JS_PROP_AUTOINIT != 0 {
            // auto init property
            // if prs_flags & JS_PROP_WRITABLE == 0 {
            //     return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
            //         Err(-1)
            //     } else {
            //         Ok(false)
            //     };
            // }
            let mut pr = std::mem::replace(
                &mut self.0.prop_values.borrow_mut()[idx],
                JSProperty::default(),
            );
            println!(
                "[O.o][init prop] {:?} {}, {:?}",
                prop,
                prop.to_str(ctx.rt()),
                pr
            );
            debug_assert!(matches!(pr, JSProperty::AutoInit { .. }));

            pr.instantiate(this_val, prop)?;
            self.0.prop_values.borrow_mut()[idx] = pr;
            Ok(true)
        } else {
            // normal, getter, setter
            let pr = &mut self.0.prop_values.borrow_mut()[idx];

            match pr {
                JSProperty::Value(v) => {
                    if prs_flags & JS_PROP_WRITABLE == 0 {
                        return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }

                    *v = value;
                    Ok(true)
                }
                JSProperty::GetSet { setter, .. } => call_setter(
                    ctx,
                    setter.as_ref(),
                    &JSValue::Object(self.clone()),
                    value,
                    flags,
                ),
                JSProperty::VarRef(vref) => {
                    // JS_PROP_WRITABLE is always true for variable references,
                    // but they are write protected in module name spaces.
                    if self.class_id() == JS_CLASS_MODULE_NS {
                        if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                            Err(-1)
                        } else {
                            Ok(false)
                        }
                    } else {
                        vref.set_value(value);
                        Ok(true)
                    }
                }
                JSProperty::AutoInit { .. } => {
                    #[cfg(debug_assertions)]
                    unreachable!()
                }
            }
        }
    }
}

impl JSValue {
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// **In most cases, obj==this_obj, except in case of Reflect.set(obj, val, [receiver]),
    /// where obj (must be an object), and this_obj is for receiver.**
    pub(crate) unsafe fn set_property_internal(
        ctx: &JSContext,
        obj: &JSValue,
        prop: JSAtom,
        val: JSValue,
        this_obj: &JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let mut ob_obj: Option<JSObject>;
        let ob_this: Option<&JSObject>;

        // pre check
        let mut state = match (obj, this_obj) {
            (JSValue::Object(o), JSValue::Object(this)) => {
                ob_obj = Some(o.clone());
                ob_this = Some(this);

                if o == this {
                    if let Some((idx, prs_flags)) = this.find_own_property(prop) {
                        return this.internal_assign_prop_value(
                            ctx,
                            prs_flags,
                            prop,
                            idx,
                            val.clone(),
                            this,
                            flags,
                        );
                    }
                    SetPropState::ExoticAndGoUp
                } else {
                    SetPropState::SetOrInit
                }
            }
            (JSValue::NULL, _) | (_, JSValue::NULL) => {
                ctx.JS_ThrowTypeError(&format!(
                    "cannot set property '{}' of null",
                    prop.to_str(ctx.rt())
                ));
                return Err(-1);
            }
            (JSValue::UNDEFINED, _) | (_, JSValue::UNDEFINED) => {
                ctx.JS_ThrowTypeError(&format!(
                    "cannot set property '{}' of undefined",
                    prop.to_str(ctx.rt())
                ));
                return Err(-1);
            }
            _ => {
                // even on a primitive type we can have setters on the prototype
                ob_this = None;
                if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, obj) {
                    ob_obj = Some(p);
                    SetPropState::SetOrInit
                } else {
                    ob_obj = None;
                    SetPropState::Add
                }
            }
        };

        // process states
        loop {
            match state {
                SetPropState::ExoticAndGoUp => {
                    let target = ob_obj.clone().unwrap();

                    if target.flags().is_exotic() {
                        match target.set_property_exotic(
                            ctx,
                            prop,
                            val.clone(),
                            this_obj.clone(),
                            flags,
                        ) {
                            Ok(b) => {
                                return Ok(b);
                            }
                            Err(None) => {
                                return Err(-1);
                            }
                            Err(Some(next_state)) => {
                                state = next_state;
                            }
                        }
                    } // is_exotic()

                    if SetPropState::ExoticAndGoUp == state {
                        // walk up proto chain
                        state = if let Some(up) = target.shape().unwrap().0.borrow().proto.clone() {
                            ob_obj = Some(up);
                            SetPropState::SetOrInit
                        } else {
                            ob_obj = None;
                            SetPropState::Add
                        };
                    }

                    continue;
                }
                SetPropState::SetOrInit => {
                    let target = ob_obj.as_ref().unwrap();
                    if let Some((idx, prs_flags)) = target.find_own_property(prop) {
                        return target.internal_assign_prop_value(
                            ctx,
                            prs_flags,
                            prop,
                            idx,
                            val.clone(),
                            ob_this.unwrap(),
                            flags,
                        );
                    }

                    state = SetPropState::ExoticAndGoUp;
                    continue;
                }

                SetPropState::Add => {
                    // to add or update a property on `this_obj`
                    if flags & JS_PROP_NO_ADD != 0 {
                        JS_ThrowReferenceErrorNotDefined(ctx, prop);
                        return Err(-1);
                    }
                    if ob_this.is_none() {
                        return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, "not an object") {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }

                    let target = ob_this.unwrap();

                    if !target.flags().extensible() {
                        return if ctx
                            .JS_ThrowTypeErrorOrFalse(flags as _, "object is not extensible")
                        {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }

                    if ob_this == ob_obj.as_ref() {
                        if target.flags().is_exotic() {
                            if target.class_id() == JS_CLASS_ARRAY
                                && target.flags().fast_array()
                                && prop.is_int_type()
                            {
                                // fast array
                                let idx = __JS_AtomToUInt32(prop);
                                if idx == target.array_length_unchecked() as u32 {
                                    return target.add_fast_array_element(ctx, val, flags);
                                }
                            }

                            state = SetPropState::CreateProp;
                            continue;
                        } else {
                            target.add_property_then(ctx, prop, JS_PROP_C_W_E, |pr| {
                                *pr = JSProperty::Value(val);
                            })?;
                            return Ok(true);
                        }
                    } else {
                        // generic case: modify the property in this_obj if it already exists
                        match target.get_own_property_descr(ctx, prop)? {
                            Some(desc) => {
                                // already exists, update the value
                                if desc.flags & JS_PROP_GETSET != 0 {
                                    return if ctx
                                        .JS_ThrowTypeErrorOrFalse(flags as _, "setter is forbidden")
                                    {
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    };
                                } else {
                                    if desc.flags & JS_PROP_WRITABLE == 0
                                        || target.class_id() == JS_CLASS_MODULE_NS
                                    {
                                        state = SetPropState::ErrReadOnly;
                                        continue;
                                    }
                                }

                                return this_obj.define_property(
                                    ctx,
                                    prop,
                                    &val,
                                    &JSValue::UNDEFINED,
                                    &JSValue::UNDEFINED,
                                    JS_PROP_HAS_VALUE,
                                );
                            }
                            None => {
                                // not exist, should create
                                state = SetPropState::CreateProp;
                                continue;
                            }
                        }
                    }
                }
                SetPropState::CreateProp => {
                    // create property on this_obj
                    return ob_this.unwrap().create_property(
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
                }

                SetPropState::ErrReadOnly => {
                    JS_FreeValue(ctx, &val);
                    return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }
                SetPropState::ErrTypedArrayOOB => {
                    // must convert the argument even if out of bound access
                    if matches!(
                        ob_obj.unwrap().class_id(),
                        JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY
                    ) {
                        unimplemented!()
                        // let mut v:i64;
                        // if JS_ToBigInt64Free(ctx, &v, val)!=0{
                        //     return -1;
                        // }
                    } else {
                        let v = val.coerce_to_number_free(ctx)?; // consume val
                    }
                    return Ok(true);
                }
            }
        }
    }

    #[inline(always)]
    pub fn set_property(&self, ctx: &JSContext, prop: JSAtom, val: JSValue) -> Result<bool, i32> {
        unsafe { Self::set_property_internal(ctx, self, prop, val, self, JS_PROP_THROW) }
    }

    pub fn set_property_value(
        &self,
        ctx: &JSContext,
        prop: &JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        match (self, prop) {
            (JSValue::Object(p), JSValue::Int(idx)) => {
                let idx = *idx;
                let mut ta_out_of_bound = false;

                // early return using fast path if possible
                match p.class_id() {
                    JS_CLASS_ARRAY => {
                        // fast path: fast array
                        if p.flags().fast_array() && p.flags().extensible() {
                            let r = p
                                .with_fast_array_values_mut(|lst| -> Result<Option<bool>, i32> {
                                    match idx.cmp(&(lst.len() as i32)) {
                                        std::cmp::Ordering::Less => {
                                            // update array element
                                            std::mem::replace(
                                                lst.get_mut(idx as usize).unwrap(),
                                                val.clone(),
                                            );
                                            return Ok(Some(true));
                                        }
                                        std::cmp::Ordering::Equal => {
                                            // 检查原型链是否有数字属性
                                            let mut has_int_prop = false;
                                            let mut p1 =
                                                p.shape().unwrap().0.borrow().proto.clone();

                                            while p1.is_some() && !has_int_prop {
                                                let ob = p1.unwrap();
                                                let sh1 = ob.shape().unwrap();

                                                match ob.class_id() {
                                                    JS_CLASS_ARRAY => {
                                                        if !ob.flags().fast_array() {
                                                            has_int_prop = true;
                                                        }
                                                    }
                                                    JS_CLASS_OBJECT => {
                                                        if sh1.0.borrow().has_small_array_index {
                                                            has_int_prop = true;
                                                        }
                                                    }
                                                    _ => {
                                                        has_int_prop = true;
                                                    }
                                                }

                                                p1 = sh1.0.borrow().proto.clone();
                                            }

                                            if !has_int_prop {
                                                // append array element
                                                return p
                                                    .add_fast_array_element(ctx, val.clone(), flags)
                                                    .map(|b| Some(b));
                                            }
                                        }
                                        std::cmp::Ordering::Greater => {}
                                    }

                                    Ok(None)
                                })
                                .unwrap()?;

                            match r {
                                Some(b) => {
                                    return Ok(b); // done
                                }
                                None => {
                                    // handle with slow array
                                }
                            }
                        }
                    }
                    JS_CLASS_ARGUMENTS => {
                        let done = p
                            .with_fast_array_values_mut(|lst| -> bool {
                                if idx < lst.len() as i32 {
                                    std::mem::replace(
                                        lst.get_mut(idx as usize).unwrap(),
                                        val.clone(),
                                    );
                                    true
                                } else {
                                    false
                                }
                            })
                            .unwrap();
                        if done {
                            return Ok(true); // done fast array
                        }
                    }

                    //
                    // typed array
                    //
                    JS_CLASS_UINT8C_ARRAY => {
                        let mut v = 0;
                        if unsafe { JS_ToUint8Clamp(ctx, &mut v, &val) } != 0 {
                            return Err(-1);
                        }

                        if unsafe { UncheckedInto::<Uint8Array>::unchecked_into(self.clone()) }
                            .set_element(idx as _, v as _)
                            .is_err()
                        {
                            ta_out_of_bound = true;
                        } else {
                            return Ok(true);
                        }
                    }
                    cid @ (JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY) => {
                        let v = if let Ok(n) = val.coerce_to_i32(ctx) {
                            n as u8
                        } else {
                            return Err(-1);
                        };

                        let r = unsafe {
                            if cid == JS_CLASS_INT8_ARRAY {
                                UncheckedInto::<Int8Array>::unchecked_into(self.clone())
                                    .set_element(idx as _, v as _)
                            } else {
                                UncheckedInto::<Uint8Array>::unchecked_into(self.clone())
                                    .set_element(idx as _, v as _)
                            }
                        };

                        if r.is_err() {
                            ta_out_of_bound = true;
                        } else {
                            return Ok(true);
                        }
                    }
                    cid @ (JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY) => {
                        let v = if let Ok(n) = val.coerce_to_i32(ctx) {
                            n as u16
                        } else {
                            return Err(-1);
                        };

                        let r = unsafe {
                            if cid == JS_CLASS_INT16_ARRAY {
                                UncheckedInto::<Int16Array>::unchecked_into(self.clone())
                                    .set_element(idx as _, v as _)
                            } else {
                                UncheckedInto::<Uint16Array>::unchecked_into(self.clone())
                                    .set_element(idx as _, v as _)
                            }
                        };

                        if r.is_err() {
                            ta_out_of_bound = true;
                        } else {
                            return Ok(true);
                        }
                    }
                    cid @ (JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY) => {
                        let v = if let Ok(n) = val.coerce_to_i32(ctx) {
                            n as u32
                        } else {
                            return Err(-1);
                        };

                        let r = unsafe {
                            if cid == JS_CLASS_INT32_ARRAY {
                                UncheckedInto::<Int32Array>::unchecked_into(self.clone())
                                    .set_element(idx as _, v as _)
                            } else {
                                UncheckedInto::<Uint32Array>::unchecked_into(self.clone())
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
                        if unsafe { JS_ToFloat64(ctx, &mut d, &val) } != 0 {
                            return Err(-1);
                        }

                        let r = unsafe {
                            UncheckedInto::<Float32Array>::unchecked_into(self.clone())
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
                        if unsafe { JS_ToFloat64(ctx, &mut d, &val) } != 0 {
                            return Err(-1);
                        }

                        let r = unsafe {
                            UncheckedInto::<Float64Array>::unchecked_into(self.clone())
                                .set_element(idx as _, d)
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
            _ => {}
        } // match

        // fail safe: slow_path
        let atom = JS_ValueToAtom(ctx, &prop);
        if atom == JS_ATOM_NULL {
            return Err(-1);
        }
        let ret = unsafe { Self::set_property_internal(ctx, self, atom, val, self, flags) };
        JS_FreeAtom(ctx, atom);
        ret
    }

    #[inline(always)]
    pub fn set_property_u32(&self, ctx: &JSContext, idx: u32, val: JSValue) -> Result<bool, i32> {
        self.set_property_value(ctx, &JSValue::from(idx), val, JS_PROP_THROW)
    }

    pub fn set_property_i64(&self, ctx: &JSContext, idx: i64, val: JSValue) -> Result<bool, i32> {
        if (idx as u64) <= i32::MAX as u64 {
            // fast path for fast arrays
            return self.set_property_value(ctx, &JSValue::Int(idx as i32), val, JS_PROP_THROW);
        }

        let prop = ctx.rt_mut().register_atom_i64(idx);
        if prop == JS_ATOM_NULL {
            return Err(-1);
        }

        self.set_property(ctx, prop, val)
    }

    #[inline]
    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
        let ret = self.set_property(ctx, atom, val);
        JS_FreeAtom(ctx, atom);
        ret
    }
}
