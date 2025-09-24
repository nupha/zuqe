use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::{JS_AtomIsNumericIndex, JS_FreeAtom, JSAtom, consts::JS_ATOM_length},
    context::{JSContext, is_strict_mode},
    error::JS_ThrowReferenceErrorNotDefined,
    object::{
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT,
        JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_THROW_STRICT, JS_PROP_TMASK, JS_PROP_VARREF,
        JSObject, JSProperty,
    },
    typed_array::{
        Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, TypedArray, Uint8Array,
        Uint16Array, Uint32Array,
    },
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{
    JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_VALUE, JS_PROP_NO_ADD, JS_PROP_THROW,
    JS_PROP_WRITABLE,
};

impl JSObject {
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// In case of `Reflect.set(obj, val, [receiver])`, `receiver` could be null, undefined or Some(object).
    /// For other cases,  `receiver` should be None, suggesting it's same to `self`.
    ///
    /// `prop` is not consumed.
    pub(crate) fn set_property_internal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        receiver: Option<&JSObject>,
        flags: u32,
    ) -> Result<bool, i32> {
        macro_rules! fail {
            ($msg:expr) => {
                return if ctx.can_throw(flags) {
                    ctx.throw_type_error($msg);
                    Err(-1)
                } else {
                    Ok(false)
                }
            };
        }

        macro_rules! fail_read_only {
            ($prop:expr) => {
                return if ctx.can_throw(flags) {
                    ctx.throw_type_error_read_only($prop);
                    Err(-1)
                } else {
                    Ok(false)
                }
            };
        }

        let this = receiver.unwrap_or(self);

        for cur_obj in self.shape_proto_iter(true) {
            // 1. find shape property
            'shape: while let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                match prs_flags & JS_PROP_TMASK {
                    JS_PROP_NORMAL if cur_obj.eq(this) => {
                        // possible to update property value, when proto == this
                        if prs_flags & JS_PROP_LENGTH != 0 {
                            // set array length
                            debug_assert_eq!(this.class_id(), JS_CLASS_ARRAY);
                            debug_assert_eq!(prop, JS_ATOM_length);
                            return this.as_Array().unwrap().set_length(
                                ctx,
                                val.to_array_length(ctx, false)?,
                                ctx.can_throw(flags),
                            );
                        }

                        if prs_flags & JS_PROP_WRITABLE != 0 {
                            this.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(val);
                            return Ok(true);
                        } else {
                            fail_read_only!(prop)
                        }
                    }

                    JS_PROP_VARREF if cur_obj.eq(this) => {
                        if this.class_id() == JS_CLASS_MODULE_NS {
                            fail_read_only!(prop)
                        }

                        if let Some(JSProperty::VarRef(vref)) =
                            this.0.prop_values.borrow_mut().get_mut(idx)
                        {
                            vref.set_value(val);
                            return Ok(true);
                        } else {
                            unreachable!()
                        }
                    }

                    JS_PROP_GETSET => {
                        let gs =
                            unsafe { cur_obj.0.prop_values.borrow().get_unchecked(idx).clone() };
                        return if let JSProperty::GetSet { setter, .. } = gs {
                            call_setter(ctx, setter.as_ref(), &this.clone().into(), val, flags)
                        } else {
                            unreachable!()
                        };
                    }
                    JS_PROP_LAZYINIT => {
                        unsafe {
                            cur_obj
                                .0
                                .prop_values
                                .borrow_mut()
                                .get_unchecked_mut(idx)
                                .instantiate(&cur_obj, idx, &cur_obj)?;
                        }
                        continue 'shape;
                    }

                    JS_PROP_NORMAL | JS_PROP_VARREF => {
                        // in case proto != this
                        if prs_flags & JS_PROP_WRITABLE == 0 {
                            fail_read_only!(prop)
                        }
                        break;
                    }
                    _ => {
                        unreachable!();
                    }
                }
            }

            // 2. check exotic handlers
            let obj_flags = cur_obj.flags();
            if obj_flags.is_exotic() {
                if obj_flags.is_fast_array() {
                    if let Some(idx) = prop.to_small_int() {
                        if idx < cur_obj.fast_array_count() as u32 {
                            return if cur_obj.eq(this) {
                                this.set_property_value(ctx, &JSValue::Int(idx as _), val, flags)
                            } else {
                                break; // proto iter loop
                            };
                        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            // typed array oob
                            return Ok(true);
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                    {
                        let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
                        if ret < 0 {
                            return Err(-1);
                        } else if ret == 1 {
                            // typed array oob
                            return Ok(true);
                        }
                    }
                } else if let Some(xm) = unsafe {
                    ctx.rt().class_defs[cur_obj.class_id() as usize]
                        .exotic
                        .as_ref()
                } {
                    // apply exotic methods
                    if let Some(sp) = xm.set_property {
                        return sp(
                            ctx,
                            JSValue::Object(cur_obj.clone()),
                            prop,
                            val,
                            this.clone().into(),
                            flags as _,
                        );
                    }

                    if let Some(f) = xm.get_own_property {
                        if let Some(desc) = f(ctx, JSValue::Object(cur_obj.clone()), prop)? {
                            if desc.flags & JS_PROP_GETSET != 0 {
                                return call_setter(
                                    ctx,
                                    desc.setter.as_object(),
                                    &this.clone().into(),
                                    val,
                                    flags,
                                );
                            }

                            if desc.flags & JS_PROP_WRITABLE == 0 {
                                fail_read_only!(prop)
                            }

                            if cur_obj.eq(this) {
                                return this.define_property(
                                    ctx,
                                    prop,
                                    val,
                                    JSValue::UNDEFINED,
                                    JSValue::UNDEFINED,
                                    JS_PROP_HAS_VALUE,
                                );
                            }
                        }
                    }
                }
            }
        }

        if flags & JS_PROP_NO_ADD != 0 {
            JS_ThrowReferenceErrorNotDefined(ctx, prop);
            return Err(-1);
        }
        if !this.flags().extensible() {
            fail!("object is not extensible")
        }

        if this == self {
            if this.flags().is_exotic() {
                if let Some(idx) = prop.to_small_int()
                    && let Some(arr) = this.as_Array()
                    && arr.is_fast_array()
                // && arr.class_id() == JS_CLASS_ARRAY // should exclude JS_CLASS_ARGUMENTS?
                {
                    if idx == arr.fast_array_count() {
                        return arr.add_fast_array_element(ctx, val, flags);
                    }
                }
            } else {
                this.add_own_property(ctx, prop, JS_PROP_C_W_E, JSProperty::Normal(val))?;
                return Ok(true);
            }
        } else {
            // generic case: modify the property in `this` if it already exists
            if let Some(desc) = this.get_own_property(ctx, prop)? {
                if desc.flags & JS_PROP_GETSET != 0 {
                    fail!("setter is forbidden")
                } else {
                    if desc.flags & JS_PROP_WRITABLE == 0 || this.class_id() == JS_CLASS_MODULE_NS {
                        fail_read_only!(prop);
                    }

                    return this.define_property(
                        ctx,
                        prop,
                        val,
                        JSValue::UNDEFINED,
                        JSValue::UNDEFINED,
                        JS_PROP_HAS_VALUE,
                    );
                }
            }
        }

        // create property on `this`, returns Result<bool, i32>
        this.create_property(
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
        )
    }

    /// `prop` is not consumed.
    #[inline(always)]
    pub fn set_property(&self, ctx: &JSContext, prop: JSAtom, val: JSValue) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, None, JS_PROP_THROW)
    }

    /// flags can be JS_PROP_THROW or JS_PROP_THROW_STRICT
    pub fn set_property_value(
        &self,
        ctx: &JSContext,
        prop: &JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // check if prop is a u32
        let int_key = match prop {
            JSValue::Int(i) if *i >= 0 => Some(*i as u32),
            JSValue::Float64(f) if *f >= 0.0 => {
                if f.fract() == 0.0 {
                    let n = f.trunc() as i64;
                    if n <= u32::MAX as i64 {
                        Some(n as u32)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(idx) = int_key {
            let mut oob = false;

            // fast path: fast array
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    let ob_flags = self.flags();
                    if ob_flags.is_fast_array() && ob_flags.extensible() {
                        let idx = idx as usize;

                        enum State {
                            Append,
                            SlowPath,
                            Done,
                        }
                        let s = self
                            .with_fast_array_values_mut(|lst| -> Result<State, i32> {
                                match idx.cmp(&(lst.len())) {
                                    std::cmp::Ordering::Less => {
                                        // update existing list element
                                        unsafe {
                                            *lst.get_unchecked_mut(idx) = val.clone();
                                        }
                                        Ok(State::Done)
                                    }
                                    std::cmp::Ordering::Equal => {
                                        // add one element to list
                                        let mut use_slow_path = false;

                                        // find int prop through prototype chain.
                                        // Note: can't use ::prototype_iter() here, because of payload borrow conflict.
                                        let mut up = self.shape().unwrap().0.borrow().proto.clone();
                                        while let Some(p) = up {
                                            let sh1 = p.shape().unwrap();
                                            match p.class_id() {
                                                JS_CLASS_ARRAY => {
                                                    if !p.flags().is_fast_array() {
                                                        use_slow_path = true;
                                                        break;
                                                    }
                                                }
                                                JS_CLASS_OBJECT => {
                                                    if sh1.0.borrow().has_small_array_index {
                                                        use_slow_path = true;
                                                        break;
                                                    }
                                                }
                                                _ => {
                                                    use_slow_path = true;
                                                    break;
                                                }
                                            }
                                            up = sh1.0.borrow().proto.clone();
                                        }

                                        Ok(if !use_slow_path {
                                            State::Append
                                        } else {
                                            State::SlowPath
                                        })
                                    }
                                    std::cmp::Ordering::Greater => {
                                        // make a hold in list,
                                        // go slow path, destruct to sparse array
                                        Ok(State::SlowPath)
                                    }
                                }
                            })
                            .unwrap()?;

                        match s {
                            State::Append => {
                                return self.add_fast_array_element(ctx, val, flags);
                            }
                            State::Done => {
                                return Ok(true);
                            }
                            State::SlowPath => {}
                        }
                    }
                }
                JS_CLASS_ARGUMENTS => {
                    let idx = idx as usize;
                    if self
                        .with_fast_array_values_mut(|lst| -> bool {
                            if idx < lst.len() {
                                unsafe {
                                    *lst.get_unchecked_mut(idx) = val.clone();
                                }
                                true
                            } else {
                                false
                            }
                        })
                        .unwrap()
                    {
                        // early exit for fast array
                        return Ok(true);
                    }
                }

                //
                // typed array
                //
                JS_CLASS_UINT8C_ARRAY => {
                    let v = val.coerce_to_u8_clamp(ctx)?;
                    if Uint8Array(TypedArray(self.clone()))
                        .set_element(idx as _, v)
                        .is_err()
                    {
                        oob = true;
                    } else {
                        return Ok(true);
                    }
                }
                cid @ (JS_CLASS_INT8_ARRAY | JS_CLASS_UINT8_ARRAY) => {
                    let v = if let Ok(n) = val.to_i32(ctx) {
                        n as u8
                    } else {
                        return Err(-1);
                    };

                    let r = if cid == JS_CLASS_INT8_ARRAY {
                        Int8Array(TypedArray(self.clone())).set_element(idx as _, v as _)
                    } else {
                        Uint8Array(TypedArray(self.clone())).set_element(idx as _, v as _)
                    };

                    if r.is_err() {
                        oob = true;
                    } else {
                        return Ok(true);
                    }
                }
                cid @ (JS_CLASS_INT16_ARRAY | JS_CLASS_UINT16_ARRAY) => {
                    let v = if let Ok(n) = val.to_i32(ctx) {
                        n as u16
                    } else {
                        return Err(-1);
                    };

                    let r = if cid == JS_CLASS_INT16_ARRAY {
                        Int16Array(TypedArray(self.clone())).set_element(idx as _, v as _)
                    } else {
                        Uint16Array(TypedArray(self.clone())).set_element(idx as _, v as _)
                    };

                    if r.is_err() {
                        oob = true;
                    } else {
                        return Ok(true);
                    }
                }
                cid @ (JS_CLASS_INT32_ARRAY | JS_CLASS_UINT32_ARRAY) => {
                    let v = if let Ok(n) = val.to_i32(ctx) {
                        n as u32
                    } else {
                        return Err(-1);
                    };

                    let r = if cid == JS_CLASS_INT32_ARRAY {
                        Int32Array(TypedArray(self.clone())).set_element(idx as _, v as _)
                    } else {
                        Uint32Array(TypedArray(self.clone())).set_element(idx as _, v as _)
                    };

                    if r.is_err() {
                        oob = true;
                    } else {
                        return Ok(true);
                    }
                }
                JS_CLASS_FLOAT32_ARRAY => {
                    let d = val.to_f64(ctx)?;
                    let r = Float32Array(TypedArray(self.clone())).set_element(idx as _, d as _);

                    if r.is_err() {
                        oob = true;
                    } else {
                        return Ok(true);
                    }
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    let d = val.to_f64(ctx)?;
                    let r = Float64Array(TypedArray(self.clone())).set_element(idx as _, d);
                    if r.is_err() {
                        oob = true;
                    } else {
                        return Ok(true);
                    }
                }
                JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                    unimplemented!()
                }

                _ => {}
            }

            if oob {
                return Ok(true);
            }
        }

        // slow path
        let atom = prop.to_atom(ctx)?;
        let ret = self.set_property_internal(ctx, atom, val, None, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    #[inline(always)]
    pub fn set_property_u32(&self, ctx: &JSContext, idx: u32, val: JSValue) -> Result<bool, i32> {
        self.set_property_value(ctx, &JSValue::from(idx), val, JS_PROP_THROW)
    }

    #[inline]
    pub fn set_property_i64(&self, ctx: &JSContext, idx: i64, val: JSValue) -> Result<bool, i32> {
        if (0..=u32::MAX as i64).contains(&idx) {
            // fast path
            self.set_property_u32(ctx, idx as u32, val)
        } else {
            let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let r = self.set_property(ctx, atom, val);
            JS_FreeAtom(ctx, atom);
            r
        }
    }

    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.set_property(ctx, atom, val);
        JS_FreeAtom(ctx, atom);
        ret
    }
}

impl JSValue {
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// In case of `Reflect.set(obj, val, [receiver])`, `receiver` could be null, undefined or Some(object).
    /// For other cases,  `receiver` should be None, suggesting it's same to `self`.
    ///
    /// `prop` is not consumed.
    pub(crate) fn set_property_internal(
        ctx: &JSContext,
        obj: &JSValue,
        prop: JSAtom,
        val: JSValue,
        receiver: Option<&JSObject>,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(o) = obj {
            o.set_property_internal(ctx, prop, val, receiver, flags)
        } else if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, obj) {
            p.set_property_internal(ctx, prop, val, receiver, flags)
        } else {
            if ctx.throw_type_error_or_false(flags as _, "not an object") {
                Err(-1)
            } else {
                Ok(false)
            }
        }
    }

    /// `prop` is not consumed.
    #[inline(always)]
    pub fn set_property(&self, ctx: &JSContext, prop: JSAtom, val: JSValue) -> Result<bool, i32> {
        Self::set_property_internal(ctx, self, prop, val, None, JS_PROP_THROW)
    }

    #[inline]
    pub fn set_property_value(
        &self,
        ctx: &JSContext,
        prop: &JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(ob) = self {
            ob.set_property_value(ctx, prop, val, flags)
        } else {
            let atom = prop.to_atom(ctx)?;
            let ret = Self::set_property_internal(ctx, self, atom, val, None, flags);
            JS_FreeAtom(ctx, atom);
            ret
        }
    }

    #[inline(always)]
    pub fn set_property_u32(&self, ctx: &JSContext, idx: u32, val: JSValue) -> Result<bool, i32> {
        self.set_property_value(ctx, &JSValue::from(idx), val, JS_PROP_THROW)
    }

    #[inline]
    pub fn set_property_i64(&self, ctx: &JSContext, idx: i64, val: JSValue) -> Result<bool, i32> {
        if (0..u32::MAX as i64).contains(&idx) {
            // fast path
            self.set_property_u32(ctx, idx as u32, val)
        } else {
            let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let r = self.set_property(ctx, atom, val);
            JS_FreeAtom(ctx, atom);
            r
        }
    }

    #[inline]
    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.set_property(ctx, atom, val);
        JS_FreeAtom(ctx, atom);
        ret
    }
}

fn call_setter(
    ctx: &JSContext,
    setter: Option<&JSObject>,
    this_obj: &JSValue,
    val: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    if let Some(func) = setter {
        JSValue::Object(func.clone()).call(ctx, this_obj, std::slice::from_ref(&val))?;
        Ok(true)
    } else {
        if flags & JS_PROP_THROW != 0 || (flags & JS_PROP_THROW_STRICT != 0 && is_strict_mode(ctx))
        {
            ctx.throw_type_error("no setter for property");
            return Err(-1);
        }
        Ok(false)
    }
}
