use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::{JS_AtomIsNumericIndex, JS_FreeAtom, JSAtom},
    context::{JSContext, is_strict_mode},
    error::JS_ThrowReferenceErrorNotDefined,
    object::{
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT,
        JS_PROP_NORMAL, JS_PROP_THROW_STRICT, JS_PROP_TMASK, JSObject, JSProperty,
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

pub(crate) fn call_setter(
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

#[derive(Debug, Clone, PartialEq)]
enum SetPropState {
    /// walk up proto chain
    WalkUpProto,
    /// Add or update property
    Add,
    /// runtime has exception value
    Err,
    /// Read-only error
    ReadOnly,
    /// TypedArray out of bounds error
    TypedArrayOOB,
}

impl JSObject {
    /// Set indexed element value of a fast array.
    ///
    /// Returns:
    /// * true - the indexed element was updated.
    /// * false - the index does not exist.
    fn set_property_fast_array(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValue,
        flags: u32,
    ) -> Result<bool, SetPropState> {
        debug_assert!(self.flags().is_fast_array());

        let mut state = SetPropState::WalkUpProto;

        if let Some(idx) = prop.to_small_int() {
            if idx < self.fast_array_count() as u32 {
                if this_obj.object() == Some(self) {
                    return this_obj
                        .set_property_value(ctx, &JSValue::Int(idx as _), val, flags)
                        .map_err(|_| SetPropState::Err);
                } else {
                    state = SetPropState::Add;
                }
            } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&self.class_id()) {
                state = SetPropState::TypedArrayOOB;
            }
        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&self.class_id()) {
            let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
            if ret != 0 {
                if ret < 0 {
                    state = SetPropState::TypedArrayOOB;
                }
            }
        }

        Err(state)
    }

    /// set property for fast array or using exotic handler.
    fn set_property_exotic_or_fast_array(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: JSValue,
        flags: u32,
    ) -> Result<bool, SetPropState> {
        debug_assert!(self.flags().is_exotic());

        if self.flags().is_fast_array() {
            self.set_property_fast_array(ctx, prop, val, this_obj, flags)
        } else {
            // use exotic handler
            let mut next_state = SetPropState::WalkUpProto;

            if let Some(em) = unsafe {
                ctx.rt().class_defs[self.class_id() as usize]
                    .exotic
                    .as_ref()
            } {
                if let Some(cb) = em.set_property {
                    return cb(
                        ctx,
                        JSValue::Object(self.clone()),
                        prop,
                        val,
                        this_obj,
                        flags as _,
                    )
                    .map_err(|_| SetPropState::Err);
                }

                if let Some(cb) = em.get_own_property {
                    if let Some(desc) = cb(ctx, JSValue::Object(self.clone()), prop)
                        .map_err(|_| SetPropState::Err)?
                    {
                        if desc.flags & JS_PROP_GETSET != 0 {
                            return call_setter(ctx, desc.setter.object(), &this_obj, val, flags)
                                .map_err(|_| SetPropState::Err);
                        }

                        if desc.flags & JS_PROP_WRITABLE == 0 {
                            next_state = SetPropState::ReadOnly;
                        } else if this_obj.object() == Some(self) {
                            return self
                                .define_property(
                                    ctx,
                                    prop,
                                    val,
                                    JSValue::UNDEFINED,
                                    JSValue::UNDEFINED,
                                    JS_PROP_HAS_VALUE,
                                )
                                .map_err(|_| SetPropState::Err);
                        } else {
                            next_state = SetPropState::Add;
                        }
                    }
                }
            }

            Err(next_state)
        }
    }

    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// In most cases, self==this_obj, except in case of Reflect.set(target, prop, val, [[receiver]]),
    /// where `self` is _target_, and `this_obj` is _receiver_.
    ///
    /// `prop` is not consumed.
    pub(crate) fn set_property_internal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        this_obj: &JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        match this_obj {
            JSValue::Object(this) => {
                let mut update_own: Option<(usize, u32)> = None;

                for up in self.prototype_iter(ctx, true) {
                    let proto = up?;

                    // find in shape property
                    if let Some((idx, prs_flags)) = proto.find_shape_property(prop) {
                        match prs_flags & JS_PROP_TMASK {
                            JS_PROP_NORMAL => {
                                if proto.eq(this) {
                                    // should update own property of `this`
                                    update_own = Some((idx, prs_flags));
                                    break;
                                }
                            }
                            JS_PROP_LAZYINIT => {
                                if proto.eq(this) {
                                    // should update own property of `this`
                                    update_own = Some((idx, prs_flags));
                                    break;
                                }
                            }
                            JS_PROP_GETSET => {
                                // use setter
                                let gs = proto.0.prop_values.borrow()[idx].clone(); // avoid borrow conflict
                                if let JSProperty::GetSet { setter, .. } = gs {
                                    return call_setter(
                                        ctx,
                                        setter.as_ref(),
                                        &this.clone().into(),
                                        val.clone(),
                                        flags,
                                    );
                                } else {
                                    unreachable!()
                                }
                            }
                            _ => {
                                #[cfg(debug_assertions)]
                                unreachable!();
                            }
                        }
                    }

                    // try exotic or fast array
                    if proto.flags().is_exotic() {
                        match proto.set_property_exotic_or_fast_array(
                            ctx,
                            prop,
                            val.clone(),
                            this_obj.clone(),
                            flags,
                        ) {
                            Ok(b) => {
                                return Ok(b);
                            }
                            Err(SetPropState::WalkUpProto) => {
                                // for next prototype
                                continue;
                            }
                            Err(SetPropState::Add) => {
                                // stop proto iter. do add prop on `this`
                                break;
                            }
                            Err(SetPropState::Err) => {
                                // has exception
                                return Err(-1);
                            }
                            Err(SetPropState::ReadOnly) => {
                                return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }
                            Err(SetPropState::TypedArrayOOB) => {
                                // index overflow for typed array.
                                // ignore index oob, do nothing, returns true
                                return Ok(true);
                            }
                        }
                    }
                }

                // update own property of `this`
                if let Some((idx, prs_flags)) = update_own {
                    return this.put_prop_slot(ctx, prs_flags, prop, idx, val.clone(), this, flags);
                }

                if this.flags().is_exotic() {
                    match this.set_property_exotic_or_fast_array(
                        ctx,
                        prop,
                        val.clone(),
                        this_obj.clone(),
                        flags,
                    ) {
                        Ok(b) => {
                            return Ok(b);
                        }
                        Err(SetPropState::WalkUpProto) => {}
                        Err(SetPropState::Add) => {}
                        Err(SetPropState::Err) => {
                            // has exception
                            return Err(-1);
                        }
                        Err(SetPropState::ReadOnly) => {
                            return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
                                Err(-1)
                            } else {
                                Ok(false)
                            };
                        }
                        Err(SetPropState::TypedArrayOOB) => {
                            // index overflow for typed array.
                            // ignore index oob, do nothing, returns true
                            return Ok(true);
                        }
                    }
                }

                //
                // add new property on `this`
                //
                if flags & JS_PROP_NO_ADD != 0 {
                    JS_ThrowReferenceErrorNotDefined(ctx, prop);
                    return Err(-1);
                } else {
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
                    )?;
                    Ok(true)
                }
            }

            JSValue::EXCEPTION => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                Err(-1)
            }
            _ => {
                if ctx.throw_type_error_or_false(flags as _, "receiver must be an object") {
                    Err(-1)
                } else {
                    Ok(false)
                }
            }
        }
    }

    /// `prop` is not consumed.
    #[inline(always)]
    pub fn set_property(&self, ctx: &JSContext, prop: JSAtom, val: JSValue) -> Result<bool, i32> {
        self.set_property_internal(
            ctx,
            prop,
            val,
            &JSValue::Object(self.clone()),
            JS_PROP_THROW,
        )
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
                                        lst[idx] = val.clone();
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
                                lst[idx] = val.clone();
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
        let ret = self.set_property_internal(ctx, atom, val, &JSValue::Object(self.clone()), flags);
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
    /// **In most cases, obj==this_obj, except in case of Reflect.set(obj, val, [receiver]),
    /// where obj (must be an object), and this_obj is for receiver.**
    ///
    /// `prop` is not consumed.
    pub(crate) fn set_property_internal(
        ctx: &JSContext,
        obj: &JSValue,
        prop: JSAtom,
        val: JSValue,
        this_obj: &JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(o) = obj {
            o.set_property_internal(ctx, prop, val, this_obj, flags)
        } else if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, obj) {
            p.set_property_internal(ctx, prop, val, this_obj, flags)
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
        Self::set_property_internal(ctx, self, prop, val, self, JS_PROP_THROW)
    }

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
            let ret = Self::set_property_internal(ctx, self, atom, val, self, flags);
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

    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.set_property(ctx, atom, val);
        JS_FreeAtom(ctx, atom);
        ret
    }
}
