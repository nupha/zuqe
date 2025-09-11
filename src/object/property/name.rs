use crate::{
    JS_CLASS_STRING,
    atom::{AtomDescKind, JS_ATOM_NULL, JS_FreeAtom, JSAtom},
    context::JSContext,
    error::{JS_ThrowReferenceErrorUninitialized, JSResult},
    iterator::JSIteratorKindEnum,
    object::{JS_PROP_TMASK, JS_PROP_VARREF, JSObject, JSProperty},
    string::JSString,
    value::JSValue,
};

use super::{JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_GPN_SET_ENUM, JS_PROP_ENUMERABLE};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyKeyKind {
    String,
    Symbol,
    /// in range \[0, u32::MAX)
    ArrayIndex(u32),
}

#[derive(Debug)]
pub struct PropertyKey {
    pub atom: JSAtom,
    pub isa: PropertyKeyKind,
    pub is_enumerable: bool,
    pub is_exotic: bool,
}

impl PropertyKey {
    /// Note: `atom` is used **AS-IS**: it's ownership is transparent, it's neither cloned nor consumed inside.
    pub fn new(ctx: &JSContext, atom: JSAtom, is_enumerable: bool, is_exotic: bool) -> Self {
        let kind = atom.kind(ctx.rt());

        let isa = if let Some(idx) = atom.to_array_index(ctx.rt()) {
            PropertyKeyKind::ArrayIndex(idx as u32)
        } else if kind == AtomDescKind::JS_ATOM_KIND_STRING {
            PropertyKeyKind::String
        } else {
            PropertyKeyKind::Symbol
        };

        Self {
            atom,
            isa,
            is_enumerable,
            is_exotic,
        }
    }
}

