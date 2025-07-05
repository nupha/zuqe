use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY,
    atom::{
        __JS_AtomToUInt32, JS_ATOM_MAX_INT, JS_ATOM_NULL, JS_AtomIsNumericIndex, JS_FreeAtom,
        JS_ValueToAtom, JSAtom, consts::*,
    },
    context::JSContext,
    error::{JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized},
    function::JS_Call,
    object::{JSObject, JSProperty},
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{JS_PROP_AUTOINIT, JS_PROP_GETSET, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_VARREF};

impl JSObject {
    /// walk up proto chain and get property value by shape or exotic handler.
    /// Note: returned value is duplicated.
    pub(crate) fn get_property_from_shape_or_exotic(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        this_obj: &JSValue,
        throw_ref_error: bool,
    ) -> JSValue {
        let mut cur_obj = self.clone();

        loop {
            // 1. find own property in shape
            if let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                return match prs_flags & JS_PROP_TMASK {
                    JS_PROP_NORMAL => {
                        if let JSProperty::Normal(val) = &cur_obj.0.prop_values.borrow()[idx] {
                            val.clone()
                        } else {
                            unreachable!()
                        }
                    }
                    JS_PROP_GETSET => {
                        let pr = cur_obj.0.prop_values.borrow()[idx].clone();
                        pr.value(ctx, this_obj)
                    }
                    JS_PROP_VARREF => {
                        let pr = cur_obj.0.prop_values.borrow()[idx].clone();
                        let val = pr.value(ctx, this_obj);
                        if val.is_uninitialized() {
                            JS_ThrowReferenceErrorUninitialized(ctx, prop);
                            JSValue::EXCEPTION
                        } else {
                            val
                        }
                    }
                    JS_PROP_AUTOINIT => {
                        // Lazy instantiate property.
                        let mut pr = std::mem::take(&mut cur_obj.0.prop_values.borrow_mut()[idx]);
                        let val = pr
                            .instantiate(&cur_obj, prop, self)
                            .unwrap_or(JSValue::EXCEPTION);
                        debug_assert!(matches!(pr, JSProperty::Normal(_)));
                        debug_assert_eq!(
                            cur_obj.find_shape_property(prop).unwrap().1 & JS_PROP_TMASK,
                            JS_PROP_NORMAL
                        );
                        cur_obj.0.prop_values.borrow_mut()[idx] = pr; // instantiated, write value back

                        val
                    }
                    _ => unreachable!(),
                };
            }

            // 2. try array or exotic handler
            let ob_flags = cur_obj.flags();
            if ob_flags.is_exotic() {
                if ob_flags.fast_array() {
                    if prop.is_int_type() {
                        let idx = __JS_AtomToUInt32(prop);
                        if idx < cur_obj.array_length_unchecked() as u32 {
                            return cur_obj.get_property_value(ctx, &JSValue::from(idx));
                        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            return JSValue::UNDEFINED;
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                    {
                        let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
                        if ret != 0 {
                            return if ret < 0 {
                                JSValue::EXCEPTION
                            } else {
                                JSValue::UNDEFINED
                            };
                        }
                    }
                } else if let Some(xm) = unsafe {
                    ctx.rt().class_defs[cur_obj.class_id() as usize]
                        .exotic
                        .as_ref()
                } {
                    // use exotic handlers
                    if let Some(cb) = xm.get_property {
                        /* XXX: should pass throw_ref_error */
                        /* Note: if 'p' is a prototype, it can be
                        freed in the called function */
                        return cb(
                            ctx,
                            JSValue::Object(cur_obj.clone()),
                            prop,
                            this_obj.clone(),
                        );
                    }
                    if let Some(cb) = xm.get_own_property {
                        match cb(ctx, JSValue::Object(cur_obj.clone()), prop) {
                            Ok(Some(desc)) => {
                                return if desc.flags & JS_PROP_GETSET != 0 {
                                    JS_Call(ctx, &desc.getter, this_obj, &[])
                                } else {
                                    desc.value
                                };
                            }
                            Ok(None) => {}
                            Err(_) => {
                                return JSValue::EXCEPTION;
                            }
                        }
                    }
                }
            }

            // 3. walk up proto chain
            if let Some(up) = cur_obj.shape().and_then(|s| s.0.borrow().proto.clone()) {
                cur_obj = up;
            } else {
                break;
            }
        }

        if throw_ref_error {
            JS_ThrowReferenceErrorNotDefined(ctx, prop);
            JSValue::EXCEPTION
        } else {
            JSValue::UNDEFINED
        }
    }

    /// get property with atom name.
    /// returns EXCEPTION when fail
    #[inline(always)]
    pub fn get_property(&self, ctx: &JSContext, prop: JSAtom) -> JSValue {
        self.get_property_from_shape_or_exotic(ctx, prop, &JSValue::Object(self.clone()), false)
    }

    /// returns EXCEPTION when fail
    pub fn get_property_value(&self, ctx: &JSContext, prop: &JSValue) -> JSValue {
        // check fast path: fast array and index within slice bound.
        if prop.is_int() {
            let idx = if let JSValue::Int(i) = prop {
                *i
            } else {
                unreachable!()
            };
            debug_assert!(idx >= 0);
            let idx = idx as usize;

            macro_rules! typed_array_elemnt {
                ($ty:ident, $vtype:ident) => {
                    if let Some(v) = self
                        .0
                        .isa
                        .borrow()
                        .with_typed_array(|ta| {
                            if idx < ta.count as usize {
                                let n = ta.with_bytes(|buf| unsafe {
                                    *buf.as_ptr().cast::<$ty>().add(idx)
                                });
                                Some(JSValue::from(n as $vtype))
                            } else {
                                None
                            }
                        })
                        .unwrap()
                    {
                        return v;
                    }
                };
            }

            match self.class_id() {
                JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                    if let Some(r) = self
                        .with_fast_array_values(|lst| {
                            if idx < lst.len() {
                                return Some(lst[idx as usize].clone()); // use fast array value
                            }
                            None
                        })
                        .flatten()
                    {
                        return r;
                    }
                }

                //
                // typed array
                //
                JS_CLASS_INT8_ARRAY => {
                    typed_array_elemnt!(i8, i32);
                }
                JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                    typed_array_elemnt!(u8, i32);
                }
                JS_CLASS_INT16_ARRAY => {
                    typed_array_elemnt!(i16, i32);
                }
                JS_CLASS_UINT16_ARRAY => {
                    typed_array_elemnt!(u16, i32);
                }
                JS_CLASS_INT32_ARRAY => {
                    typed_array_elemnt!(i32, i32);
                }
                JS_CLASS_UINT32_ARRAY => {
                    typed_array_elemnt!(u32, u32);
                }
                JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                    unimplemented!()
                }
                JS_CLASS_FLOAT32_ARRAY => {
                    typed_array_elemnt!(f32, f64);
                }
                JS_CLASS_FLOAT64_ARRAY => {
                    typed_array_elemnt!(f64, f64);
                }
                _ => {}
            }
        }

        // slow path
        {
            let atom = JS_ValueToAtom(ctx, prop);
            if atom == JS_ATOM_NULL {
                JSValue::EXCEPTION
            } else {
                let ret = self.get_property(ctx, atom);
                JS_FreeAtom(ctx, atom);
                ret
            }
        }
    }

    #[inline(always)]
    pub fn get_property_u32(&self, ctx: &JSContext, idx: u32) -> JSValue {
        self.get_property_value(ctx, &JSValue::from(idx))
    }

    /// returns Exception when fail.
    pub fn get_property_str(&self, ctx: &JSContext, prop: &str) -> JSValue {
        if let Ok(atom) = ctx.create_atom(prop) {
            let ret = self.get_property(ctx, atom);
            JS_FreeAtom(ctx, atom);
            ret
        } else {
            JSValue::EXCEPTION
        }
    }
}

