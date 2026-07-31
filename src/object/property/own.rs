// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    atom::Atom,
    context::ScopedContext,
    error::JS_ThrowReferenceErrorUninitialized,
    function::prop_str_to_atom,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET,
        JS_PROP_HAS_SET, JS_PROP_HAS_VALUE, JS_PROP_LAZYINIT, JS_PROP_NO_EXOTIC, JS_PROP_NORMAL,
        JS_PROP_TMASK, JS_PROP_VARREF, JS_PROP_WRITABLE, JSProperty, JSPropertyDescriptor, Object,
        ObjectImpl,
    },
    utils::unlikely,
    value::JSValue,
};

use super::get_prop_flags;

impl ObjectImpl {
    /// Add an own property, and grow object's prop value slots.
    /// This method adds the new prop is to shape, it assumes the new prop does not exist in shape yet.
    /// Returns Err if memory error.
    #[inline]
    pub(crate) fn add_own_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        prop_flags: u32,
        value: JSProperty,
    ) -> Result<(), i32> {
        let len = self.modify_shape_add_prop(ctx, prop, prop_flags)?;
        debug_assert_eq!(self.prop_values.len(), len);
        self.set_prop_slot(len - 1, value);
        Ok(())
    }

    /// Find shape property by name atom.
    /// Returns shape property index (the slot index, not the sequence) and prop flags if the prop exists.
    #[inline]
    pub(crate) fn find_shape_property(&self, prop: Atom) -> Option<(usize, u32)> {
        let sh = self.shape()?;
        for (i, prs) in sh.prop_defs.find_hash(prop.id()) {
            if !matches!(self.prop_slot(i), JSProperty::Deleted) {
                return Some((i, prs.flags()));
            }
        }
        None
    }

    /// Check if given property name exists in shape.
    #[inline(always)]
    pub(crate) fn has_shape_property(&self, prop: Atom) -> bool {
        self.find_shape_property(prop).is_some()
    }

    /// test if own property `prop` exists:
    /// 1. find in shape,
    /// 1. if not shape property, and if is exotic or fast array, test by exotic or array length.
    pub fn has_own_property(&self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        // is shape prop?
        if self.has_shape_property(prop) {
            return Ok(true);
        }

        // check exotic
        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                if let Some(idx) = prop.to_small_int()
                    && idx < self.fast_array_count()
                {
                    // array index is own property
                    return Ok(true);
                }
            } else if let Some(cb) = ctx.rt_ptr().class_registry[self.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|x| x.get_own_property)
            {
                return cb(ctx, Object::from_ref(self).into(), prop).map(|d| d.is_some());
            }
        }

        Ok(false)
    }

    #[inline]
    pub fn has_own_property_str(&self, ctx: &mut ScopedContext, prop: &str) -> Result<bool, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.has_own_property(ctx, atom)
    }

    /// get own property descriptor.
    /// Returns None if own property does not exist.
    pub fn get_own_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        while let Some((idx, prs_flags)) = self.find_shape_property(prop) {
            let mut desc = JSPropertyDescriptor {
                flags: prs_flags & JS_PROP_C_W_E,
                ..Default::default()
            };

            match prs_flags & JS_PROP_TMASK {
                JS_PROP_NORMAL => {
                    desc.value = self.prop_slot(idx).value(ctx, JSValue::UNDEFINED)?;
                }
                JS_PROP_GETSET => {
                    desc.flags |= JS_PROP_GETSET;
                    if let JSProperty::GetSet { getter, setter } = self.prop_slot(idx) {
                        desc.getter = getter.map_or(JSValue::UNDEFINED, JSValue::from);
                        desc.setter = setter.map_or(JSValue::UNDEFINED, JSValue::from);
                    } else {
                        unreachable!()
                    }
                }
                JS_PROP_VARREF => {
                    desc.value = self.prop_slot(idx).value(ctx, JSValue::UNDEFINED)?;
                    if unlikely(desc.value.is_uninitialized()) {
                        return Err(JS_ThrowReferenceErrorUninitialized(ctx, prop));
                    }
                }
                JS_PROP_LAZYINIT => {
                    // take lazy field definition
                    let pd = self.replace_prop_slot(idx, JSProperty::default());
                    debug_assert!(matches!(pd, JSProperty::LazyInit { .. }));

                    // modify shape property kind to NORMAL.
                    // Modifying the shape may compact deleted slots (shifting
                    // indices); the returned index is the up-to-date one.
                    let idx2 =
                        self.modify_shape_property_index_flags(ctx, idx, |f| f & !JS_PROP_TMASK)?;

                    // instantiate the value, and set it to prop slot
                    let val = pd.instantiate(prop, Object::from_ref(self));
                    self.set_prop_slot(idx2, JSProperty::Normal(val?));

                    continue;
                }
                _ => {
                    unreachable!();
                }
            }

            return Ok(Some(desc));
        }

        let flags = self.flags();
        if flags.is_exotic() {
            if flags.is_fast_array() {
                // specific case for fast arrays
                if let Some(idx) = prop.to_small_int()
                    && idx < self.fast_array_count()
                {
                    return Ok(Some(JSPropertyDescriptor {
                        flags: JS_PROP_C_W_E,
                        value: self.get_property_value(ctx, JSValue::from(idx))?,
                        getter: JSValue::UNDEFINED,
                        setter: JSValue::UNDEFINED,
                    }));
                }
            } else if let Some(f) = ctx.rt_ptr().class_registry[self.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|x| x.get_own_property)
            {
                return f(ctx, Object::from_ref(self).into(), prop);
            }
        }

        Ok(None)
    }

    /// Similar to define_property(), usually used to initialize property for new instance.
    ///
    /// - If property already exists, update its value;
    /// - If property is array index and self is fast array,
    ///   update indexed value. Will destruct fast array to sparse array when needed.
    /// - otherwise, call `Self::add_own_property()` to add a shape property.
    pub(super) fn create_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        fn update_array_length_fn(
            cx: &mut ScopedContext,
            mut arr_obj: Object,
            idx: u32,
            cur_len: i64,
            flags: u32,
        ) -> Option<Result<bool, i32>> {
            debug_assert!(idx < u32::MAX, "array length<=u32::MAX, so idx<u32::MAX");

            if idx + 1 > cur_len as u32 {
                // first prop must be `length`
                let prs_flags = arr_obj
                    .shape()
                    .and_then(|sh| sh.prop_defs.first())
                    .map(|p| p.flags())
                    .unwrap(); // <-- prop length must exist.

                if unlikely(prs_flags & JS_PROP_WRITABLE == 0) {
                    return if cx.rt().can_throw(flags) {
                        Some(Err(cx.throw_type_error_read_only(Atom::CONST_length)))
                    } else {
                        None
                    };
                } else {
                    arr_obj.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(idx + 1)));
                }
            }

            None
        }

        let obj_flags = self.flags();

        // add a new property or modify an existing exotic one
        if obj_flags.is_exotic() {
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    // get current `length`
                    let Ok(arr_len) = self
                        .prop_slot(0)
                        .value(ctx, JSValue::UNDEFINED)
                        .and_then(|v| v.to_length(ctx))
                    else {
                        return if ctx.rt().can_throw(flags) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    };

                    if obj_flags.is_fast_array() {
                        if let Some(idx) = prop.to_small_int() {
                            let list_count = self.fast_array_count();
                            if idx == list_count {
                                if unlikely(!self.flags().extensible()) {
                                    return if ctx.rt().can_throw(flags) {
                                        Err(ctx.throw_type_error("object is not extensible"))
                                    } else {
                                        Ok(false)
                                    };
                                }

                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    if let Err(e) = self.destruct_fast_array(ctx) {
                                        return if ctx.rt().can_throw(flags) {
                                            Err(e)
                                        } else {
                                            Ok(false)
                                        };
                                    }
                                    if let Some(ret) = update_array_length_fn(
                                        ctx,
                                        Object::from_ref(self),
                                        idx,
                                        arr_len,
                                        flags,
                                    ) {
                                        return ret;
                                    }
                                }

                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    match self.destruct_fast_array(ctx) {
                                        Ok(_) => {
                                            if let Some(ret) = update_array_length_fn(
                                                ctx,
                                                Object::from_ref(self),
                                                idx,
                                                arr_len,
                                                flags,
                                            ) {
                                                return ret;
                                            }
                                        }
                                        Err(e) => {
                                            return if ctx.rt().can_throw(flags) {
                                                Err(e)
                                            } else {
                                                Ok(false)
                                            };
                                        }
                                    }
                                } else {
                                    // fast array append element.
                                    return self.add_fast_array_element(ctx, val, flags);
                                }
                            } else {
                                // leaves a hole between `arr_idx` and `idx`,
                                // will degrade to sparse array.
                                if let Err(e) = self.destruct_fast_array(ctx) {
                                    return if ctx.rt().can_throw(flags) {
                                        Err(e)
                                    } else {
                                        Ok(false)
                                    };
                                }
                                if let Some(ret) = update_array_length_fn(
                                    ctx,
                                    Object::from_ref(self),
                                    idx,
                                    arr_len,
                                    flags,
                                ) {
                                    return ret;
                                }
                            }
                        } else if let Some(idx) = prop.to_array_index(ctx.rt()) {
                            // convert fast array to sparse array
                            if self.destruct_fast_array(ctx).is_err() {
                                return if ctx.rt().can_throw(flags) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            } else if let Some(ret) = update_array_length_fn(
                                ctx,
                                Object::from_ref(self),
                                idx,
                                arr_len,
                                flags,
                            ) {
                                return ret;
                            }
                        }
                    } else if let Some(idx) = prop.to_array_index(ctx.rt())
                        && let Some(ret) =
                            update_array_length_fn(ctx, Object::from_ref(self), idx, arr_len, flags)
                    {
                        return ret;
                    }
                } // Array, Arguments

                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    if prop.is_numeric_index(ctx)? {
                        return if ctx.rt().can_throw(flags) {
                            Err(ctx.throw_type_error("cannot create numeric index in typed array"))
                        } else {
                            Ok(false)
                        };
                    }
                }

                cid => {
                    if flags & JS_PROP_NO_EXOTIC == 0
                        && let Some(pm) = ctx.rt_ptr().class_registry[cid as usize].exotic.as_ref()
                    {
                        if let Some(f) = pm.define_own_property {
                            return f(
                                ctx,
                                Object::from_ref(self).into(),
                                prop,
                                val,
                                getter,
                                setter,
                                flags as _,
                            );
                        }

                        if unlikely(!self.is_extensible_obj(ctx)?) {
                            return if ctx.rt().can_throw(flags) {
                                Err(ctx.throw_type_error("object is not extensible"))
                            } else {
                                Ok(false)
                            };
                        }
                    }
                }
            }
        }

        // Generic: if prop is not an exotic property, nor an array element:
        // add shape property and put the value in value slot.

        if unlikely(!obj_flags.extensible()) {
            if ctx.rt().can_throw(flags) {
                Err(ctx.throw_type_error("object is not extensible"))
            } else {
                Ok(false)
            }
        } else {
            let prop_flags = if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                (flags & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
            } else {
                flags & JS_PROP_C_W_E
            };

            self.add_own_property(ctx, prop, prop_flags, {
                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                    // getset property
                    JSProperty::GetSet {
                        getter: if flags & JS_PROP_HAS_GET != 0 && getter.is_callable() {
                            getter.as_object()
                        } else {
                            None
                        },
                        setter: if flags & JS_PROP_HAS_SET != 0 && setter.is_callable() {
                            setter.as_object()
                        } else {
                            None
                        },
                    }
                } else {
                    // normal property
                    JSProperty::Normal(if flags & JS_PROP_HAS_VALUE != 0 {
                        val
                    } else {
                        JSValue::UNDEFINED
                    })
                }
            })?;

            Ok(true)
        }
    }
}

impl JSValue {
    /// test if own property prop exists.
    pub fn has_own_property(&self, ctx: &mut ScopedContext, prop: Atom) -> Result<bool, i32> {
        if let Some(p) = self.as_object() {
            p.has_own_property(ctx, prop)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    pub fn has_own_property_str(&self, ctx: &mut ScopedContext, prop: &str) -> Result<bool, i32> {
        if let Some(p) = self.as_object() {
            p.has_own_property_str(ctx, prop)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    /// get description of own property.
    pub fn get_own_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
    ) -> Result<Option<JSPropertyDescriptor>, i32> {
        if let Some(mut p) = self.as_object() {
            p.get_own_property(ctx, prop)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }
}
