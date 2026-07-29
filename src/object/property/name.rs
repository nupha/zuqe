// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_STRING, ScopedContext,
    array::alloc_fast_array_slots,
    atom::{Atom, AtomDescKind},
    builtins::iterator::JSIteratorKindEnum,
    context::ContextImpl,
    error::{JS_ThrowReferenceErrorUninitialized, JSResult},
    object::{JS_PROP_TMASK, JS_PROP_VARREF, JSProperty, Object, ObjectImpl},
    string::StringRef,
    utils::unlikely,
    value::JSValue,
};

use super::{JS_GPN_ENUM_ONLY, JS_GPN_SET_ENUM, JS_GPN_STR_OR_INT_MASK, JS_PROP_ENUMERABLE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKeyKind {
    String,
    Symbol,
    /// in range `[0, u32::MAX)`
    ArrayIndex(u32),
}

#[derive(Debug, Clone)]
pub struct PropertyKey {
    pub atom: Atom,
    pub kind: PropertyKeyKind,
    pub is_enumerable: bool,
    pub is_exotic: bool,
}

impl GcTrace for PropertyKey {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.atom.trace(tr);
    }
}

impl PropertyKey {
    pub(crate) fn new(
        ctx: &mut ContextImpl,
        atom: Atom,
        is_enumerable: bool,
        is_exotic: bool,
    ) -> Self {
        let kind = if let Some(idx) = atom.to_array_index(ctx.rt()) {
            PropertyKeyKind::ArrayIndex(idx)
        } else if atom.desc_kind(ctx.rt()) == AtomDescKind::JS_ATOM_KIND_STRING {
            PropertyKeyKind::String
        } else {
            PropertyKeyKind::Symbol
        };

        Self {
            atom,
            kind,
            is_enumerable,
            is_exotic,
        }
    }
}

