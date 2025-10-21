use std::ops::Deref;

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{Atom, JS_ATOM_NULL, JS_AtomIsNumericIndex1, consts::*},
    branch_hints::unlikely,
    bytecode::js_get_function_name,
    context::JSContext,
    number::JS_NumberIsNegativeOrMinusZero,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_THROW, JSObject,
        JSProperty, PropertyKey,
    },
    value::JSValue,
};

use super::{
    JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
    JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
    JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT, JS_PROP_LENGTH, JS_PROP_TMASK, JS_PROP_WRITABLE,
    JSPropertyDescriptor, check_define_prop_flags, get_prop_flags,
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
    pub(crate) fn define_property(
        &self,
        ctx: &JSContext,
        prop: &Atom,
        value: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let mut val: JSValue = value;

        loop {
            if let Some((idx, prs_flags)) = self.find_shape_property(prop) {
                if prs_flags & JS_PROP_LENGTH != 0 && flags & JS_PROP_HAS_VALUE != 0 {
                    // is `length` property
                    // the range of the Array length property is always tested before
                    let array_length = val.to_array_length(ctx, false)?;
                    val = JSValue::from(array_length as u32);
                }

                if !check_define_prop_flags(prs_flags, flags) {
                    return if ctx.can_throw(flags) {
                        ctx.throw_type_error("property is not configurable");
                        Err(-1)
                    } else {
                        Ok(false)
                    };
                }

                if prs_flags & JS_PROP_TMASK == JS_PROP_LAZYINIT {
                    // Instantiate lazy property
                    unsafe {
                        self.0
                            .prop_values
                            .borrow_mut()
                            .get_unchecked_mut(idx)
                            .instantiate(self, idx, self)?;
                    }
                    continue;
                }

                let res = if flags
                    & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
                    != 0
                {
                    if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                        // update to getter/setter property
                        self.update_prop_slot_getset(
                            ctx, prop, prs_flags, idx, getter, setter, flags,
                        )
                    } else {
                        // update to normal property
                        self.update_prop_slot_normal(ctx, prop, prs_flags, idx, val, flags)
                    }
                } else {
                    Ok(true)
                };

                // update shape property flag
                let mut mask = 0;
                let new_flags = if flags & JS_PROP_LENGTH != 0 {
                    if flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE) == JS_PROP_HAS_WRITABLE {
                        // has {writable:false} setting for `length` property
                        mask |= JS_PROP_WRITABLE;
                    }
                    0
                } else {
                    if flags & JS_PROP_HAS_WRITABLE != 0 {
                        // has {writable:XXX} setting
                        mask |= JS_PROP_WRITABLE;
                    }
                    if flags & JS_PROP_HAS_CONFIGURABLE != 0 {
                        mask |= JS_PROP_CONFIGURABLE;
                    }
                    if flags & JS_PROP_HAS_ENUMERABLE != 0 {
                        mask |= JS_PROP_ENUMERABLE;
                    }

                    flags & mask
                };

                if res.is_ok() || flags & JS_PROP_LENGTH != 0 {
                    // do update shape property flags only when property was updated successfully
                    // or is updating length property.
                    // Note: shape may have been changed, should reload current shape property
                    let (_, prs_flags2) = self.find_shape_property(&prop).unwrap();
                    self.update_shape_property_flags(ctx, prop, (prs_flags2 & !mask) | new_flags)?;
                }

                return res;
            }

            // shape property does not exist, try fast array element
            if self.flags().is_fast_array() {
                match self.class_id() {
                    JS_CLASS_ARRAY => {
                        if let Some(idx) = prop.to_small_int() {
                            let idx = idx as usize;
                            let array_len = self.with_fast_array_values(|lst| lst.len()).unwrap();

                            if idx < array_len {
                                // update value in list
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
                        if let Some(idx) = prop.to_small_int() {
                            /* if the typed array is detached, p->u.array.count = 0 */
                            if idx >= self.fast_array_count() as u32 {
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

                            return if unlikely(
                                flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0
                                    || prop_flags
                                        != (JS_PROP_ENUMERABLE
                                            | JS_PROP_WRITABLE
                                            | JS_PROP_CONFIGURABLE),
                            ) {
                                if ctx.can_throw(flags) {
                                    ctx.throw_type_error("invalid descriptor flags");
                                    Err(-1)
                                } else {
                                    Ok(false)
                                }
                            } else if flags & JS_PROP_HAS_VALUE != 0 {
                                self.set_property_value(ctx, &JSValue::Int(idx as i32), val, flags)
                            } else {
                                Ok(true)
                            };
                        } else {
                            /* slow path with to handle all numeric indexes */
                            let num = JS_AtomIsNumericIndex1(ctx, prop).err()?;
                            if num.is_undefined() {
                                break; // goto typed_array_done
                            }

                            if unlikely(!num.is_integral(ctx)?) {
                                return if ctx.throw_type_error_or_false(
                                    flags,
                                    "non integer index in typed array",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            } else {
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
                                if !prop.is_small_int() {
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

        self.create_property(
            ctx,
            prop.clone(),
            val,
            getter.clone(),
            setter.clone(),
            flags,
        )
    }

    /// shortcut to add or redefine a new property value.
    /// Note: `prop` is not consumed.
    pub fn define_property_value(
        &self,
        ctx: &JSContext,
        prop: &Atom,
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
        let atom = Atom::new(ctx.rt(), prop)?;
        self.define_property_value(ctx, &atom, val, flags)
    }

    #[inline(always)]
    pub fn define_property_value_u32(
        &self,
        ctx: &JSContext,
        idx: u32,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value(ctx, &Atom::new_small_int(idx), val, flags)
    }

    #[inline]
    pub fn define_property_value_i64(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = Atom::from_i64(ctx.rt(), idx)?;
        self.define_property_value(ctx, &atom, val, flags)
    }

    /// define getter, setter property.
    #[inline(always)]
    pub fn define_property_getset(
        &self,
        ctx: &JSContext,
        prop: &Atom,
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

    /// define property using descriptor object configuration.
    /// `prop` is not consumed.
    #[inline]
    pub fn define_property_desc(
        &self,
        ctx: &JSContext,
        prop: &Atom,
        desc: &JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let d = JSPropertyDescriptor::from_value(ctx, desc)?;
        self.define_property(ctx, prop, d.value, d.getter, d.setter, d.flags | flags)
    }

    /// define multiple properties with descriptor defination object.
    ///
    /// equivelent to js `Object.defineProperties(properties)`
    pub fn define_property_desc_multi(
        &self,
        ctx: &JSContext,
        properties: &JSValue,
    ) -> Result<(), i32> {
        let props = properties.clone().objectify(ctx)?;

        // Note: each property of `props` is a descriptor object
        props.own_property_names_foreach(
            ctx,
            JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
            |_, key: PropertyKey| -> Result<(), i32> {
                let desc = props.get_property(ctx, &key.atom)?;
                self.define_property_desc(ctx, &key.atom, &desc, JS_PROP_THROW)?;
                Ok(())
            },
        )
    }

    /// define name property with value `name` in object.
    /// if name property already exists, don't define again.
    ///
    /// Note: `name` is not consumed.
    pub fn define_name(&self, ctx: &JSContext, name: &Atom, flags: u32) -> Result<bool, i32> {
        if name.is_null() {
            ctx.throw_type_error("invalid name");
            Err(-1)
        } else if !self.has_name() {
            self.define_property_value(
                ctx,
                JS_ATOM_name.deref(),
                name.force_string(ctx)?.into(),
                flags,
            )
        } else {
            Ok(false)
        }
    }

    pub fn define_name_computed(
        &self,
        ctx: &JSContext,
        str_val: &JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if !self.has_name() {
            let atom = str_val.to_atom(ctx)?;
            let name_str = js_get_function_name(ctx, &atom)?;
            self.define_property_value(ctx, JS_ATOM_name.deref(), name_str.into(), flags)
        } else {
            Ok(false)
        }
    }

    /// Check if object has 'name' property with non-empty normal string.
    fn has_name(&self) -> bool {
        if let Some((idx, _)) = self.find_shape_property(JS_ATOM_name.deref()) {
            if let JSProperty::Normal(JSValue::String(s)) =
                self.0.prop_values.borrow().get(idx).unwrap()
            {
                !s.is_empty()
            } else {
                true
            }
        } else {
            false
        }
    }

    #[inline(always)]
    #[deprecated]
    pub fn create_data_property_u32(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value_i64(ctx, idx, val, flags | JS_PROP_C_W_E)
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
    pub(crate) fn define_property(
        &self,
        ctx: &JSContext,
        prop: &Atom,
        val: JSValue,    // TODO: use Option<JSValue> to eliminate JS_PROP_HAS_XXX
        getter: JSValue, // TODO: use Option<JSValue> to eliminate JS_PROP_HAS_XXX
        setter: JSValue, // TODO: use Option<JSValue> to eliminate JS_PROP_HAS_XXX
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.define_property(ctx, prop, val, getter, setter, flags)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// add or update a property with `value`.
    /// Note: `prop` is not consumed.
    #[inline(always)]
    pub fn define_property_value(
        &self,
        ctx: &JSContext,
        prop: &Atom,
        value: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property(
            ctx,
            prop,
            value,
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
        let atom = Atom::new(ctx.rt(), prop)?;
        self.define_property_value(ctx, &atom, val, flags)
    }

    #[inline(always)]
    pub fn define_property_value_u32(
        &self,
        ctx: &JSContext,
        idx: u32,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value(ctx, &Atom::new_small_int(idx), val, flags)
    }

    #[inline(always)]
    pub fn define_property_value_i64(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = Atom::from_i64(ctx.rt(), idx)?;
        self.define_property_value(ctx, &atom, val, flags)
    }

    #[inline(always)]
    pub fn define_property_getset(
        &self,
        ctx: &JSContext,
        prop: &Atom,
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

    #[deprecated(
        note = "use JSValue::define_property_value_i64() or JSValue::define_property_value_u32()"
    )]
    #[inline(always)]
    pub fn create_data_property_u32(
        &self,
        ctx: &JSContext,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value_i64(ctx, idx, val, flags | JS_PROP_C_W_E)
    }
}
