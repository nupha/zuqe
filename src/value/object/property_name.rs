use crate::{
    array::JS_NewArray,
    atom::{JSAtom, JSAtomKindEnum, JS_AtomIsArrayIndex, JS_FreeAtom},
    context::JSContext,
    iterator::JSIteratorKindEnum,
    mem::js_free,
    value::{
        js_string_obj_get_length, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_VALUE_GET_OBJ,
    },
    JS_CLASS_STRING, JS_MKPTR,
};

use super::{
    js_free_desc, JSObject, JSPropertyDescriptor, JS_GetOwnPropertyInternal, JS_ToObject,
    JS_GPN_ENUM_ONLY, JS_GPN_STRING_MASK, JS_PROP_ENUMERABLE, JS_PROP_THROW,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropertyEnumIsa {
    Index(u32),
    String,
    Symbol,
}

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
    pub unsafe fn own_property_names_foreach(
        &self,
        ctx: &mut JSContext,
        flags: i32,
        mut callback: impl FnMut(&mut JSContext, PropertyEnum) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let mut num_key = 0;

        for (&atom, prs) in self.shape().unwrap().props.iter() {
            let is_enumerable = prs.flags() & JS_PROP_ENUMERABLE != 0;
            let kind = atom.kind(ctx.rt());

            if (flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable) && (flags >> (kind as u8)) & 1 != 0
            {
                let isa = if JS_AtomIsArrayIndex(ctx, &mut num_key, atom) {
                    PropertyEnumIsa::Index(num_key)
                } else if kind == JSAtomKindEnum::JS_ATOM_KIND_STRING {
                    PropertyEnumIsa::String
                } else {
                    PropertyEnumIsa::Symbol
                };
                callback(
                    ctx,
                    PropertyEnum {
                        atom,
                        is_enumerable,
                        isa,
                        exotic: false,
                    },
                )?;
            }
        }

        if self.detailed.is_exotic() {
            if self.detailed.fast_array() {
                if (flags & JS_GPN_STRING_MASK) != 0 {
                    for i in 0..self.array_length_unchecked() {
                        callback(
                            ctx,
                            PropertyEnum {
                                atom: JSAtom::from_u32(i as u32),
                                is_enumerable: true,
                                isa: PropertyEnumIsa::Index(i as u32),
                                exotic: false,
                            },
                        )?;
                    }
                }
            } else if self.class_id() == JS_CLASS_STRING {
                if flags & JS_GPN_STRING_MASK != 0 {
                    let strlen = js_string_obj_get_length(JS_MKPTR!(
                        JSTag::Object,
                        (self as *const JSObject as *mut JSObject).cast()
                    ));
                    for i in 0..strlen {
                        callback(
                            ctx,
                            PropertyEnum {
                                atom: JSAtom::from_u32(i as u32),
                                is_enumerable: true,
                                isa: PropertyEnumIsa::Index(i as u32),
                                exotic: false,
                            },
                        )?;
                    }
                }
            } else if let Some(f) = ctx.rt().class_array[self.class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|em| em.get_own_property_names)
            {
                let mut tab_exotic = std::ptr::null_mut::<PropertyEnum>();
                let mut exotic_count: u32 = 0;
                if f(
                    ctx,
                    &mut tab_exotic,
                    &mut exotic_count,
                    JS_MKPTR!(
                        JSTag::Object,
                        (self as *const JSObject as *mut JSObject).cast()
                    ),
                ) != 0
                {
                    return Err(-1);
                }

                // Note: exotic keys comes after the object own properties.
                let mut has_err = false;

                for i in 0..exotic_count {
                    let atom = (*tab_exotic.offset(i as isize)).atom;

                    if !has_err {
                        let is_enumerable = (*tab_exotic.offset(i as isize)).is_enumerable;
                        let kind = atom.kind(ctx.rt());
                        if (flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable)
                            && ((flags >> (kind as u8)) & 1) != 0
                        {
                            let isa = if JS_AtomIsArrayIndex(ctx, &mut num_key, atom) {
                                PropertyEnumIsa::Index(num_key)
                            } else if kind == JSAtomKindEnum::JS_ATOM_KIND_STRING {
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

                js_free(ctx, tab_exotic.cast());

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
    pub unsafe fn get_own_property_names_internal(
        &self,
        ctx: &mut JSContext,
        flags: i32,
    ) -> Result<Box<[PropertyEnum]>, i32> {
        let mut lst = Vec::<PropertyEnum>::with_capacity(16);
        let mut sort_cnt = 0;

        let r = self.own_property_names_foreach(ctx, flags, |cx, pp| {
            if lst.len() == lst.capacity() {
                if lst.try_reserve_exact(16).is_err() {
                    cx.JS_ThrowOutOfMemory();
                    return Err(-1);
                }
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
    pub fn get_own_property_names2(
        &self,
        ctx: &mut JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSValue {
        // make result array
        let ret_array = unsafe { JS_NewArray(ctx).into_rc(ctx.rt()) };
        if ret_array.is_exception() {
            return JSValue::EXCEPTION;
        }

        if let Ok(lst) =
            unsafe { self.get_own_property_names_internal(ctx, flags & !JS_GPN_ENUM_ONLY) }
        {
            let has_err: bool = 'outer: loop {
                let mut idx: u32 = 0;

                for name in lst.iter() {
                    unsafe {
                        if flags & JS_GPN_ENUM_ONLY != 0 {
                            let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                            let res = JS_GetOwnPropertyInternal(
                                ctx,
                                &mut desc,
                                self as *const JSObject as *mut JSObject,
                                name.atom,
                            );
                            if res < 0 {
                                break 'outer true;
                            }
                            if res == 0 {
                                continue;
                            }
                            js_free_desc(ctx, &mut desc);
                            if desc.flags & JS_PROP_ENUMERABLE == 0 {
                                continue; // skip non enumerable
                            }
                        }

                        let val: JSValue;
                        match kind {
                            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                                val = name.atom.to_value(ctx, false);
                                if val.is_exception() {
                                    break 'outer true;
                                }
                            }
                            JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                                val = JS_MKPTR!(
                                    JSTag::Object,
                                    (self as *const JSObject as *mut JSObject).cast()
                                )
                                .get_property(ctx, name.atom);
                                if val.is_exception() {
                                    break 'outer true;
                                }
                            }
                            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                                val = JS_NewArray(ctx);
                                if val.is_exception() {
                                    break 'outer true;
                                }
                                let key = name.atom.to_value(ctx, false);
                                if key.is_exception() {
                                    JS_FreeValue(ctx, val);
                                    break 'outer true;
                                }
                                if val
                                    .create_data_property_u32(ctx, 0, key, JS_PROP_THROW)
                                    .is_err()
                                {
                                    JS_FreeValue(ctx, val);
                                    break 'outer true;
                                }

                                let value = JS_MKPTR!(
                                    JSTag::Object,
                                    (self as *const JSObject as *mut JSObject).cast()
                                )
                                .get_property(ctx, name.atom);
                                if value.is_exception() {
                                    JS_FreeValue(ctx, val);
                                    break 'outer true;
                                }
                                if val
                                    .create_data_property_u32(ctx, 1, value, JS_PROP_THROW)
                                    .is_err()
                                {
                                    JS_FreeValue(ctx, val);
                                    break 'outer true;
                                }
                            }
                        }

                        // append to result array
                        if ret_array
                            .create_data_property_u32(ctx, idx as _, val, 0)
                            .is_err()
                        {
                            break 'outer true;
                        }

                        idx += 1;
                    }
                } // for

                break 'outer false;
            };

            for name in lst.iter() {
                unsafe {
                    JS_FreeAtom(ctx, name.atom);
                }
            }
            if has_err {
                JSValue::EXCEPTION
            } else {
                ret_array.into()
            }
        } else {
            JSValue::EXCEPTION
        }
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
    /// Returns JSValue::EXCEPTION if the operation fails.
    pub fn get_own_property_names2(
        self,
        ctx: &mut JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSValue {
        let obj = unsafe { JS_ToObject(ctx, self).into_rc(ctx.rt()) };
        if obj.is_exception() {
            JSValue::EXCEPTION
        } else {
            unsafe { obj.object().get_own_property_names2(ctx, flags, kind) }
        }
    }
}

// ptab and its atoms must be freed by the user.
#[deprecated(note = "use JSObject::get_own_property_names_internal instead")]
pub unsafe fn JS_GetOwnPropertyNamesInternal(
    ctx: *mut JSContext,
    ptab: *mut *mut PropertyEnum,
    plen: *mut u32,
    p: *mut JSObject,
    flags: i32,
) -> Result<(), i32> {
    // clear pointer for consistency in case of failure
    *ptab = std::ptr::null_mut();
    *plen = 0;

    let res = (*p).get_own_property_names_internal(ctx.as_mut().unwrap(), flags)?;
    let mut lst = res;
    *plen = lst.len() as u32;
    *ptab = lst.as_mut_ptr();
    Box::leak(lst);
    Ok(())
}

#[deprecated(note = "use JSValue::get_own_property_names2() instead")]
pub unsafe fn JS_GetOwnPropertyNames2(
    ctx: *mut JSContext,
    target: JSValueConst,
    flags: i32,
    kind: JSIteratorKindEnum,
) -> JSValue {
    target.get_own_property_names2(ctx.as_mut().unwrap(), flags, kind)
}

// #[deprecated]
// pub unsafe fn js_free_prop_enum(ctx: *mut JSContext, tab: *mut PropertyEnum, len: usize) {
//     if !tab.is_null() {
//         for i in 0..len {
//             JS_FreeAtom(ctx, (*tab.add(i)).atom);
//         }
//         js_free(ctx, tab.cast());
//     }
// }
