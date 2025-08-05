use crate::{
    JS_CLASS_STRING,
    atom::{AtomDescKind, JS_ATOM_NULL, JS_FreeAtom, JSAtom},
    context::JSContext,
    error::JSResult,
    iterator::JSIteratorKindEnum,
    mem::js_free,
    object::JSObject,
    string::JSString,
    value::JSValue,
};

use super::{JS_GPN_ENUM_ONLY, JS_GPN_STRING_MASK, JS_PROP_ENUMERABLE, JS_PROP_THROW};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyEnumIsa {
    Index(u32),
    String,
    Symbol,
}

#[derive(Debug)]
pub struct PropertyEnum {
    pub atom: JSAtom,
    pub is_enumerable: bool,
    pub isa: PropertyEnumIsa,
    pub exotic: bool,
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
        mut callback: impl FnMut(&JSContext, PropertyEnum) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let enum_only = flags & JS_GPN_ENUM_ONLY != 0;

        for (_, prs) in self.shape().unwrap().0.borrow().prop_defs.iter() {
            if prs.atom != JS_ATOM_NULL {
                let is_enumerable = prs.flags() & JS_PROP_ENUMERABLE != 0;
                let kind = prs.atom.kind(ctx.rt());

                if (!enum_only || is_enumerable) && ((flags >> (kind as u8)) & 1 != 0) {
                    callback(
                        ctx,
                        PropertyEnum {
                            atom: prs.atom, // NOTE: atom is not duplicated here.
                            isa: if let Some(num_key) = prs.atom.to_array_index(ctx.rt()) {
                                PropertyEnumIsa::Index(num_key as u32)
                            } else if kind == AtomDescKind::JS_ATOM_KIND_STRING {
                                PropertyEnumIsa::String
                            } else {
                                PropertyEnumIsa::Symbol
                            },
                            is_enumerable,
                            exotic: false,
                        },
                    )?;
                }
            }
        }

        if self.flags().is_exotic() {
            if self.flags().is_fast_array() {
                if (flags & JS_GPN_STRING_MASK) != 0 {
                    for i in 0..self.array_length_unchecked() {
                        callback(
                            ctx,
                            PropertyEnum {
                                atom: JSAtom::new_u32(i as u32),
                                is_enumerable: true,
                                isa: PropertyEnumIsa::Index(i as u32),
                                exotic: false,
                            },
                        )?;
                    }
                }
            } else if self.class_id() == JS_CLASS_STRING {
                if flags & JS_GPN_STRING_MASK != 0 {
                    let strlen = TryInto::<JSString>::try_into(self.clone()).unwrap().len();
                    for i in 0..strlen {
                        callback(
                            ctx,
                            PropertyEnum {
                                atom: JSAtom::new_u32(i as u32),
                                is_enumerable: true,
                                isa: PropertyEnumIsa::Index(i as u32),
                                exotic: false,
                            },
                        )?;
                    }
                }
            } else if let Some(f) = unsafe {
                ctx.rt().class_defs[self.class_id() as usize]
                    .exotic
                    .as_ref()
                    .and_then(|xm| xm.get_own_property_names)
            } {
                let mut tab_exotic = std::ptr::null_mut::<PropertyEnum>();
                let mut exotic_count: u32 = 0;
                if f(
                    ctx,
                    &mut tab_exotic,
                    &mut exotic_count,
                    JSValue::Object(self.clone()),
                ) != 0
                {
                    return Err(-1);
                }

                // Note: exotic keys comes after the object own properties.
                let mut has_err = false;

                for i in 0..exotic_count {
                    let atom = unsafe { (*tab_exotic.offset(i as isize)).atom };

                    if !has_err {
                        let is_enumerable =
                            unsafe { (*tab_exotic.offset(i as isize)).is_enumerable };
                        let kind = atom.kind(ctx.rt());
                        if (flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable)
                            && ((flags >> (kind as u8)) & 1) != 0
                        {
                            let isa = if let Some(num_key) = atom.to_array_index(ctx.rt()) {
                                PropertyEnumIsa::Index(num_key as u32)
                            } else if kind == AtomDescKind::JS_ATOM_KIND_STRING {
                                PropertyEnumIsa::String
                            } else {
                                PropertyEnumIsa::Symbol
                            };
                            has_err = callback(
                                ctx,
                                PropertyEnum {
                                    atom,
                                    is_enumerable,
                                    isa,
                                    exotic: true,
                                },
                            )
                            .is_err();
                        }
                    }

                    JS_FreeAtom(ctx, atom);
                }

                unsafe {
                    js_free(ctx, tab_exotic.cast());
                }

                if has_err {
                    return Err(-1);
                }
            }
        }