impl ObjectImpl {
    /// Query all own properties of object `self` and call `callback` for each property key.
    ///
    /// This method handles following types of properties:
    /// - Regular object properties
    /// - Array indices (for fast arrays)
    /// - String object indices
    /// - Exotic object properties
    ///
    /// Important:
    /// - Property atoms are not cloned when passed to callback
    /// - Iteration stops immediately in case `callback` returned an Err()
    ///
    /// Property order:
    /// 1. In defination order
    /// 1. Then indices (if is array or string)
    /// 1. Then exotic properties (if has exotic `get_own_property_names`)
    pub fn own_property_names_foreach(
        &mut self,
        ctx: &mut ScopedContext,
        flags: i32,
        mut callback: impl FnMut(&mut ScopedContext, PropertyKey) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let enum_only = flags & JS_GPN_ENUM_ONLY != 0;
        let sh = self.shape().unwrap();

        // 1. shape props
        for (idx, prs) in sh.prop_defs.iter() {
            if !prs.atom.is_null() && !matches!(self.prop_slot(idx), JSProperty::Deleted) {
                let is_enumerable = prs.flags() & JS_PROP_ENUMERABLE != 0;
                let kind = prs.atom.desc_kind(ctx.rt());

                if (!enum_only || is_enumerable) && ((flags >> (kind as u8)) & 1 != 0) {
                    // need to raise an exception in case of the module
                    // name space (implicit GetOwnProperty)
                    if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF
                        && flags & (JS_GPN_ENUM_ONLY | JS_GPN_SET_ENUM) != 0
                        && let JSProperty::VarRef(vref) = self.prop_slot(idx)
                        && unlikely(vref.value().is_uninitialized())
                    {
                        return Err(JS_ThrowReferenceErrorUninitialized(ctx, prs.atom));
                    }

                    let pk = PropertyKey::new(ctx, prs.atom, is_enumerable, false);
                    callback(ctx, pk)?;
                }
            }
        }

        // 2. exotic or array index
        let obj_flags = self.flags();
        if obj_flags.is_exotic() {
            if obj_flags.is_fast_array() {
                if flags & JS_GPN_STR_OR_INT_MASK != 0 {
                    for i in 0..self.fast_array_count() {
                        callback(
                            ctx,
                            PropertyKey {
                                atom: Atom::new_small_int(i),
                                is_enumerable: true,
                                kind: PropertyKeyKind::ArrayIndex(i),
                                is_exotic: false,
                            },
                        )?;
                    }
                }
            } else if self.class_id() == JS_CLASS_STRING {
                // String object
                if flags & JS_GPN_STR_OR_INT_MASK != 0 {
                    let buf_len = self.payload().string().unwrap().len();
                    for i in 0..buf_len {
                        callback(
                            ctx,
                            PropertyKey {
                                atom: Atom::new_small_int(i as u32),
                                is_enumerable: true,
                                kind: PropertyKeyKind::ArrayIndex(i as u32),
                                is_exotic: false,
                            },
                        )?;
                    }
                }
            } else if let Some(f) = ctx.rt().class_registry[self.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|m| m.get_own_property_names)
            {
                // Note: exotic keys comes after the object own properties.
                let mut has_err = false;

                for pk in f(ctx, Object::from_ref(self))? {
                    if !has_err {
                        let atom_kind = pk.atom.desc_kind(ctx.rt());
                        if (flags >> (atom_kind as u8)) & 1 != 0 {
                            let mut is_enumerable = false;

                            if flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY) != 0
                                && let Some(d) = self.get_own_property(ctx, pk.atom)?
                            {
                                is_enumerable = d.flags & JS_PROP_ENUMERABLE != 0;
                            }

                            if is_enumerable || flags & JS_GPN_ENUM_ONLY == 0 {
                                let isa = if let Some(i) = pk.atom.to_array_index(ctx.rt()) {
                                    PropertyKeyKind::ArrayIndex(i)
                                } else if atom_kind == AtomDescKind::JS_ATOM_KIND_STRING {
                                    PropertyKeyKind::String
                                } else {
                                    PropertyKeyKind::Symbol
                                };
                                has_err = callback(
                                    ctx,
                                    PropertyKey {
                                        atom: pk.atom,
                                        kind: isa,
                                        is_enumerable: pk.is_enumerable,
                                        is_exotic: true,
                                    },
                                )
                                .is_err();
                            }
                        }
                    }
                }

                if unlikely(has_err) {
                    return Err(-1);
                }
            }
        }

