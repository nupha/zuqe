// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::{Atom, JS_ATOM_TAG_INT},
    bytecode::operators::js_get_function_name,
    context::ContextImpl,
    number::IntOrFloat,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_THROW, JSProperty, Object,
        ObjectImpl, PropertyKey,
    },
    utils::unlikely,
    value::JSValue,
};

use super::{
    JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_HAS_CONFIGURABLE,
    JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
    JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT, JS_PROP_LENGTH, JS_PROP_TMASK, JS_PROP_WRITABLE,
    JSPropertyDescriptor, check_define_prop_flags, get_prop_flags,
};

impl ObjectImpl {
    /* allowed flags:
       JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, JS_PROP_ENUMERABLE
       JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
       JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_WRITABLE, JS_PROP_HAS_ENUMERABLE,
       JS_PROP_THROW, JS_PROP_NO_EXOTIC.
       If JS_PROP_THROW is set, return an exception instead of FALSE.
       if JS_PROP_NO_EXOTIC is set, do not call the exotic define_own_property callback.
    */
    pub(crate) fn define_property(
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
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
                    val = JSValue::from(array_length);
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
                    let val = {
                        let this = Object::from_ref(self);
                        self.prop_slot_mut(idx).instantiate(ctx, this)
                    };

                    // modify shape property kind to NORMAL
                    self.modify_shape_property_index_flags(ctx, idx, |f| f & !JS_PROP_TMASK)?;
                    // update prop slot value
                    self.set_prop_slot(
                        idx,
                        JSProperty::Normal(if let Ok(v) = val {
                            v
                        } else {
                            JSValue::UNDEFINED
                        }),
                    );

                    val?;
                    continue;
                }

