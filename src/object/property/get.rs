// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY,
    JS_CLASS_UINT32_ARRAY,
    atom::{Atom, JS_ATOM_MAX_INT},
    context::ContextImpl,
    error::{JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized, JSResult},
    object::{JSProperty, Object, ObjectImpl},
    utils::unlikely,
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{JS_PROP_GETSET, JS_PROP_LAZYINIT, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_VARREF};

impl ObjectImpl {
    /// The core method to retrive property value: either by shape/slots or by exotic handler.
    ///
    /// walk up proto chain and get property in shape or by exotic handler.
    pub(crate) fn get_property_internal(
        &self,
        ctx: &mut ContextImpl,
        prop: Atom,
        this_obj: JSValue,
        throw_ref_error: bool,
    ) -> JSResult {
        #[cfg(debug_assertions)]
        self.gc_head()
            .debug_assert_node_valid(ctx.rt_ref().gc_heap());

        for mut cur_obj in self.shape_proto_iter(true) {
            'redo: loop {
                // 1. search own property (in shape)
                if let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                    match prs_flags & JS_PROP_TMASK {
                        JS_PROP_NORMAL => {
                            return if let JSProperty::Normal(val) = cur_obj.prop_slot(idx) {
                                debug_assert!(!val.is_exception());
                                Ok(*val)
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
                            return if let JSProperty::GetSet {
                                getter: Some(fun), ..
                            } = cur_obj.prop_slot(idx)
                            {
                                // protect `self` before calling getter
                                ctx.gc_heap_mut().protect_node(self.gc_head_ptr());
                                fun.call(ctx, this_obj, &[])
                            } else {
                                Ok(JSValue::UNDEFINED)
                            };
                        }
                        JS_PROP_VARREF => {
                            let val = cur_obj.prop_slot(idx).value(ctx, this_obj)?;
                            return if unlikely(val.is_uninitialized()) {
                                JS_ThrowReferenceErrorUninitialized(ctx, prop);
                                Err(-1)
                            } else {
                                Ok(val)
                            };
                        }
                        JS_PROP_LAZYINIT => {
                            // instantiate lazy object
                            let val = cur_obj
                                .prop_slot_mut(idx)
                                .instantiate(ctx, Object::from_ref(self));

                            // modify shape property kind to NORMAL
                            cur_obj.modify_shape_property_index_flags(ctx, idx, |f| {
                                f & !JS_PROP_TMASK
                            })?;

                            // set value to prop slot
                            cur_obj.set_prop_slot(
                                idx,
                                JSProperty::Normal(if let Ok(v) = val {
                                    v
                                } else {
                                    JSValue::UNDEFINED
                                }),
                            );

                            // redo to get prop value from slot
                            val?;
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
                        if idx < cur_obj.fast_array_count() {
                            return cur_obj.get_property_value(ctx, JSValue::from(idx));
                        }

                        if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            return Ok(JSValue::UNDEFINED);
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                        && prop.is_numeric_index(ctx)?
                    {
                        return Ok(JSValue::UNDEFINED);
                    }
                } else if let Some(pm) = ctx.rt_ptr().class_registry[cur_obj.class_id() as usize]
                    .exotic
                    .as_ref()
                {
                    if let Some(cb) = pm.get_property {
                        // protect self, cur_obj, this_obj before calling handler
                        // {
                        //     let heap = ctx.gc_heap_mut();
                        //     heap.protect_nodes_v2(&[self.gc_head_ptr(), cur_obj.gc_head_ptr()]);
                        //     if let Some(n) = this_obj.gc_node_ptr() {
                        //         heap.protect_node_v2(n);
                        //     }
                        // }

                        // XXX: should pass throw_ref_error
                        return cb(ctx.gc_ref().into(), cur_obj.into(), prop, this_obj);
                    }

                    if let Some(cb) = pm.get_own_property
                        && let Some(desc) = cb(ctx.into(), JSValue::Object(cur_obj), prop)?
                    {
                        return if unlikely(desc.flags & JS_PROP_GETSET != 0) {
                            // protect self, cur_obj, this_obj before calling getter
                            // {
                            //     let heap = ctx.gc_heap_mut();
                            //     heap.protect_nodes_v2(&[self.gc_head_ptr(), cur_obj.gc_head_ptr()]);
                            //     if let Some(n) = this_obj.gc_node_ptr() {
                            //         heap.protect_node_v2(n);
                            //     }
                            // }

                            desc.getter.call(ctx, this_obj, &[])
                        } else {
                            debug_assert!(!desc.value.is_exception());
                            Ok(desc.value)
                        };
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

    /// get property by prop atom.
    #[inline(always)]
    pub fn get_property(&self, ctx: &mut ContextImpl, prop: Atom) -> JSResult {
        self.get_property_internal(ctx, prop, Object::from_ref(self).into(), false)
    }

    /// get property by index key.
    ///
    /// Returns:
    /// - Some: property value
    /// - None: try slow path instead
    fn get_property_index(&self, idx: usize) -> Option<JSResult> {
        macro_rules! typed_array_el {
            ($ty:ident, $vtype:ident) => {
                let o = Object::from_ref(self);
                let ta = o.as_TypedArray().unwrap();
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
                if self.is_fast_array() {
                    let arr_data = self.payload().array().unwrap();
                    if let Some(&el) = arr_data.values().get(idx) {
                        return Some(Ok(el));
                    }
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

    #[inline]
    fn get_property_value_slow(&self, ctx: &mut ContextImpl, prop: JSValue) -> JSResult {
        let atom = prop.to_atom(ctx)?;
        self.get_property_internal(ctx, atom, Object::from_ref(self).into(), false)
    }

    /// get property by prop value
    pub fn get_property_value(&self, ctx: &mut ContextImpl, prop: JSValue) -> JSResult {
        // fast path: check if prop is an index
        match prop {
            JSValue::Int(i) if i >= 0 => {
                if let Some(r) = self.get_property_index(i as usize) {
                    return r;
                }
            }
            JSValue::Float64(f) if f >= 0.0 => {
                if f.fract() == 0.0 {
                    let n = f.trunc() as i64;
                    if n <= u32::MAX as i64
                        && let Some(r) = self.get_property_index(n as usize)
                    {
                        return r;
                    }
                }
            }
            _ => {}
        }

        // slow path
        self.get_property_value_slow(ctx, prop)
    }

    #[inline]
    pub fn get_property_u32(&self, ctx: &mut ContextImpl, idx: u32) -> JSResult {
        self.get_property_index(idx as usize)
            .unwrap_or_else(|| self.get_property_value_slow(ctx, JSValue::from(idx)))
    }

    #[inline]
    pub fn get_property_i64(&self, ctx: &mut ContextImpl, idx: i64) -> JSResult {
        if (0..u32::MAX as i64).contains(&idx) {
            self.get_property_u32(ctx, idx as u32)
        } else {
            let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
            self.get_property(ctx, atom)
        }
    }

    #[inline(always)]
    pub fn get_property_str(&self, ctx: &mut ContextImpl, prop: &str) -> JSResult {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.get_property(ctx, atom)
    }

    /// Check if `self` has a generalized numeric property `idx`.
    /// If yes, get property value of the prop `idx`.
    /// Returns Err for exception, Ok(Some(_)) if property exists, Ok(None) if proprty does not exist.
    pub fn try_get_property_i64(
        &self,
        ctx: &mut ContextImpl,
        idx: i64,
    ) -> Result<Option<JSValue>, i32> {
        if (0..=JS_ATOM_MAX_INT as i64).contains(&idx) {
            // fast path
            if self.has_property(ctx, Atom::new_small_int(idx as u32))? {
                Ok(Some(
                    self.get_property_value(ctx, JSValue::from(idx as u32))?,
                ))
            } else {
                Ok(None)
            }
        } else {
            let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
            self.has_property(ctx, atom).and_then(|found| {
                if found {
                    Ok(Some(self.get_property(ctx, atom)?))
                } else {
                    Ok(None)
                }
            })
        }
    }
}

impl JSValue {
    pub(crate) fn get_property_internal(
        &self,
        ctx: &mut ContextImpl,
        prop: Atom,
        this_obj: JSValue,
        throw_ref_error: bool,
    ) -> JSResult {
        match self {
            Self::Object(o) => o.get_property_internal(ctx, prop, this_obj, throw_ref_error),
            Self::String(s) => {
                if unlikely(prop == Atom::CONST_length) {
                    // string length
                    Ok(JSValue::from(s.len() as u32))
                } else if let Some(idx) = prop.to_small_int() {
                    // code point at idx
                    if let Some(code) = s.content().get(idx as usize) {
                        ctx.create_string_from_char(code.into()).map(JSValue::from)
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
                    prop.to_utf8(ctx.rt_ref()),
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

    /// get property by prop atom
    #[inline(always)]
    pub fn get_property(&self, ctx: &mut ContextImpl, prop: Atom) -> JSResult {
        self.get_property_internal(ctx, prop, *self, false)
    }

    /// get property by prop value
    #[inline]
    pub fn get_property_value(&self, ctx: &mut ContextImpl, prop: JSValue) -> JSResult {
        if let JSValue::Object(o) = self {
            o.get_property_value(ctx, prop)
        } else {
            let atom = prop.to_atom(ctx)?;
            self.get_property_internal(ctx, atom, *self, false)
        }
    }

    /// get property by str prop
    #[inline(always)]
    pub fn get_property_str(&self, ctx: &mut ContextImpl, prop: &str) -> JSResult {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.get_property(ctx, atom)
    }

    /// get property by u32 prop
    #[inline(always)]
    pub fn get_property_u32(&self, ctx: &mut ContextImpl, idx: u32) -> JSResult {
        self.get_property_value(ctx, JSValue::from(idx))
    }

    pub fn get_property_i64(&self, ctx: &mut ContextImpl, idx: i64) -> JSResult {
        if (0..=u32::MAX as i64).contains(&idx) {
            self.get_property_u32(ctx, idx as u32)
        } else {
            let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
            self.get_property(ctx, atom)
        }
    }

    /// Check if `self` has a generalized numeric property `idx`.
    /// If yes, get property value of the prop `idx`.
    /// Returns Err for exception, Ok(Some(_)) if property exists, Ok(None) if proprty does not exist.
    pub fn try_get_property_i64(
        &self,
        ctx: &mut ContextImpl,
        idx: i64,
    ) -> Result<Option<JSValue>, i32> {
        if (0..=JS_ATOM_MAX_INT as i64).contains(&idx) {
            // fast path
            if self.has_property(ctx, Atom::new_small_int(idx as u32))? {
                Ok(Some(
                    self.get_property_value(ctx, JSValue::from(idx as u32))?,
                ))
            } else {
                Ok(None)
            }
        } else {
            let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
            self.has_property(ctx, atom).and_then(|found| {
                if found {
                    Ok(Some(self.get_property(ctx, atom)?))
                } else {
                    Ok(None)
                }
            })
        }
    }
}
