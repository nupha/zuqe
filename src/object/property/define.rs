use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    array::JS_ToArrayLengthFree,
    atom::{JS_ATOM_NULL, JS_AtomIsNumericIndex1, JS_FreeAtom, JSAtom, consts::*},
    bytecode::js_get_function_name,
    context::JSContext,
    number::JS_NumberIsNegativeOrMinusZero,
    object::{JS_PROP_NORMAL, JSObject, js_object_has_name},
    value::JSValue,
};

use super::{
    JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
    JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
    JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT, JS_PROP_LENGTH, JS_PROP_TMASK, JS_PROP_WRITABLE,
    JSProperty, JSPropertyDescriptor, check_define_prop_flags, get_prop_flags,
};

impl JSObject {
    /* allowed flags:
       JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
       JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
       JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
       JS_PROP_THROW, JS_PROP_NO_EXOTIC.
       If JS_PROP_THROW is set, return an exception instead of FALSE.
       if JS_PROP_NO_EXOTIC is set, do not call the exotic define_own_property callback.
    */
    /// Note: `prop` is not consumed.
    pub fn define_property<V: Into<JSValue>, G: Into<JSValue>, S: Into<JSValue>>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: V,
        getter: G,
        setter: S,
        flags: u32,
    ) -> Result<bool, i32> {
        let getter: JSValue = getter.into();
        let setter: JSValue = setter.into();
        let mut val: JSValue = val.into();

        loop {
            if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
                if prs_flags & JS_PROP_LENGTH != 0 && flags & JS_PROP_HAS_VALUE != 0 {
                    // is `length` property
                    // the range of the Array length property is always tested before
                    let array_length = unsafe { JS_ToArrayLengthFree(ctx, val, false)? };
                    val = JSValue::from(array_length);
                }

                //
                // property already exists
                //
                if unsafe { !check_define_prop_flags(prs_flags, flags) } {
                    return if ctx.throw_type_error_or_false(flags, "property is not configurable") {
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                if prs_flags & JS_PROP_TMASK == JS_PROP_LAZYINIT {
                    // Instantiate property
                    let mut pr = std::mem::take(&mut self.0.prop_values.borrow_mut()[idx]);
                    val = pr.instantiate(self, prop, self)?;
                    debug_assert!(matches!(pr, JSProperty::Normal(_)));
                    debug_assert_eq!(
                        self.find_shape_property(prop).unwrap().1 & JS_PROP_TMASK,
                        JS_PROP_NORMAL
                    );
                    self.0.prop_values.borrow_mut()[idx] = pr;
                }

                let res = if flags
                    & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
                    != 0
                {
                    if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                        self.update_prop_slot_getset(
                            ctx, prop, prs_flags, idx, getter, setter, flags,
                        )?
                    } else {
                        self.update_prop_slot_normal(ctx, prop, prs_flags, idx, val, flags)?
                    }
                } else {
                    false
                };

                // update shape property flag
                let mut mask = 0;
                if flags & JS_PROP_HAS_CONFIGURABLE != 0 {
                    mask |= JS_PROP_CONFIGURABLE;
                }
                if flags & JS_PROP_HAS_ENUMERABLE != 0 {
                    mask |= JS_PROP_ENUMERABLE;
                }
                if flags & JS_PROP_LENGTH == 0 {
                    // special prop '.length' must not be writable,
                    // while non-length property could be writable.
                    if flags & JS_PROP_HAS_WRITABLE != 0 {
                        mask |= JS_PROP_WRITABLE;
                    }
                }

                // Note: shape may have been changed, should reload current shape property
                let (_, prs_flags2) = self.find_shape_property(prop).unwrap();
                self.update_shape_property_flags(ctx, prop, (prs_flags2 & !mask) | (flags & mask))?;

                return Ok(res);
            }

            // at this point, shape property does not exist.
            // try fast array element
            if self.flags().is_fast_array() {
                match self.class_id() {
                    JS_CLASS_ARRAY => {
                        if let Some(idx) = prop.to_int() {
                            let idx = idx as usize;
                            let array_len = self.with_fast_array_values(|a| a.len()).unwrap();

                            if idx < array_len {
                                let prop_flags = get_prop_flags(flags, JS_PROP_C_W_E);
                                if prop_flags != JS_PROP_C_W_E {
                                    self.destruct_fast_array(ctx)?;
                                    continue; // redo_prop_update
                                }
                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    self.destruct_fast_array(ctx)?;
                                    continue; // redo_prop_update
                                }
                                if flags & JS_PROP_HAS_VALUE != 0 {
                                    self.with_fast_array_values_mut(|lst| {
                                        lst[idx] = val;
                                    })
                                    .unwrap();
                                }

                                return Ok(true);
                            }
                        }
                    }
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        if let Some(idx) = prop.to_int() {
                            /* if the typed array is detached, p->u.array.count = 0 */
                            if idx >= self.array_length_unchecked() as u32 {
                                return if ctx.throw_type_error_or_false(
                                    flags,
                                    "out-of-bound index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }

                            let prop_flags = get_prop_flags(
                                flags,
                                JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                            );
                            if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0
                                || prop_flags
                                    != (JS_PROP_ENUMERABLE
                                        | JS_PROP_WRITABLE
                                        | JS_PROP_CONFIGURABLE)
                            {
                                return if ctx.throw_type_error_or_false(
                                    flags as _,
                                    "invalid descriptor flags",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }
                            if flags & JS_PROP_HAS_VALUE != 0 {
                                return self.set_property_value(
                                    ctx,
                                    &JSValue::Int(idx as i32),
                                    val,
                                    flags,
                                );
                            }

                            return Ok(true);
                        } else {
                            /* slow path with to handle all numeric indexes */
                            let num = JS_AtomIsNumericIndex1(ctx, prop).err()?;
                            if num.is_undefined() {
                                break; // goto typed_array_done
                            }

                            let b = num.is_integral(ctx)?;
                            if !b {
                                return if ctx.throw_type_error_or_false(
                                    flags,
                                    "non integer index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }

                            if JS_NumberIsNegativeOrMinusZero(&num) {
                                return if ctx.throw_type_error_or_false(
                                    flags as _,
                                    "negative index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }

                            if !prop.is_int_type() {
                                return if ctx.throw_type_error_or_false(
                                    flags,
                                    "out-of-bound index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }
                        }
                    }

                    #[cfg(debug_assertions)]
                    JS_CLASS_ARGUMENTS => {}
                    _ => {
                        #[cfg(debug_assertions)]
                        unreachable!();
                    }
                }
            }

            break;
        } // loop

        self.create_property(ctx, prop, val, getter.clone(), setter.clone(), flags)
    }

    /// shortcut to add or redefine a new property value.
    /// Note: `prop` is not consumed.
    pub fn define_property_value<V: Into<JSValue>>(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: V,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property(
            ctx,
            prop,
            val,
            JSValue::UNDEFINED,
            JSValue::UNDEFINED,
            flags
                | JS_PROP_HAS_VALUE
                | JS_PROP_HAS_CONFIGURABLE
                | JS_PROP_HAS_WRITABLE
                | JS_PROP_HAS_ENUMERABLE,
        )
    }

    pub fn define_property_value_value<V: Into<JSValue>>(
        &self,
        ctx: &JSContext,
        prop: JSValue,
        val: V,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = prop.to_atom(ctx)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    pub fn define_property_value_str<V: Into<JSValue>>(
        &self,
        ctx: &JSContext,
        prop: &str,
        val: V,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    /// define property using descriptor object configuration.
    /// `prop` is not consumed.
    pub fn define_property_desc(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        desc: &JSObject,
        flags: u32,
    ) -> Result<bool, i32> {
        let d = JSPropertyDescriptor::parse(ctx, desc)?;
        self.define_property(ctx, prop, d.value, d.getter, d.setter, d.flags | flags)
    }

    #[inline(always)]
    pub fn create_data_property_u32(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value_i64(
            ctx,
            idx,
            val,
            flags | JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
        )
    }

    #[inline]
    pub fn define_property_value_i64(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    /// define getter, setter property.
    #[inline(always)]
    pub fn define_property_getset(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property(
            ctx,
            prop,
            JSValue::UNDEFINED,
            getter,
            setter,
            flags
                | JS_PROP_HAS_GET
                | JS_PROP_HAS_SET
                | JS_PROP_HAS_CONFIGURABLE
                | JS_PROP_HAS_ENUMERABLE,
        )
    }
}

// TODO: these methods on JSValue are not needed.
impl JSValue {
    /* allowed flags:
       JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
       JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
       JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
       JS_PROP_THROW, JS_PROP_NO_EXOTIC.
       If JS_PROP_THROW is set, return an exception instead of FALSE.
       if JS_PROP_NO_EXOTIC is set, do not call the exotic define_own_property callback.
    */
    /// Note: `prop` is not consumed.
    pub fn define_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.define_property(ctx, prop, val, getter, setter, flags)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// short cut to add or redefine a new property value.
    /// Note: `prop` is not consumed.
    #[inline(always)]
    pub fn define_property_value(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property(
            ctx,
            prop,
            val,
            JSValue::UNDEFINED,
            JSValue::UNDEFINED,
            flags
                | JS_PROP_HAS_VALUE
                | JS_PROP_HAS_CONFIGURABLE
                | JS_PROP_HAS_WRITABLE
                | JS_PROP_HAS_ENUMERABLE,
        )
    }

    pub fn define_property_value_str(
        &self,
        ctx: &JSContext,
        prop: &str,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = JSAtom::new(ctx.rt_mut(), prop)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    pub fn define_property_value_value(
        &self,
        ctx: &JSContext,
        prop: &JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = prop.to_atom(ctx)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    /// Note: `val` is consumed.
    #[inline(always)]
    pub fn define_property_value_u32(
        &self,
        ctx: &JSContext,
        idx: u32,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value(ctx, JSAtom::new_u32(idx), val, flags)
    }

    /// Note: `val` is consumed.
    #[inline(always)]
    pub fn define_property_value_i64(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // self.define_property_value_value(ctx, JSValue::from_i64(idx), val, flags)
        let atom = JSAtom::from_i64(ctx.rt_mut(), idx)?;
        let ret = self.define_property_value(ctx, atom, val, flags);
        JS_FreeAtom(ctx, atom);
        ret
    }

    /// define getter, setter property.
    /// Note: both `getter` and `setter` are consumed.
    #[inline(always)]
    pub fn define_property_getset(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property(
            ctx,
            prop,
            JSValue::UNDEFINED,
            getter,
            setter,
            flags
                | JS_PROP_HAS_GET
                | JS_PROP_HAS_SET
                | JS_PROP_HAS_CONFIGURABLE
                | JS_PROP_HAS_ENUMERABLE,
        )
    }

    /// `val` is consumed.
    #[inline(always)]
    pub fn create_data_property_u32(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value_i64(
            ctx,
            idx,
            val,
            flags | JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE | JS_PROP_WRITABLE,
        )
    }
}

#[deprecated(note = "unused")]
pub unsafe fn js_free_desc(ctx: &JSContext, desc: *mut JSPropertyDescriptor) {
    unreachable!("unused");
}

pub unsafe fn JS_DefineObjectName(ctx: &JSContext, obj: &JSValue, name: JSAtom, flags: u32) -> i32 {
    if name != JS_ATOM_NULL
        && obj.is_object()
        && !js_object_has_name(obj)
        && obj
            .define_property_value(
                ctx,
                JS_ATOM_name,
                name.to_string(ctx)
                    .map_or(JSValue::EXCEPTION, JSValue::String),
                flags,
            )
            .is_err()
    {
        -1
    } else {
        0
    }
}

/// Note: `obj` and `str_val` are not consumed.
pub unsafe fn JS_DefineObjectNameComputed(
    ctx: &JSContext,
    obj: &JSValue,
    str_val: &JSValue,
    flags: u32,
) -> i32 {
    if obj.is_object() && !js_object_has_name(obj) {
        let prop = str_val.to_atom(ctx).unwrap_or(JS_ATOM_NULL);
        if prop == JS_ATOM_NULL {
            return -1;
        }

        let name_str = js_get_function_name(ctx, prop);
        JS_FreeAtom(ctx, prop);
        if name_str.is_exception() {
            return -1;
        }
        if obj
            .define_property_value(ctx, JS_ATOM_name, name_str, flags)
            .is_err()
        {
            return -1;
        }
    }

    0
}