impl JSObject {
    /// Iterates over all own properties of object `self` and executes `callback` for each property.
    ///
    /// This method handles three types of properties:
    /// - Regular object properties
    /// - Array indices (for fast arrays)
    /// - String object indices
    /// - Exotic object properties
    ///
    /// The `callback` receives property information including:
    /// - The property name (as an atom)
    /// - Whether it is enumerable
    /// - The property type (index, string, or symbol)
    ///
    /// Important:
    /// - Property atoms are not cloned when passed to callback
    /// - Iteration stops immediately if callback returns an error
    ///
    /// Property order:
    /// 1. In defination order
    /// 1. Then indices (if is array or string)
    /// 1. Then exotic properties (if has exotic `get_own_property_names`)
    pub fn own_property_names_foreach(
        &self,
        ctx: &JSContext,
        flags: i32,
        mut callback: impl FnMut(&JSContext, PropertyKey) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let enum_only = flags & JS_GPN_ENUM_ONLY != 0;

        // 1. shape props
        for (i, (_, prs)) in self
            .shape()
            .unwrap()
            .0
            .borrow()
            .prop_defs
            .iter()
            .enumerate()
        {
            if prs.atom != JS_ATOM_NULL {
                let is_enumerable = prs.flags() & JS_PROP_ENUMERABLE != 0;
                let kind = prs.atom.kind(ctx.rt());

                if (!enum_only || is_enumerable) && ((flags >> (kind as u8)) & 1 != 0) {
                    // need to raise an exception in case of the module
                    // name space (implicit GetOwnProperty)
                    if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF
                        && flags & (JS_GPN_ENUM_ONLY | JS_GPN_SET_ENUM) != 0
                    {
                        if let JSProperty::VarRef(vref) =
                            self.0.prop_values.borrow().get(i).unwrap()
                        {
                            if vref.value().is_uninitialized() {
                                JS_ThrowReferenceErrorUninitialized(ctx, prs.atom);
                                return Err(-1);
                            }
                        }
                    }

                    callback(ctx, PropertyKey::new(ctx, prs.atom, is_enumerable, false))?;
                }
            }
        }

        // 2. exotic or array index
        let obj_flags = self.flags();
        if obj_flags.is_exotic() {
            if obj_flags.is_fast_array() {
                if flags & JS_GPN_INDEX_MASK != 0 {
                    for i in 0..self.fast_array_count() {
                        callback(
                            ctx,
                            PropertyKey {
                                atom: JSAtom::new_small_int(i as u32),
                                is_enumerable: true,
                                isa: PropertyKeyKind::ArrayIndex(i as u32),
                                is_exotic: false,
                            },
                        )?;
                    }
                }
            } else if self.class_id() == JS_CLASS_STRING {
                if flags & JS_GPN_INDEX_MASK != 0 {
                    let buf_len = TryInto::<JSString>::try_into(self.clone()).unwrap().len();

                    for i in 0..buf_len {
                        callback(
                            ctx,
                            PropertyKey {
                                atom: JSAtom::new_small_int(i as u32),
                                is_enumerable: true,
                                isa: PropertyKeyKind::ArrayIndex(i as u32),
                                is_exotic: false,
                            },
                        )?;
                    }
                }
            } else if let Some(f) = unsafe {
                ctx.rt().class_defs[self.class_id() as usize]
                    .exotic
                    .as_ref()
                    .and_then(|m| m.get_own_property_names)
            } {
                // Note: exotic keys comes after the object own properties.
                let mut has_err = false;
                for pk in f(ctx, self)? {
                    if !has_err {
                        let atom_kind = pk.atom.kind(ctx.rt());
                        if (flags >> (atom_kind as u8)) & 1 != 0 {
                            let mut is_enumerable = false;
                            if flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY) != 0 {
                                if let Some(d) = self.get_own_property(ctx, pk.atom)? {
                                    is_enumerable = d.flags & JS_PROP_ENUMERABLE != 0;
                                }
                            }

                            if is_enumerable || flags & JS_GPN_ENUM_ONLY == 0 {
                                let isa = if let Some(i) = pk.atom.to_array_index(ctx.rt()) {
                                    PropertyKeyKind::ArrayIndex(i as u32)
                                } else if atom_kind == AtomDescKind::JS_ATOM_KIND_STRING {
                                    PropertyKeyKind::String
                                } else {
                                    PropertyKeyKind::Symbol
                                };
                                has_err = callback(
                                    ctx,
                                    PropertyKey {
                                        atom: pk.atom,
                                        isa,
                                        is_enumerable: pk.is_enumerable,
                                        is_exotic: true,
                                    },
                                )
                                .is_err();
                            }
                        }
                    }

                    JS_FreeAtom(ctx, pk.atom);
                }

                if has_err {
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
        &self,
        ctx: &JSContext,
        flags: i32,
    ) -> Result<Box<[PropertyKey]>, i32> {
        let mut lst = Vec::<PropertyKey>::with_capacity(16);
        let mut sort_cnt = 0;

        let r = self.own_property_names_foreach(ctx, flags, |cx, pp| {
            if lst.len() == lst.capacity() {
                lst.try_reserve_exact(8).map_err(|_| {
                    cx.throw_out_of_memory();
                    -1
                })?;
            }

            if !pp.is_exotic {
                sort_cnt += 1;
            }

            pp.atom.dup(cx.rt()); // dup atom
            lst.push(pp);

            Ok(())
        });

        if r.is_err() {
            // free atoms
            lst.iter().for_each(|p| {
                JS_FreeAtom(ctx, p.atom);
            });
            return Err(-1);
        }

        // Note: sort in index, string, symbol segments
        if sort_cnt > 0 {
            lst[0..sort_cnt].sort_by(|p1, p2| match (p1.isa, p2.isa) {
                // index vs index: order by integer
                (PropertyKeyKind::ArrayIndex(i1), PropertyKeyKind::ArrayIndex(i2)) => i1.cmp(&i2),

                // index is always prior to string or symbol
                (PropertyKeyKind::ArrayIndex(_), _) => std::cmp::Ordering::Less,
                (_, PropertyKeyKind::ArrayIndex(_)) => std::cmp::Ordering::Greater,

                // string vs string, symbol vs symbol: in defined order.
                (PropertyKeyKind::String, PropertyKeyKind::String)
                | (PropertyKeyKind::Symbol, PropertyKeyKind::Symbol) => {
                    // // string vs symbol: keep insert sequence
                    std::cmp::Ordering::Equal
                }

                // string is always prior to symbol
                (PropertyKeyKind::String, PropertyKeyKind::Symbol) => std::cmp::Ordering::Less,
                (PropertyKeyKind::Symbol, PropertyKeyKind::String) => std::cmp::Ordering::Greater,
            });
        }

        Ok(lst.into_boxed_slice())
    }

    /// Gets object's own properties [key, value] pairs.
    /// Returns list of property [key,value] pairs, sorted according to js specification.
    ///
    /// Arguments
    /// * `ctx` - JavaScript context
    /// * `flags` - Property filter flags
    /// * `kind` - Iteration mode (keys, values, or entries)
    ///
    ///
    /// Similar to JavaScript's Object.keys(), Object.values(), Object.entries()
    pub fn get_own_property_kvs(
        &self,
        ctx: &JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> Result<Box<[(JSValue, JSValue)]>, i32> {
        let lst = self.get_own_property_names(ctx, flags & !JS_GPN_ENUM_ONLY)?;

        let mut ret = Vec::<(JSValue, JSValue)>::new();
        ret.try_reserve_exact(lst.len()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let mut has_err = false;
        for name in lst.iter() {
            if flags & JS_GPN_ENUM_ONLY != 0 {
                match self.get_own_property(ctx, name.atom) {
                    Ok(Some(desc)) => {
                        if desc.flags & JS_PROP_ENUMERABLE == 0 {
                            continue; // skip non enumerable
                        }
                    }
                    Ok(None) => {
                        continue;
                    }
                    Err(_) => {
                        has_err = true;
                        break;
                    }
                }
            }

            if !has_err {
                let mut key = JSValue::UNDEFINED;
                let mut val = JSValue::UNDEFINED;

                match kind {
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                        if let Ok(v) = name.atom.to_string_or_symbol(ctx) {
                            key = v;
                        } else {
                            has_err = true;
                            break;
                        }
                    }
                    JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                        if let Ok(v) = self.get_property(ctx, name.atom) {
                            val = v;
                        } else {
                            has_err = true;
                            break;
                        }
                    }
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                        if let Ok(v) = name.atom.to_string_or_symbol(ctx) {
                            key = v;
                        } else {
                            has_err = true;
                            break;
                        };
                        if let Ok(v) = self.get_property(ctx, name.atom) {
                            val = v;
                        } else {
                            has_err = true;
                            break;
                        }
                    }
                }

                ret.push((key, val));
            }
        }

        // free atoms
        for name in lst.iter() {
            JS_FreeAtom(ctx, name.atom);
        }

        if has_err {
            Err(-1)
        } else {
            Ok(ret.into_boxed_slice())
        }
    }
}

impl JSValue {
    /// Gets object's own properties [key, value] pairs.
    /// Returns list of property [key,value] pairs, sorted according to js specification.
    ///
    /// Arguments
    /// * `ctx` - JavaScript context
    /// * `flags` - Property filter flags
    /// * `kind` - Iteration mode (keys, values, or entries)
    ///
    ///
    /// Similar to JavaScript's Object.keys(), Object.values(), Object.entries()
    #[inline]
    pub fn get_own_property_kvs(
        &self,
        ctx: &JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> Result<Box<[(JSValue, JSValue)]>, i32> {
        self.to_object(ctx)?.get_own_property_kvs(ctx, flags, kind)
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
        ctx: &JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSResult {
        let kvs = self.get_own_property_kvs(ctx, flags, kind)?;

        let mut lst = Vec::<JSValue>::new();
        lst.try_reserve_exact(kvs.len()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        for (key, val) in kvs {
            match kind {
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                    lst.push(key);
                }
                JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                    lst.push(val);
                }
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                    let o = ctx.create_array_with_values(vec![key, val])?;
                    lst.push(o.into());
                }
            }
        }

        ctx.create_array_with_values(lst).map(|a| a.into())
    }
}