                let res = if flags
                    & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)
                    != 0
                {
                    if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                        // update to getter/setter func
                        self.update_prop_slot_getset(
                            ctx, prop, prs_flags, idx, getter, setter, flags,
                        )
                    } else {
                        // set property value, handle 'writable' flag
                        let r = self.update_prop_slot_value(ctx, prop, prs_flags, idx, val, flags);
                        if prs_flags & JS_PROP_LENGTH != 0 {
                            return r;
                        }
                        r
                    }
                } else {
                    Ok(true)
                };

                // update 'configurable', 'enumerable' flags
                let mut mask = 0;
                if flags & JS_PROP_HAS_CONFIGURABLE != 0 {
                    mask |= JS_PROP_CONFIGURABLE;
                }
                if flags & JS_PROP_HAS_ENUMERABLE != 0 {
                    mask |= JS_PROP_ENUMERABLE;
                }
                self.modify_shape_property_flags(ctx, prop, |f| (f & !mask) | (flags & mask))?;
                return res;
            }

            // shape property does not exist, try fast array element
            if self.flags().is_fast_array() {
                match self.class_id() {
                    JS_CLASS_ARRAY => {
                        if let Some(idx) = prop.to_small_int() {
                            let idx = idx as usize;
                            let array_len = self.payload().array().unwrap().len();

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
                                    let xref = self.gc_head().partition_id();
                                    self.payload_mut().array_mut().unwrap().set_nth(
                                        ctx.rt_mut(),
                                        idx,
                                        val,
                                        xref,
                                    );
                                }
                                return Ok(true);
                            }
                        }
                    }
                    JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                        if let Some(idx) = prop.to_small_int() {
                            /* if the typed array is detached, p->u.array.count = 0 */
                            if idx >= self.fast_array_count() {
                                return if ctx.can_throw(flags) {
                                    ctx.throw_type_error("out-of-bound index in typed array");
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
                                self.set_property_value(ctx, JSValue::Int(idx as i32), val, flags)
                            } else {
                                Ok(true)
                            };
                        } else {
                            /* slow path with to handle all numeric indexes */
                            let Some(num) = prop.to_numeric_index(ctx)? else {
                                break;
                            };

                            match num {
                                IntOrFloat::Int64(i) => {
                                    if i < 0 {
                                        return if ctx.can_throw(flags) {
                                            ctx.throw_type_error("negative index in typed array");
                                            Err(-1)
                                        } else {
                                            Ok(false)
                                        };
                                    }
                                    if i > JS_ATOM_TAG_INT as i64 {
                                        return if ctx.can_throw(flags) {
                                            ctx.throw_type_error(
                                                "out-of-bound index in typed array",
                                            );
                                            Err(-1)
                                        } else {
                                            Ok(false)
                                        };
                                    }
                                }
                                IntOrFloat::Float64(_) => {
                                    return if ctx.can_throw(flags) {
                                        ctx.throw_type_error("non integer index in typed array");
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

        self.create_property(ctx.into(), prop, val, getter, setter, flags)
    }

    /// shortcut to add or redefine a new property value.
    /// Note: `prop` is not consumed.
    pub fn define_property_value(
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
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
        &mut self,
        ctx: &mut ContextImpl,
        prop: &str,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.define_property_value(ctx, atom, val, flags)
    }

    #[inline(always)]
    pub fn define_property_value_u32(
        &mut self,
        ctx: &mut ContextImpl,
        idx: u32,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value(ctx, Atom::new_small_int(idx), val, flags)
    }

    #[inline]
    pub fn define_property_value_i64(
        &mut self,
        ctx: &mut ContextImpl,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
        self.define_property_value(ctx, atom, val, flags)
    }

    /// define getter, setter property.
    #[inline(always)]
    pub fn define_property_getset(
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
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
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
        desc: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let d = JSPropertyDescriptor::from_value(ctx, desc)?;
        self.define_property(ctx, prop, d.value, d.getter, d.setter, d.flags | flags)
    }

    /// define multiple properties with descriptor defination object.
    ///
    /// equivelent to js `Object.defineProperties(properties)`
    pub fn define_properties(
        &mut self,
        ctx: &mut ContextImpl,
        properties: JSValue,
    ) -> Result<(), i32> {
        let props = properties.objectify(ctx)?;

        // Note: each property of `props` is a descriptor object
        let mut this = Object::from_ref(self);
        let props2 = props;

        props.own_property_names_foreach(
            ctx,
            JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
            |cx, key: PropertyKey| -> Result<(), i32> {
                let desc = props2.get_property(cx, key.atom)?;
                this.define_property_desc(cx, key.atom, desc, JS_PROP_THROW)?;
                Ok(())
            },
        )
    }

    /// define name property with value `name` in object.
    /// if name property already exists, don't define again.
    pub fn define_name(
        &mut self,
        ctx: &mut ContextImpl,
        name: Atom,
        flags: u32,
    ) -> Result<bool, i32> {
        if name.is_null() {
            ctx.throw_type_error("invalid name");
            Err(-1)
        } else if !self.has_name() {
            let name_str = name.force_string(ctx)?;
            self.define_property_value(ctx, Atom::CONST_name, name_str.into(), flags)
        } else {
            Ok(false)
        }
    }

    pub fn define_name_computed(
        &mut self,
        ctx: &mut ContextImpl,
        str_val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        if !self.has_name() {
            let atom = str_val.to_atom(ctx)?;
            let name_str = js_get_function_name(ctx.into(), atom)?;
            self.define_property_value(ctx, Atom::CONST_name, name_str.into(), flags)
        } else {
            Ok(false)
        }
    }

    /// Check if object has 'name' property with non-empty normal string.
    pub fn has_name(&self) -> bool {
        if let Some((idx, _)) = self.find_shape_property(Atom::CONST_name) {
            if let JSProperty::Normal(JSValue::String(s)) = self.prop_slot(idx) {
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
        &mut self,
        ctx: &mut ContextImpl,
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
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
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
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
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

    #[inline(always)]
    pub fn define_property_value_str(
        &mut self,
        ctx: &mut ContextImpl,
        prop: &str,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.define_property_value(ctx, atom, val, flags)
    }

    #[inline(always)]
    pub fn define_property_value_u32(
        &mut self,
        ctx: &mut ContextImpl,
        idx: u32,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value(ctx, Atom::new_small_int(idx), val, flags)
    }

    #[inline(always)]
    pub fn define_property_value_i64(
        &mut self,
        ctx: &mut ContextImpl,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
        self.define_property_value(ctx, atom, val, flags)
    }

    #[inline(always)]
    pub fn define_property_getset(
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
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
        &mut self,
        ctx: &mut ContextImpl,
        idx: i64,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        self.define_property_value_i64(ctx, idx, val, flags | JS_PROP_C_W_E)
    }
}