        Ok(())
    }

    /// Gets all property names that belong to this object
    ///
    /// Returns a sorted list containing:
    /// - Property names (numbers, strings, symbols)
    /// - Sorted by: numbers first, then strings, symbols, and exotic names
    /// - Each atom is cloned (dup) so that it needs to be freed by caller
    /// - Can be filtered using flags
    pub(crate) fn query_own_property_names(
        &self,
        ctx: &JSContext,
        flags: i32,
    ) -> Result<Box<[PropertyEnum]>, i32> {
        let mut lst = Vec::<PropertyEnum>::with_capacity(16);
        let mut sort_cnt = 0;

        let r = self.own_property_names_foreach(ctx, flags, |cx, pp| {
            if lst.len() == lst.capacity() {
                lst.try_reserve_exact(16).map_err(|_| {
                    cx.throw_out_of_memory();
                    -1
                })?;
            }

            if !pp.exotic {
                sort_cnt += 1;
            }

            pp.atom.dup(cx.rt()); // dup atom
            lst.push(pp);

            Ok(())
        });

        if r.is_err() {
            // free lst elements' atom
            lst.iter().for_each(|p| {
                JS_FreeAtom(ctx, p.atom);
            });
            return Err(-1);
        }

        // Note: sort owned number, string, symbol only, exotic keys are not sorted.
        if sort_cnt > 0 {
            lst[0..sort_cnt].sort_by(|p1, p2| match (p1.isa, p2.isa) {
                (PropertyEnumIsa::Index(i1), PropertyEnumIsa::Index(i2)) => {
                    i1.cmp(&i2) // for both index, order by integer
                }
                (
                    PropertyEnumIsa::String | PropertyEnumIsa::Symbol,
                    PropertyEnumIsa::String | PropertyEnumIsa::Symbol,
                ) => {
                    // string vs symbol: keep insert sequence
                    std::cmp::Ordering::Equal
                }
                (PropertyEnumIsa::Index(_), _) => {
                    // index always comes before string or symbol
                    std::cmp::Ordering::Less
                }
                (_, PropertyEnumIsa::Index(_)) => {
                    // index always comes before string or symbol
                    std::cmp::Ordering::Greater
                }
            });
        }

        Ok(lst.into_boxed_slice())
    }

    /// Gets an array containing object properties based on specified iteration mode
    ///
    /// # Arguments
    /// * `ctx` - JavaScript context
    /// * `flags` - Property filter flags
    /// * `kind` - Iteration mode (keys, values, or entries)
    ///
    /// # Returns
    /// * Array of property keys, values or [key,value] pairs
    /// * Exception if operation fails
    ///
    /// Similar to JavaScript's Object.keys(), Object.values(), Object.entries()
    pub(crate) fn get_own_property_names_ex(
        &self,
        ctx: &JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSResult {
        // make result array
        let ret_array = ctx.create_array()?;
        let lst = self.query_own_property_names(ctx, flags & !JS_GPN_ENUM_ONLY)?;

        let mut idx: u32 = 0;
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

            let val: JSValue;
            match kind {
                JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                    val = if let Ok(v) = name.atom.to_string_or_symbol(ctx, false) {
                        v
                    } else {
                        has_err = true;
                        break;
                    };
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
                    val = if let Ok(a) = ctx.create_array() {
                        a
                    } else {
                        has_err = true;
                        break;
                    };

                    let key = if let Ok(v) = name.atom.to_string_or_symbol(ctx, false) {
                        v
                    } else {
                        has_err = true;
                        break;
                    };
                    if val
                        .create_data_property_u32(ctx, 0, key, JS_PROP_THROW)
                        .is_err()
                    {
                        has_err = true;
                        break;
                    }

                    if let Ok(v2) = self.get_property(ctx, name.atom) {
                        if val
                            .create_data_property_u32(ctx, 1, v2, JS_PROP_THROW)
                            .is_err()
                        {
                            has_err = true;
                            break;
                        }
                    } else {
                        has_err = true;
                        break;
                    }
                }
            }

            // append to result array
            if ret_array
                .create_data_property_u32(ctx, idx as _, val, 0)
                .is_err()
            {
                has_err = true;
                break;
            }

            idx += 1;
        } // for

        for name in lst.iter() {
            JS_FreeAtom(ctx, name.atom);
        }

        if has_err { Err(-1) } else { Ok(ret_array) }
    }
}

impl JSValue {
    /// Returns an value (Array object) containing the own properties of `self` value
    /// based on the specified iteration kind.
    ///
    /// This method converts the value to an object and returns either:
    /// - An array of property keys (for kind = KEY)
    /// - An array of property values (for kind = VALUE)
    /// - An array of [key, value] pairs (for kind = KEY_AND_VALUE)
    ///
    /// Properties are sorted according to the standard JavaScript property enumeration order.
    #[inline]
    pub fn get_own_property_names2(
        &self,
        ctx: &JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSResult {
        self.coerce_to_object(ctx)
            .and_then(|o| o.get_own_property_names_ex(ctx, flags, kind))
    }
}
