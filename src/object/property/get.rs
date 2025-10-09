use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY,
    atom::{JS_ATOM_MAX_INT, JS_AtomIsNumericIndex, JS_FreeAtom, JSAtom, consts::*},
    branch_hints::unlikely,
    context::JSContext,
    error::{JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized, JSResult},
    object::{JSObject, JSProperty},
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
        for cur_obj in self.shape_proto_iter(true) {
            'redo: loop {
                // 1. search own property (in shape)
                if let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                    match prs_flags & JS_PROP_TMASK {
                        JS_PROP_NORMAL => {
                            return if let JSProperty::Normal(val) =
                                &cur_obj.0.prop_values.borrow()[idx]
                            {
                                debug_assert!(!val.is_exception());
                                Ok(val.clone())
                            } else {
                                #[cfg(debug_assertions)]
                                unreachable!();
                                #[cfg(not(debug_assertions))]
                                unsafe {
                                    std::hint::unreachable_unchecked();
                                }
                            };
                        }
                        JS_PROP_GETSET => {
                            let pr = cur_obj.0.prop_values.borrow()[idx].clone(); // borrow in short
                            return if let JSProperty::GetSet {
                                getter: Some(fun), ..
                            } = pr
                            {
                                fun.call(ctx, this_obj, &[])
                            } else {
                                Ok(JSValue::UNDEFINED)
                            };
                        }
                        JS_PROP_VARREF => {
                            let pr = cur_obj.0.prop_values.borrow()[idx].clone(); // borrow in short
                            let val = pr.value(ctx, this_obj)?;
                            return if unlikely(val.is_uninitialized()) {
                                JS_ThrowReferenceErrorUninitialized(ctx, prop);
                                Err(-1)
                            } else {
                                Ok(val)
                            };
                        }
                        JS_PROP_LAZYINIT => {
                            // instantiate lazy object, and redo
                            cur_obj.0.prop_values.borrow_mut()[idx]
                                .instantiate(&cur_obj, idx, self)?;
                            continue 'redo;
                        }
                        _ => unreachable!(),
                    }
                }

                break;
            }

            // 2. try fast array or exotic handler
            let ob_flags = cur_obj.flags();
            if ob_flags.is_exotic() {
                if ob_flags.is_fast_array() {
                    if let Some(idx) = prop.to_small_int() {
                        if idx < cur_obj.fast_array_count() as u32 {
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
                            return if unlikely(ret < 0) {
                                Err(-1)
                            } else {
                                Ok(JSValue::UNDEFINED)
                            };
                        }
                    }
                } else if let Some(m) = unsafe {
                    ctx.rt().class_defs[cur_obj.class_id() as usize]
                        .exotic
                        .as_ref()
                } {
                    if let Some(cb) = m.get_property {
                        // XXX: should pass throw_ref_error
                        return cb(ctx, cur_obj.clone().into(), prop, this_obj.clone());
                    }

                    if let Some(cb) = m.get_own_property {
                        if let Some(desc) = cb(ctx, JSValue::Object(cur_obj.clone()), prop)? {
                            return if unlikely(desc.flags & JS_PROP_GETSET != 0) {
                                desc.getter.call(ctx, this_obj, &[])
                            } else {
                                debug_assert!(!desc.value.is_exception());
                                Ok(desc.value)
                            };
                        }
                    }
                }
            }
        }

        if unlikely(throw_ref_error) {
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

    /// get property by index key.
    ///
    /// Returns:
    /// - Some: property value
    /// - None: try slow path instead
    fn get_property_index(&self, idx: usize) -> Option<JSResult> {
        macro_rules! typed_array_el {
            ($ty:ident, $vtype:ident) => {
                let ta = self.as_TypedArray().unwrap();
                return if idx < ta.length() {
                    Some(Ok(JSValue::from(
                        ta.with_elements_as::<$ty, _>(|lst| lst[idx] as $vtype),
                    )))
                } else {
                    None
                };
            };
        }

        match self.class_id() {
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                if let Some(el) = self
                    .with_fast_array_values(|lst| {
                        if idx < lst.len() {
                            Some(lst[idx as usize].clone()) // return element value
                        } else {
                            None // try slow path later
                        }
                    })
                    .flatten()
                {
                    return Some(Ok(el));
                }
            }

            //
            // typed array
            //
            JS_CLASS_INT8_ARRAY => {
                typed_array_el!(i8, i32);
            }
            JS_CLASS_UINT8C_ARRAY | JS_CLASS_UINT8_ARRAY => {
                typed_array_el!(u8, i32);
            }
            JS_CLASS_INT16_ARRAY => {
                typed_array_el!(i16, i32);
            }
            JS_CLASS_UINT16_ARRAY => {
                typed_array_el!(u16, i32);
            }
            JS_CLASS_INT32_ARRAY => {
                typed_array_el!(i32, i32);
            }
            JS_CLASS_UINT32_ARRAY => {
                typed_array_el!(u32, u32);
            }
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                unimplemented!()
            }
            JS_CLASS_FLOAT32_ARRAY => {
                typed_array_el!(f32, f64);
            }
            JS_CLASS_FLOAT64_ARRAY => {
                typed_array_el!(f64, f64);
            }

            _ => {}
        }

        None
    }

    fn get_property_slow(&self, ctx: &JSContext, prop: &JSValue) -> JSResult {
        let atom = prop.to_atom(ctx)?;
        let ret = self.get_property_internal(ctx, atom, &JSValue::Object(self.clone()), false);
        JS_FreeAtom(ctx, atom);
        ret
    }

    pub fn get_property_value(&self, ctx: &JSContext, prop: &JSValue) -> JSResult {
        // check if prop is an index
        let index_key = match prop {
            JSValue::Int(i) if *i >= 0 => Some(*i as usize),
            JSValue::Float64(f) if *f >= 0.0 => {
                if f.fract() == 0.0 {
                    let n = f.trunc() as i64;
                    if n <= u32::MAX as i64 {
                        Some(n as usize)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(idx) = index_key {
            if let Some(r) = self.get_property_index(idx) {
                return r;
            }
        }

        // slow path
        self.get_property_slow(ctx, prop)
    }

    pub fn get_property_u32(&self, ctx: &JSContext, idx: u32) -> JSResult {
        self.get_property_index(idx as usize)
            .unwrap_or_else(|| self.get_property_slow(ctx, &JSValue::from(idx)))
    }

    pub fn get_property_i64(&self, ctx: &JSContext, idx: i64) -> JSResult {
        if (0..u32::MAX as i64).contains(&idx) {
            self.get_property_u32(ctx, idx as u32)
        } else {
            let prop = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let r = self.get_property(ctx, prop);
            JS_FreeAtom(ctx, prop);
            r
        }
    }

    pub fn get_property_str(&self, ctx: &JSContext, prop: &str) -> JSResult {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.get_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        ret
    }

    /// Check if `self` has a generalized numeric property `idx`.
    /// If yes, get property value of the prop `idx`.
    /// Returns Err for exception, Ok(Some(_)) if property exists, Ok(None) if proprty does not exist.
    pub fn try_get_property_i64(&self, ctx: &JSContext, idx: i64) -> Result<Option<JSValue>, i32> {
        if (0..=JS_ATOM_MAX_INT as i64).contains(&idx) {
            // fast path
            if self.has_property(ctx, JSAtom::new_small_int(idx as u32))? {
                Ok(Some(
                    self.get_property_value(ctx, &JSValue::from(idx as u32))?,
                ))
            } else {
                Ok(None)
            }
        } else {
            let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let ret = self.has_property(ctx, atom).and_then(|found| {
                if found {
                    Ok(Some(self.get_property(ctx, atom)?))
                } else {
                    Ok(None)
                }
            });
            JS_FreeAtom(ctx, atom);
            ret
        }
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
                if unlikely(prop == JS_ATOM_length) {
                    // string length
                    Ok(JSValue::from(s.len() as u32))
                } else if let Some(idx) = prop.to_small_int() {
                    // code point at idx
                    if let Some(code) = s.content().get(idx as usize) {
                        ctx.create_string_from_char(code.into())
                            .map(JSValue::String)
                    } else {
                        Ok(JSValue::UNDEFINED)
                    }
                } else {
                    JS_GetPrototypePrimitive(ctx, self)
                        .as_object()
                        .map(|o| o.get_property_internal(ctx, prop, this_obj, throw_ref_error))
                        .unwrap_or(Ok(JSValue::UNDEFINED))
                }
            }
            v @ (Self::NULL | Self::UNDEFINED) => {
                ctx.throw_type_error(&format!(
                    "cannot read property '{}' of {}",
                    prop.to_utf8(ctx.rt()),
                    if v.is_undefined() {
                        "undefined"
                    } else {
                        "null"
                    }
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
                .as_object()
                .map(|o| o.get_property_internal(ctx, prop, this_obj, throw_ref_error))
                .unwrap_or(Ok(JSValue::UNDEFINED)),
        }
    }

    /// returns Exception when failed.
    #[inline(always)]
    pub fn get_property(&self, ctx: &JSContext, prop: JSAtom) -> JSResult {
        self.get_property_internal(ctx, prop, self, false)
    }

    pub fn get_property_value(&self, ctx: &JSContext, prop: &JSValue) -> JSResult {
        if let JSValue::Object(o) = self {
            o.get_property_value(ctx, prop)
        } else {
            let atom = prop.to_atom(ctx)?;
            let ret = self.get_property(ctx, atom);
            JS_FreeAtom(ctx, atom);
            ret
        }
    }

    /// returns Exception when fail.
    pub fn get_property_str(self, ctx: &JSContext, prop: &str) -> JSResult {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.get_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        ret
    }

    #[inline(always)]
    pub fn get_property_u32(&self, ctx: &JSContext, idx: u32) -> JSResult {
        self.get_property_value(ctx, &JSValue::from(idx))
    }

    pub fn get_property_i64(&self, ctx: &JSContext, idx: i64) -> JSResult {
        if (0..=u32::MAX as i64).contains(&idx) {
            self.get_property_u32(ctx, idx as u32)
        } else {
            let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let r = self.get_property(ctx, atom);
            JS_FreeAtom(ctx, atom);
            r
        }
    }

    /// Check if `self` has a generalized numeric property `idx`.
    /// If yes, get property value of the prop `idx`.
    /// Returns Err for exception, Ok(Some(_)) if property exists, Ok(None) if proprty does not exist.
    pub fn try_get_property_i64(&self, ctx: &JSContext, idx: i64) -> Result<Option<JSValue>, i32> {
        if (0..=JS_ATOM_MAX_INT as i64).contains(&idx) {
            // fast path
            if self.has_property(ctx, JSAtom::new_small_int(idx as u32))? {
                Ok(Some(
                    self.get_property_value(ctx, &JSValue::from(idx as u32))?,
                ))
            } else {
                Ok(None)
            }
        } else {
            let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
            let ret = self.has_property(ctx, atom).and_then(|found| {
                if found {
                    Ok(Some(self.get_property(ctx, atom)?))
                } else {
                    Ok(None)
                }
            });
            JS_FreeAtom(ctx, atom);
            ret
        }
    }
}
