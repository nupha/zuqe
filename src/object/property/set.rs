use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::{__JS_AtomToUInt32, JS_ATOM_NULL, JS_AtomIsNumericIndex, JS_FreeAtom, JSAtom},
    context::{JSContext, is_strict_mode},
    error::JS_ThrowReferenceErrorNotDefined,
    function::JS_Call,
    object::{
        JS_PROP_LAZYINIT, JS_PROP_NORMAL, JS_PROP_THROW_STRICT, JS_PROP_TMASK, JSObject, JSProperty,
    },
    typed_array::{
        Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, TypedArray, Uint8Array,
        Uint16Array, Uint32Array,
    },
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{
    JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE,
    JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_WRITABLE,
};

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
    /// set property for fast array.
    /// returns false means property should be added.
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

        if prop.is_int_type() {
            let idx = __JS_AtomToUInt32(prop);
            if idx < self.array_length_unchecked() as u32 {
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
    fn set_property_exotic(
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

                for p in self.prototype_iter(ctx, true) {
                    let proto = p?;

                    // find own property
                    if let Some((idx, prs_flags)) = proto.find_shape_property(prop) {
                        match prs_flags & JS_PROP_TMASK {
                            JS_PROP_GETSET => {
                                // use setter
                                return this.put_prop_slot(
                                    ctx,
                                    prs_flags,
                                    prop,
                                    idx,
                                    val.clone(),
                                    this,
                                    flags,
                                );
                            }
                            JS_PROP_NORMAL | JS_PROP_LAZYINIT => {
                                if proto.eq(this) {
                                    // should update own property of `this`
                                    update_own = Some((idx, prs_flags));
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }

                    // try exotic or fast array
                    if proto.flags().is_exotic() {
                        match proto.set_property_exotic(
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

                // update own property
                if let Some((idx, prs_flags)) = update_own {
                    return this.put_prop_slot(ctx, prs_flags, prop, idx, val.clone(), this, flags);
                }

                if this.flags().is_exotic() {
                    match this.set_property_exotic(ctx, prop, val.clone(), this_obj.clone(), flags)
                    {
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

                // add new property
                if flags & JS_PROP_NO_ADD != 0 {
                    JS_ThrowReferenceErrorNotDefined(ctx, prop);
                    return Err(-1);
                } else if !this.flags().extensible() {
                    return if ctx.throw_type_error_or_false(flags as _, "object is not extensible")
                    {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                this.add_own_property(ctx, prop, JS_PROP_C_W_E, JSProperty::Normal(val))?;
                Ok(true)
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
        if let JSValue::Int(idx) = prop {
            let idx = *idx;
            let mut ta_out_of_bound = false;

            // try fast path: fast array
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    // fast path: fast array
                    let ob_flags = self.flags();
                    if ob_flags.is_fast_array() && ob_flags.extensible() {
                        let r = self
                            .with_fast_array_values_mut(|lst| -> Result<Option<bool>, i32> {
                                match idx.cmp(&(lst.len() as i32)) {
                                    std::cmp::Ordering::Less => {
                                        // update array element
                                        lst[idx as usize] = val.clone();
                                        return Ok(Some(true));
                                    }
                                    std::cmp::Ordering::Equal => {
                                        // find int prop through prototype chain
                                        let mut use_slow_path = false;
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

                                        if !use_slow_path {
                                            // append to array
                                            lst.try_reserve_exact(1).map_err(|_| {
                                                ctx.throw_out_of_memory();
                                                -1
                                            })?;
                                            lst.push(val.clone());

                                            // update prop length
                                            self.0.prop_values.borrow_mut()[0] =
                                                JSProperty::Normal(JSValue::Int(lst.len() as i32));

                                            return Ok(Some(true));
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
                                // leave to slow array
                            }
                        }
                    }
                }
                JS_CLASS_ARGUMENTS => {
                    let done = self
                        .with_fast_array_values_mut(|lst| -> bool {
                            if idx < lst.len() as i32 {
                                lst[idx as usize] = val.clone();
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
                    let v = val.coerce_to_u8_clamp(ctx)?;
                    if Uint8Array(TypedArray(self.clone()))
                        .set_element(idx as _, v)
                        .is_err()
                    {
                        ta_out_of_bound = true;
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
                        ta_out_of_bound = true;
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
                        ta_out_of_bound = true;
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
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }
                }
                JS_CLASS_FLOAT32_ARRAY => {
                    let d = val.coerce_to_f64(ctx)?;
                    let r = Float32Array(TypedArray(self.clone())).set_element(idx as _, d as _);

                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    let d = val.coerce_to_f64(ctx)?;
                    let r = Float64Array(TypedArray(self.clone())).set_element(idx as _, d);
                    if r.is_err() {
                        ta_out_of_bound = true;
                    } else {
                        return Ok(true);
                    }
                }
                JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                    unimplemented!()
                }

                _ => {
                    // go slow path
                }
            }

            if ta_out_of_bound {
                return Ok(true);
            }
        }

        // slow_path
        let atom = prop.coerce_to_atom(ctx)?;
        let ret = self.set_property_internal(ctx, atom, val, &JSValue::Object(self.clone()), flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    #[inline(always)]
    pub fn set_property_u32(&self, ctx: &JSContext, idx: u32, val: JSValue) -> Result<bool, i32> {
        self.set_property_value(ctx, &JSValue::from(idx), val, JS_PROP_THROW)
    }

    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
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
        } else if let JSValue::Object(p) = unsafe { JS_GetPrototypePrimitive(ctx, obj) } {
            p.set_property_internal(ctx, prop, val, this_obj, flags)
        } else {
            if ctx.throw_type_error_or_false(flags as _, "not an object") {
                Err(-1)
            } else {
                Ok(false)
            }
        }

        // let mut ob_obj: Option<JSObject>;
        // let ob_this: Option<&JSObject>;

        // // pre check
        // let mut state = match (obj, this_obj) {
        //     (JSValue::Object(o), JSValue::Object(this)) => {
        //         ob_obj = Some(o.clone());
        //         ob_this = Some(this);

        //         if o == this {
        //             if let Some((idx, prs_flags)) = o.find_shape_property(prop) {
        //                 return o.put_prop_slot(ctx, prs_flags, prop, idx, val.clone(), o, flags);
        //             }
        //             SetPropState::ExoticAndGoUp
        //         } else {
        //             SetPropState::SetSlotValue
        //         }
        //     }
        //     (JSValue::UNDEFINED, _) | (JSValue::NULL, _) => {
        //         ctx.JS_ThrowTypeError(&format!(
        //             "cannot set property '{}' of non-object",
        //             prop.to_str(ctx.rt())
        //         ));
        //         return Err(-1);
        //     }
        //     _ => {
        //         // even on a primitive type we can have setters on the prototype
        //         ob_this = None;
        //         if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, obj) {
        //             ob_obj = Some(p);
        //             SetPropState::SetSlotValue
        //         } else {
        //             ob_obj = None;
        //             SetPropState::Add
        //         }
        //     }
        // };

        // // process states
        // loop {
        //     match state {
        //         SetPropState::ExoticAndGoUp => {
        //             let target = ob_obj.clone().unwrap();

        //             if target.flags().is_exotic() {
        //                 match target.set_property_exotic(
        //                     ctx,
        //                     prop,
        //                     val.clone(),
        //                     this_obj.clone(),
        //                     flags,
        //                 ) {
        //                     Ok(b) => {
        //                         return Ok(b);
        //                     }
        //                     Err(None) => {
        //                         return Err(-1);
        //                     }
        //                     Err(Some(next_state)) => {
        //                         state = next_state;
        //                     }
        //                 }
        //             } // is_exotic()

        //             if SetPropState::ExoticAndGoUp == state {
        //                 // walk up proto chain
        //                 state = if let Some(up) = target.shape().unwrap().0.borrow().proto.clone() {
        //                     ob_obj = Some(up);
        //                     SetPropState::SetSlotValue
        //                 } else {
        //                     ob_obj = None;
        //                     SetPropState::Add
        //                 };
        //             }

        //             continue;
        //         }
        //         SetPropState::SetSlotValue => {
        //             let target = ob_obj.as_ref().unwrap();
        //             if let Some((idx, prs_flags)) = target.find_shape_property(prop) {
        //                 return target.put_prop_slot(
        //                     ctx,
        //                     prs_flags,
        //                     prop,
        //                     idx,
        //                     val.clone(),
        //                     ob_this.unwrap(),
        //                     flags,
        //                 );
        //             }

        //             state = SetPropState::ExoticAndGoUp;
        //             continue;
        //         }

        //         SetPropState::Add => {
        //             // to add or update a property on `this_obj`
        //             if flags & JS_PROP_NO_ADD != 0 {
        //                 JS_ThrowReferenceErrorNotDefined(ctx, prop);
        //                 return Err(-1);
        //             }
        //             if ob_this.is_none() {
        //                 return if ctx.JS_ThrowTypeErrorOrFalse(flags as _, "not an object") {
        //                     Err(-1)
        //                 } else {
        //                     Ok(false)
        //                 };
        //             }

        //             let target = ob_this.unwrap();

        //             if !target.flags().extensible() {
        //                 return if ctx
        //                     .JS_ThrowTypeErrorOrFalse(flags as _, "object is not extensible")
        //                 {
        //                     Err(-1)
        //                 } else {
        //                     Ok(false)
        //                 };
        //             }

        //             if ob_this == ob_obj.as_ref() {
        //                 if target.flags().is_exotic() {
        //                     if target.class_id() == JS_CLASS_ARRAY
        //                         && target.flags().fast_array()
        //                         && prop.is_int_type()
        //                     {
        //                         // fast array
        //                         let idx = __JS_AtomToUInt32(prop);
        //                         if idx == target.array_length_unchecked() as u32 {
        //                             return target.add_fast_array_element(ctx, val, flags);
        //                         }
        //                     }

        //                     state = SetPropState::CreateProp;
        //                     continue;
        //                 } else {
        //                     target.add_shape_property_then(ctx, prop, JS_PROP_C_W_E, |pr| {
        //                         *pr = JSProperty::Normal(val);
        //                     })?;
        //                     return Ok(true);
        //                 }
        //             } else {
        //                 // generic case: modify the property in this_obj if it already exists
        //                 match target.get_own_property(ctx, prop)? {
        //                     Some(desc) => {
        //                         // already exists, update the value
        //                         if desc.flags & JS_PROP_GETSET != 0 {
        //                             return if ctx
        //                                 .JS_ThrowTypeErrorOrFalse(flags as _, "setter is forbidden")
        //                             {
        //                                 Err(-1)
        //                             } else {
        //                                 Ok(false)
        //                             };
        //                         } else if desc.flags & JS_PROP_WRITABLE == 0
        //                             || target.class_id() == JS_CLASS_MODULE_NS
        //                         {
        //                             state = SetPropState::ErrReadOnly;
        //                             continue;
        //                         }

        //                         return this_obj.define_property(
        //                             ctx,
        //                             prop,
        //                             &val,
        //                             &JSValue::UNDEFINED,
        //                             &JSValue::UNDEFINED,
        //                             JS_PROP_HAS_VALUE,
        //                         );
        //                     }
        //                     None => {
        //                         // not exist, should create
        //                         state = SetPropState::CreateProp;
        //                         continue;
        //                     }
        //                 }
        //             }
        //         }
        //         SetPropState::CreateProp => {
        //             // create property on this_obj
        //             return ob_this.unwrap().create_property(
        //                 ctx,
        //                 prop,
        //                 val,
        //                 JSValue::UNDEFINED,
        //                 JSValue::UNDEFINED,
        //                 flags
        //                     | JS_PROP_HAS_VALUE
        //                     | JS_PROP_HAS_ENUMERABLE
        //                     | JS_PROP_HAS_WRITABLE
        //                     | JS_PROP_HAS_CONFIGURABLE
        //                     | JS_PROP_C_W_E,
        //             );
        //         }

        //         SetPropState::ErrReadOnly => {
        //             return if ctx.JS_ThrowTypeErrorReadOnly(flags, prop) {
        //                 Err(-1)
        //             } else {
        //                 Ok(false)
        //             };
        //         }
        //         SetPropState::ErrTypedArrayOOB(class_id) => {
        //             // must convert the argument even if out of bound access
        //             if matches!(
        //                 class_id,
        //                 JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY
        //             ) {
        //                 unimplemented!()
        //                 // let mut v:i64;
        //                 // if JS_ToBigInt64Free(ctx, &v, val)!=0{
        //                 //     return -1;
        //                 // }
        //             } else {
        //                 let v = val.coerce_to_number_free(ctx)?; // consume val
        //             }
        //             return Ok(true);
        //         }
        //     }
        // }
    }

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
            let atom = prop.coerce_to_atom(ctx)?;
            let ret = Self::set_property_internal(ctx, self, atom, val, self, flags);
            JS_FreeAtom(ctx, atom);
            ret
        }
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

    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = ctx.create_atom(prop)?;
        let ret = self.set_property(ctx, atom, val);
        JS_FreeAtom(ctx, atom);
        ret
    }
}
