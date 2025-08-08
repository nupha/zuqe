use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY,
    atom::{
        __JS_AtomToUInt32, JS_ATOM_MAX_INT, JS_ATOM_NULL, JS_AtomIsNumericIndex, JS_FreeAtom,
        JSAtom, consts::*,
    },
    context::JSContext,
    error::{JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized, JSResult},
    function::JS_Call,
    object::{JSObject, JSProperty},
    string::StringBuf,
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{JS_PROP_GETSET, JS_PROP_LAZYINIT, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_VARREF};

impl JSObject {
    /// walk up proto chain and get property in shape or by exotic handler.
    /// `prop` is not consumed.
    /// Note: returned value is duplicated.
    pub(crate) fn get_property_internal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        this_obj: &JSValue,
        throw_ref_error: bool,
    ) -> JSResult {
        for p in self.prototype_iter(ctx, true) {
            let cur_obj = p?;

            // 1. try own property (in shape)
            if let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                return match prs_flags & JS_PROP_TMASK {
                    JS_PROP_NORMAL => {
                        if let JSProperty::Normal(val) = &cur_obj.0.prop_values.borrow()[idx] {
                            debug_assert!(!val.is_exception());

                            Ok(val.clone())
                        } else {
                            unreachable!()
                        }
                    }
                    JS_PROP_GETSET => {
                        let pr = cur_obj.0.prop_values.borrow()[idx].clone(); // borrow in short
                        pr.value(ctx, this_obj)
                    }
                    JS_PROP_VARREF => {
                        let pr = cur_obj.0.prop_values.borrow()[idx].clone(); // borrow in short
                        let val = pr.value(ctx, this_obj)?;
                        if val.is_uninitialized() {
                            JS_ThrowReferenceErrorUninitialized(ctx, prop);
                            Err(-1)
                        } else {
                            Ok(val)
                        }
                    }
                    JS_PROP_LAZYINIT => {
                        // Lazy instantiate property.
                        let mut pr = std::mem::take(&mut cur_obj.0.prop_values.borrow_mut()[idx]); // borrow in short
                        let val = pr.instantiate(&cur_obj, prop, self)?;
                        debug_assert!(matches!(pr, JSProperty::Normal(_)));
                        debug_assert_eq!(
                            cur_obj.find_shape_property(prop).unwrap().1 & JS_PROP_TMASK,
                            JS_PROP_NORMAL
                        );

                        // object instantiated, write value back
                        cur_obj.0.prop_values.borrow_mut()[idx] = pr;
                        Ok(val)
                    }
                    _ => unreachable!(),
                };
            }

            // 2. try fast array or exotic handler
            let ob_flags = cur_obj.flags();
            if ob_flags.is_exotic() {
                if ob_flags.is_fast_array() {
                    if prop.is_int_type() {
                        let idx = __JS_AtomToUInt32(prop);
                        if idx < cur_obj.array_length_unchecked() as u32 {
                            return cur_obj.get_property_value(ctx, &JSValue::from(idx));
                        }

                        if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            return Ok(JSValue::UNDEFINED);
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                    {
                        let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
                        if ret != 0 {
                            return if ret < 0 {
                                Err(-1)
                            } else {
                                Ok(JSValue::UNDEFINED)
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
                        )
                        .check_exception();
                    }
                    if let Some(cb) = xm.get_own_property {
                        if let Some(desc) = cb(ctx, JSValue::Object(cur_obj.clone()), prop)? {
                            return if desc.flags & JS_PROP_GETSET != 0 {
                                JS_Call(ctx, &desc.getter, this_obj, &[]).check_exception()
                            } else {
                                debug_assert!(!desc.value.is_exception());
                                Ok(desc.value)
                            };
                        }
                    }
                }
            }
        }

        if throw_ref_error {
            JS_ThrowReferenceErrorNotDefined(ctx, prop);
            Err(-1)
        } else {
            Ok(JSValue::UNDEFINED)
        }
    }

    /// get property by atom name.
    /// `prop` is not consumed.
    #[inline(always)]
    pub fn get_property(&self, ctx: &JSContext, prop: JSAtom) -> JSResult {
        self.get_property_internal(ctx, prop, &JSValue::Object(self.clone()), false)
    }

    /// returns EXCEPTION when fail
    pub fn get_property_value(&self, ctx: &JSContext, prop: &JSValue) -> JSResult {
        if let JSValue::Int(i) = prop
            && *i >= 0
        {
            // fast path: property is positive int, try fast array
            let nth = *i as usize;

            macro_rules! typed_array_elemnt {
                ($ty:ident, $vtype:ident) => {
                    return Ok(
                        self.0
                            .payload
                            .borrow()
                            .with_typed_array(|ta| {
                                if nth < ta.count as usize {
                                    let n = ta.with_bytes(|buf| unsafe {
                                        *buf.as_ptr().cast::<$ty>().add(nth)
                                    });
                                    JSValue::from(n as $vtype)
                                } else {
                                    JSValue::UNDEFINED
                                }
                            })
                            .unwrap(), // must be typed array object
                    );
                };
            }

            match self.class_id() {
                JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                    if let Some(el) = self
                        .with_fast_array_values(|lst| {
                            if nth < lst.len() {
                                Some(lst[nth as usize].clone()) // return element value
                            } else {
                                None // try slow path later
                            }
                        })
                        .flatten()
                    {
                        return Ok(el);
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

        // generic path
        {
            let atom = prop.coerce_to_atom(ctx)?;
            let ret = self.get_property(ctx, atom);
            JS_FreeAtom(ctx, atom);
            ret
        }
    }

    #[inline(always)]
    pub fn get_property_u32(&self, ctx: &JSContext, idx: u32) -> JSResult {
        self.get_property_value(ctx, &JSValue::from(idx))
    }

    pub fn get_property_i64(&self, ctx: &JSContext, idx: i64) -> JSResult {
        if (idx as u64) <= i32::MAX as u64 {
            self.get_property_value(ctx, &JSValue::Int(idx as i32))
        } else {
            let prop = ctx.rt_mut().register_atom_i64(idx);
            if prop == JS_ATOM_NULL {
                Err(-1)
            } else {
                let r = self.get_property(ctx, prop);
                JS_FreeAtom(ctx, prop);
                r
            }
        }
    }

    pub fn get_property_str(&self, ctx: &JSContext, prop: &str) -> JSResult {
        let atom = ctx.create_atom(prop)?;
        let ret = self.get_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        ret
    }
}

impl JSValue {
    /// get property `prop` of `self` value.
    pub(crate) fn get_property_internal(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        this_obj: &JSValue,
        throw_ref_error: bool,
    ) -> JSResult {
        match self {
            Self::Object(o) => o.get_property_internal(ctx, prop, this_obj, throw_ref_error),
            Self::String(s) => {
                if prop == JS_ATOM_length {
                    // string length
                    Ok(JSValue::from(s.buf_len() as u32))
                } else if prop.is_int_type() {
                    // get code at index
                    let idx = __JS_AtomToUInt32(prop);
                    if let Some(code) = s.content().get(idx as usize) {
                        ctx.create_string_from_content(if (0..=255).contains(&code) {
                            StringBuf::Ascii(vec![code as u8])
                        } else {
                            StringBuf::Utf16(vec![code])
                        })
                        .map(JSValue::String)
                    } else {
                        Ok(JSValue::UNDEFINED)
                    }
                } else {
                    JS_GetPrototypePrimitive(ctx, self)
                        .object()
                        .map(|o| o.get_property_internal(ctx, prop, this_obj, throw_ref_error))
                        .unwrap_or(Ok(JSValue::UNDEFINED))
                }
            }
            Self::NULL | Self::UNDEFINED => {
                ctx.throw_type_error(&format!(
                    "cannot read property '{}' of {}",
                    prop.to_utf8(ctx.rt()),
                    self
                ));
                Err(-1)
            }
            Self::EXCEPTION => {
                #[cfg(debug_assertions)]
                unreachable!();
                #[cfg(not(debug_assertions))]
                Err(-1)
            }
            _ => JS_GetPrototypePrimitive(ctx, self)
                .object()
                .map(|o| o.get_property_internal(ctx, prop, this_obj, throw_ref_error))
                .unwrap_or(Ok(JSValue::UNDEFINED)),
        }
    }

    /// returns Exception when failed.
    #[inline(always)]
    pub fn get_property(&self, ctx: &JSContext, prop: JSAtom) -> JSResult {
        self.get_property_internal(ctx, prop, self, false)
    }

    /// returns Exception when fail.
    pub fn get_property_str(self, ctx: &JSContext, prop: &str) -> JSResult {
        let atom = ctx.create_atom(prop)?;
        let ret = self.get_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        ret
    }

    pub fn get_property_value(&self, ctx: &JSContext, prop: &JSValue) -> JSResult {
        if let JSValue::Object(o) = self {
            o.get_property_value(ctx, prop)
        } else {
            let atom = prop.coerce_to_atom(ctx)?;
            let ret = self.get_property(ctx, atom);
            JS_FreeAtom(ctx, atom);
            ret
        }
    }

    #[inline(always)]
    pub fn get_property_u32(&self, ctx: &JSContext, idx: u32) -> JSResult {
        self.get_property_value(ctx, &JSValue::from(idx))
    }

    pub fn get_property_i64(&self, ctx: &JSContext, idx: i64) -> JSResult {
        if (idx as u64) <= i32::MAX as u64 {
            self.get_property_value(ctx, &JSValue::Int(idx as i32))
        } else {
            let atom = ctx.rt_mut().register_atom_i64(idx);
            if atom == JS_ATOM_NULL {
                Err(-1)
            } else {
                let r = self.get_property(ctx, atom);
                JS_FreeAtom(ctx, atom);
                r
            }
        }
    }

    /// Check if an object has a generalized numeric property `idx`.
    /// Returns Err for exception, Ok(Some(_)) if property exists, Ok(None) if proprty does not exist.
    pub fn try_get_property_i64(&self, ctx: &JSContext, idx: i64) -> Result<Option<JSValue>, i32> {
        if idx as u64 <= JS_ATOM_MAX_INT as u64 {
            // fast path
            if self.has_property(ctx, JSAtom::new_u32(idx as u32))? {
                Ok(Some(
                    self.get_property_value(ctx, &JSValue::Int(idx as i32))?,
                ))
            } else {
                Ok(None)
            }
        } else {
            let prop = ctx.rt_mut().register_atom_i64(idx);
            let ret = if prop != JS_ATOM_NULL {
                self.has_property(ctx, prop).and_then(|b| {
                    if b {
                        Ok(Some(
                            self.get_property_value(ctx, &JSValue::Int(idx as i32))?,
                        ))
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