impl JSValue {
    pub(crate) fn get_property_internal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        this_obj: &JSValue,
        throw_ref_error: bool,
    ) -> JSValue {
        match self {
            Self::Object(o) => {
                o.get_property_from_shape_or_exotic(ctx, prop, this_obj, throw_ref_error)
            }
            Self::NULL => {
                ctx.JS_ThrowTypeError(&format!(
                    "cannot read property '{}' of null",
                    prop.to_str(ctx.rt())
                ));
                JSValue::EXCEPTION
            }
            Self::UNDEFINED => {
                ctx.JS_ThrowTypeError(&format!(
                    "cannot read property '{}' of undefined",
                    prop.to_str(ctx.rt())
                ));
                JSValue::EXCEPTION
            }
            Self::EXCEPTION => JSValue::EXCEPTION,
            Self::String(ss) => {
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < ss.len() as u32 {
                        let ch = ss.content().chars().nth(idx as usize).unwrap();
                        return ctx.create_string_from_char(ch);
                    }
                } else if prop == JS_ATOM_length {
                    return JSValue::Int(ss.len() as i32);
                }

                unsafe {
                    JS_GetPrototypePrimitive(ctx, self)
                        .object()
                        .map(|o| {
                            o.get_property_from_shape_or_exotic(
                                ctx,
                                prop,
                                this_obj,
                                throw_ref_error,
                            )
                        })
                        .unwrap_or(JSValue::UNDEFINED)
                }
            }
            _ => unsafe {
                JS_GetPrototypePrimitive(ctx, self)
                    .object()
                    .map(|o| {
                        o.get_property_from_shape_or_exotic(ctx, prop, this_obj, throw_ref_error)
                    })
                    .unwrap_or(JSValue::UNDEFINED)
            },
        }
    }

    /// returns Exception when fail.
    #[inline(always)]
    pub fn get_property(&self, ctx: &JSContext, prop: JSAtom) -> JSValue {
        self.get_property_internal(ctx, prop, self, false)
    }

    /// returns Exception when fail.
    pub fn get_property_str(self, ctx: &JSContext, prop: &str) -> JSValue {
        if let Ok(atom) = ctx.create_atom(prop) {
            let ret = self.get_property(ctx, atom);
            JS_FreeAtom(ctx, atom);
            ret
        } else {
            JSValue::EXCEPTION
        }
    }

    pub fn get_property_value(&self, ctx: &JSContext, prop: &JSValue) -> JSValue {
        if self.is_object() {
            unsafe { self.object_unchecked().get_property_value(ctx, prop) }
        } else {
            let atom = JS_ValueToAtom(ctx, prop);
            if atom == JS_ATOM_NULL {
                JSValue::EXCEPTION
            } else {
                let ret = self.get_property(ctx, atom);
                JS_FreeAtom(ctx, atom);
                ret
            }
        }
    }

    #[inline(always)]
    pub fn get_property_u32(&self, ctx: &JSContext, idx: u32) -> JSValue {
        self.get_property_value(ctx, &JSValue::from_u32(idx))
    }

    pub fn get_property_i64(&self, ctx: &JSContext, idx: i64) -> JSValue {
        if (idx as u64) <= i32::MAX as u64 {
            self.get_property_value(ctx, &JSValue::Int(idx as i32))
        } else {
            let prop = ctx.rt_mut().register_atom_i64(idx);
            if prop == JS_ATOM_NULL {
                JSValue::EXCEPTION
            } else {
                self.get_property(ctx, prop)
            }
        }
    }

    /// Check if an object has a generalized numeric property `idx`.
    /// Returns Err for exception, Ok(Some(_)) if property exists, Ok(None) if proprty does not exist.
    pub fn try_get_property_i64(&self, ctx: &JSContext, idx: i64) -> Result<Option<JSValue>, i32> {
        if idx as u64 <= JS_ATOM_MAX_INT as u64 {
            // fast path
            if self.has_property(ctx, JSAtom::new_u32(idx as u32))? {
                let val = self
                    .get_property_value(ctx, &JSValue::Int(idx as i32))
                    .check_exception()?;
                Ok(Some(val))
            } else {
                Ok(None)
            }
        } else {
            let prop = ctx.rt_mut().register_atom_i64(idx);
            let ret = if prop != JS_ATOM_NULL {
                self.has_property(ctx, prop).and_then(|b| {
                    if b {
                        let val = self
                            .get_property_value(ctx, &JSValue::from_i32(idx as i32))
                            .check_exception()?;
                        Ok(Some(val))
                    } else {
                        Ok(None) // does not exist
                    }
                })
            } else {
                Err(-1) // can't create prop atom
            };

            JS_FreeAtom(ctx, prop);
            ret
        }
    }
}