        Ok(())
    }

    /// Gets all own property names of `self` object.
    ///
    /// Returns a sorted list of property names info:
    /// - Property names (numbers, strings, symbols). Note: The atom was cloned, it should be freed by caller.
    /// - Sorted by: numbers first, then strings, symbols, and exotic names.
    /// - Can be filtered using flags
    pub fn get_own_property_names(
        &mut self,
        ctx: &mut ScopedContext,
        flags: i32,
    ) -> Result<Box<[PropertyKey]>, i32> {
        let mut lst = Vec::<PropertyKey>::with_capacity(16);
        let mut sort_cnt = 0;

        self.own_property_names_foreach(ctx, flags, |cx, pp| {
            if unlikely(lst.len() == lst.capacity()) {
                lst.try_reserve(1).map_err(|_| cx.throw_out_of_memory())?;
            }

            if !pp.is_exotic {
                sort_cnt += 1;
            }

            lst.push(pp);

            Ok(())
        })?;

        // Note: sort in index, string, symbol segments
        if sort_cnt > 0 {
            lst[0..sort_cnt].sort_by(|p1, p2| match (p1.kind, p2.kind) {
                // index vs index: order by integer
                (PropertyKeyKind::ArrayIndex(i1), PropertyKeyKind::ArrayIndex(i2)) => i1.cmp(&i2),

                // index is always prior to string or symbol
                (PropertyKeyKind::ArrayIndex(_), _) => std::cmp::Ordering::Less,
                (_, PropertyKeyKind::ArrayIndex(_)) => std::cmp::Ordering::Greater,

                // string vs string, symbol vs symbol: in defined order.
                (PropertyKeyKind::String, PropertyKeyKind::String)
                | (PropertyKeyKind::Symbol, PropertyKeyKind::Symbol) => std::cmp::Ordering::Equal,

                // string is always prior to symbol
                (PropertyKeyKind::String, PropertyKeyKind::Symbol) => std::cmp::Ordering::Less,
                (PropertyKeyKind::Symbol, PropertyKeyKind::String) => std::cmp::Ordering::Greater,
            });
        }

        Ok(lst.into_boxed_slice())
    }

    /// Gets object's own properties \[key, value\] pairs.
    /// Returns list of property \[key, value\] pairs, sorted according to js specification.
    ///
    /// Arguments
    /// * `ctx` - JavaScript context
    /// * `flags` - Property filter flags
    /// * `kind` - Iteration mode (keys, values, or entries)
    ///
    ///
    /// Similar to JavaScript's Object.keys(), Object.values(), Object.entries()
    pub fn get_own_property_kvs(
        &mut self,
        ctx: &mut ScopedContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> Result<Box<[(JSValue, JSValue)]>, i32> {
        let pkeys = self.get_own_property_names(ctx, flags & !JS_GPN_ENUM_ONLY)?;

        let mut ret = Vec::<(JSValue, JSValue)>::new();
        ret.try_reserve_exact(pkeys.len()).map_err(|_| ctx.throw_out_of_memory())?;

        for pk in pkeys.iter() {
            if flags & JS_GPN_ENUM_ONLY != 0 {
                match self.get_own_property(ctx, pk.atom)? {
                    Some(desc) => {
                        if desc.flags & JS_PROP_ENUMERABLE == 0 {
                            continue; // skip non enumerable
                        }
                    }
                    None => {
                        continue;
                    }
                }
            }

            let mut key = JSValue::UNDEFINED;
            let mut val = JSValue::UNDEFINED;
            match kind {
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                    key = pk.atom.to_value(ctx)?;
                }
                JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                    val = self.get_property(ctx, pk.atom)?;
                }
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                    key = pk.atom.to_value(ctx)?;
                    val = self.get_property(ctx, pk.atom)?;
                }
            }
            ret.push((key, val));
        }

        Ok(ret.into_boxed_slice())
    }
}

impl JSValue {
    /// Gets object's own properties \[key, value\] pairs.
    ///
    /// Returns list of \[key, value\] pairs, sorted according to js specification.
    ///
    /// ##Arguments##
    /// * `ctx` - JavaScript context
    /// * `flags` - Property filter flags
    /// * `kind` - Iteration mode (keys, values, or entries)
    ///
    ///
    /// Similar to JavaScript's Object.keys(), Object.values(), Object.entries()
    pub fn get_own_property_kvs(
        &self,
        ctx: &mut ScopedContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> Result<Box<[(JSValue, JSValue)]>, i32> {
        self.objectify(ctx)
            .and_then(|mut o| o.get_own_property_kvs(ctx, flags, kind))
    }

    /// Returns an value (Array object) containing the own properties of `self` value
    /// based on the specified iteration kind.
    ///
    /// This method converts the value to an object and returns either:
    /// - An array of property keys (for kind = KEY)
    /// - An array of property values (for kind = VALUE)
    /// - An array of [key, value] pairs (for kind = KEY_AND_VALUE)
    pub(crate) fn build_own_property_kvs_array(
        &self,
        ctx: &mut ScopedContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSResult {
        let kvs = self.get_own_property_kvs(ctx, flags, kind)?;
        let mut lst = alloc_fast_array_slots(ctx, kvs.len())?;

        for (key, val) in kvs {
            match kind {
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                    lst.push(key);
                }
                JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                    lst.push(val);
                }
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                    let o = crate::array::Array::from_values(ctx, vec![key, val])?;
                    lst.push(o.into());
                }
            }
        }

        crate::array::Array::from_values(ctx, lst).map(|a| a.into())
    }
}
