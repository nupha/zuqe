use std::ptr::NonNull;

use smallvec::SmallVec;

use crate::{
    array::JS_NewArray,
    atom::{JSAtom, JSAtomKindEnum, JS_AtomIsArrayIndex, JS_FreeAtom},
    context::JSContext,
    iterator::JSIteratorKindEnum,
    mem::js_free,
    value::{
        js_string_obj_get_length, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_IsException,
        JS_VALUE_GET_OBJ,
    },
    JS_CLASS_STRING, JS_MKPTR,
};

use super::{
    obj::{JSObject, JS_ToObject},
    JSPropertyDescriptor, JS_CreateDataPropertyUint32, JS_GetOwnPropertyInternal, JS_GetProperty,
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
}

pub struct PropertyEnumList<'a> {
    ctx: &'a mut JSContext,
    list: SmallVec<[PropertyEnum; 32]>,
}

impl<'a> PropertyEnumList<'a> {
    #[inline(always)]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    #[inline(always)]
    pub fn as_slice(&self) -> &[PropertyEnum] {
        &self.list
    }

    /// take ownership of the inner list.
    /// Note: this will give out ownership, thus elements' atom should be freed by caller.
    pub fn into_boxed_slice(mut self) -> Box<[PropertyEnum]> {
        let a = std::mem::replace(&mut self.list, SmallVec::new());
        a.into_boxed_slice()
    }
}

impl<'a> Drop for PropertyEnumList<'a> {
    fn drop(&mut self) {
        for p in self.list.iter() {
            unsafe {
                JS_FreeAtom(self.ctx, p.atom);
            }
        }
    }
}

impl JSObject {
    /// Call closure on each property name of the object.
    /// Note: property name atom is not duplicated when calling callback,
    /// if you want to keep ownership of the atom, dup it by yourself.
    /// When encounter error in retriving property names, returns Err and abort iteration.
    /// When callback returns Err, will return Err immediately, and iteration will be aborted.
    pub unsafe fn own_property_names_foreach(
        &mut self,
        ctx: &mut JSContext,
        flags: i32,
        mut cb: impl FnMut(&mut JSContext, PropertyEnum) -> Result<(), i32>,
    ) -> Result<(), i32> {
        let mut num_key = 0;

        for (&atom, prs) in self.shape_mut().unwrap().props.iter_mut() {
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

                cb(
                    ctx,
                    PropertyEnum {
                        atom,
                        is_enumerable,
                        isa,
                    },
                )?;
            }
        }

        if self.detailed.is_exotic() {
            if self.detailed.fast_array() {
                if (flags & JS_GPN_STRING_MASK) != 0 {
                    for i in 0..self.array_length_unchecked() {
                        cb(
                            ctx,
                            PropertyEnum {
                                atom: JSAtom::from_u32(i as u32),
                                is_enumerable: true,
                                isa: PropertyEnumIsa::Index(i as u32),
                            },
                        )?;
                    }
                }
            } else if self.class_id() == JS_CLASS_STRING {
                if flags & JS_GPN_STRING_MASK != 0 {
                    let strlen = js_string_obj_get_length(JS_MKPTR!(
                        JSTag::Object,
                        (self as *mut JSObject).cast()
                    ));
                    for i in 0..strlen {
                        cb(
                            ctx,
                            PropertyEnum {
                                atom: JSAtom::from_u32(i as u32),
                                is_enumerable: true,
                                isa: PropertyEnumIsa::Index(i as u32),
                            },
                        )?;
                    }
                }
            } else {
                if let Some(f) = ctx.rt().class_array[self.class_id() as usize]
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
                        JS_MKPTR!(JSTag::Object, (self as *mut JSObject).cast()),
                    ) != 0
                    {
                        return Err(-1);
                    }

                    // Note: exotic keys are not reordered and comes after the object own properties.
                    for i in 0..exotic_count {
                        let atom = (*tab_exotic.offset(i as isize)).atom;
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

                            cb(
                                ctx,
                                PropertyEnum {
                                    atom,
                                    is_enumerable,
                                    isa,
                                },
                            )
                            .map_err(|_| {
                                JS_FreeAtom(ctx, atom);
                                -1
                            })?;
                        }

                        JS_FreeAtom(ctx, atom);
                    }

                    js_free(ctx, tab_exotic.cast());
                }
            }
        }

        Ok(())
    }

    pub unsafe fn get_own_property_names_internal(
        &mut self,
        ctx: &mut JSContext,
        flags: i32,
        dup_atom: bool,
    ) -> Result<Box<[PropertyEnum]>, i32> {
        let mut lst = Vec::<PropertyEnum>::with_capacity(16);
        self.own_property_names_foreach(ctx, flags, |_, pp| {
            lst.push(pp);
            Ok(())
        })?;

        lst.sort_by(|p1, p2| match (p1.isa, p2.isa) {
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

        if dup_atom {
            for a in lst.iter_mut() {
                a.atom.dup(ctx.rt());
            }
        }

        Ok(lst.into_boxed_slice())
    }

    // /// SAFETY: returned JSPropertyEnum list must free elements' atom by hand.
    // pub unsafe fn get_own_property_names_internal<'a>(
    //     &'a mut self,
    //     ctx: &'a mut JSContext,
    //     flags: i32,
    // ) -> Result<PropertyEnumList<'a>, i32> {
    //     // to count the number of returned properties
    //     let mut num_keys_count: u32 = 0;
    //     let mut str_keys_count: u32 = 0;
    //     let mut sym_keys_count: u32 = 0;
    //     let mut num_key: u32 = 0;

    //     for (i, (&atom, prs)) in self.shape().unwrap().props.iter().enumerate() {
    //         let is_enumerable = prs.flags() & JS_PROP_ENUMERABLE != 0;
    //         let kind = atom.kind(ctx.rt());

    //         if (flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable) && (flags >> (kind as u8)) & 1 != 0
    //         {
    //             // need to raise an exception in case of the module
    //             // name space (implicit GetOwnProperty)
    //             if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF
    //                 && flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY) != 0
    //             {
    //                 let var_ref = self.prop_values[i as usize].u.var_ref;
    //                 if JS_IsUninitialized(*(*var_ref).pvalue) {
    //                     JS_ThrowReferenceErrorUninitialized(ctx, atom);
    //                     return Err(-1);
    //                 }
    //             }

    //             if JS_AtomIsArrayIndex(ctx, &mut num_key, atom) {
    //                 num_keys_count += 1;
    //             } else if kind == JSAtomKindEnum::JS_ATOM_KIND_STRING {
    //                 str_keys_count += 1;
    //             } else {
    //                 sym_keys_count += 1;
    //             }
    //         }
    //     }

    //     if self.detailed.is_exotic() {
    //         if self.detailed.fast_array() {
    //             if flags & JS_GPN_STRING_MASK != 0 {
    //                 num_keys_count += self.array_length_unchecked() as u32;
    //             }
    //         } else if self.class_id() == JS_CLASS_STRING {
    //             if flags & JS_GPN_STRING_MASK != 0 {
    //                 num_keys_count += js_string_obj_get_length(JS_MKPTR!(
    //                     JSTag::Object,
    //                     (self as *mut JSObject).cast()
    //                 )) as u32;
    //             }
    //         } else {
    //             if let Some(cb) = ctx.rt().class_array[self.class_id() as usize]
    //                 .exotic
    //                 .as_ref()
    //                 .and_then(|em| em.get_own_property_names)
    //             {
    //                 let mut exotic_keys_count: u32 = 0;
    //                 let mut tab_exotic = std::ptr::null_mut::<JSPropertyEnum>();
    //                 let mut exotic_count: u32 = 0;

    //                 if cb(
    //                     ctx,
    //                     &mut tab_exotic,
    //                     &mut exotic_count,
    //                     JS_MKPTR!(JSTag::Object, (self as *mut JSObject).cast()),
    //                 ) != 0
    //                 {
    //                     return Err(-1);
    //                 }

    //                 for i in 0..exotic_count {
    //                     let atom = (*tab_exotic.offset(i as isize)).atom;
    //                     let kind = atom.kind((*ctx).rt());

    //                     if ((flags >> (kind as u8)) & 1) != 0 {
    //                         let mut is_enumerable = false;
    //                         if flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY) != 0 {
    //                             // set the "is_enumerable" field if necessary
    //                             let mut desc: JSPropertyDescriptor = std::mem::zeroed();
    //                             let res = JS_GetOwnPropertyInternal(ctx, &mut desc, self, atom);
    //                             if res < 0 {
    //                                 js_free_prop_enum(ctx, tab_exotic, exotic_count as _);
    //                                 return Err(-1);
    //                             }
    //                             if res != 0 {
    //                                 is_enumerable = (desc.flags & JS_PROP_ENUMERABLE) != 0;
    //                                 js_free_desc(ctx, &mut desc);
    //                             }
    //                             (*tab_exotic.offset(i as isize)).is_enumerable = is_enumerable;
    //                         }

    //                         if flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable {
    //                             exotic_keys_count += 1;
    //                         }
    //                     }
    //                 }
    //             }

    //             if let Some(cb) = ctx.rt().class_array[self.class_id() as usize]
    //                 .exotic
    //                 .as_ref()
    //                 .and_then(|em| em.get_own_property_names)
    //             {
    //                 if cb(
    //                     ctx,
    //                     &mut tab_exotic,
    //                     &mut exotic_count,
    //                     JS_MKPTR!(JSTag::Object, (self as *mut JSObject).cast()),
    //                 ) != 0
    //                 {
    //                     return Err(-1);
    //                 }

    //                 for i in 0..exotic_count {
    //                     let atom = (*tab_exotic.offset(i as isize)).atom;
    //                     let kind = atom.kind((*ctx).rt());

    //                     if ((flags >> (kind as u8)) & 1) != 0 {
    //                         let mut is_enumerable = false;
    //                         if flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY) != 0 {
    //                             // set the "is_enumerable" field if necessary
    //                             let mut desc: JSPropertyDescriptor = std::mem::zeroed();
    //                             let res = JS_GetOwnPropertyInternal(ctx, &mut desc, self, atom);
    //                             if res < 0 {
    //                                 js_free_prop_enum(ctx, tab_exotic, exotic_count as _);
    //                                 return Err(-1);
    //                             }
    //                             if res != 0 {
    //                                 is_enumerable = (desc.flags & JS_PROP_ENUMERABLE) != 0;
    //                                 js_free_desc(ctx, &mut desc);
    //                             }
    //                             (*tab_exotic.offset(i as isize)).is_enumerable = is_enumerable;
    //                         }

    //                         if (!(flags & JS_GPN_ENUM_ONLY) != 0) || is_enumerable {
    //                             exotic_keys_count += 1;
    //                         }
    //                     }
    //                 }
    //             }
    //         }
    //     }

    //     // make result list

    //     let atom_count = num_keys_count + str_keys_count + sym_keys_count + exotic_keys_count;
    //     let mut list = SmallVec::<[JSPropertyEnum; 32]>::new();
    //     if list.try_reserve_exact(atom_count as usize).is_err() {
    //         js_free_prop_enum(ctx, tab_exotic, exotic_count as _);
    //         ctx.JS_ThrowOutOfMemory();
    //         return Err(-1);
    //     }
    //     std::ptr::write_bytes(list.as_mut_ptr(), 0, list.len());
    //     list.set_len(atom_count as usize);

    //     let mut num_index = 0;
    //     let mut str_index = num_keys_count;
    //     let mut sym_index = str_index + str_keys_count;
    //     let mut num_sorted = false;

    //     for (&atom, prs) in self.shape_mut().unwrap().props.iter_mut() {
    //         let is_enumerable = prs.flags() & JS_PROP_ENUMERABLE != 0;
    //         let kind = atom.kind(ctx.rt());
    //         let j: u32;

    //         if (flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable) && (flags >> (kind as u8)) & 1 != 0
    //         {
    //             if JS_AtomIsArrayIndex(ctx, &mut num_key, atom) {
    //                 j = num_index;
    //                 num_index += 1;
    //                 num_sorted = false;
    //             } else if kind == JSAtomKindEnum::JS_ATOM_KIND_STRING {
    //                 j = str_index;
    //                 str_index += 1;
    //             } else {
    //                 j = sym_index;
    //                 sym_index += 1;
    //             };
    //             list[j as usize] = JSPropertyEnum {
    //                 atom: atom.dup(ctx.rt()),
    //                 is_enumerable,
    //             };
    //         }
    //     }

    //     if self.detailed.is_exotic() {
    //         if self.detailed.fast_array() {
    //             if (flags & JS_GPN_STRING_MASK) != 0 {
    //                 for i in 0..self.array_length_unchecked() {
    //                     list[num_index as usize] = JSPropertyEnum {
    //                         atom: JSAtom::from_u32(i as u32),
    //                         is_enumerable: true,
    //                     };
    //                     debug_assert!(list[num_index as usize].atom != JS_ATOM_NULL);
    //                     num_index += 1;
    //                 }
    //             }
    //         } else if self.class_id() == JS_CLASS_STRING {
    //             if flags & JS_GPN_STRING_MASK != 0 {
    //                 let strlen = js_string_obj_get_length(JS_MKPTR!(
    //                     JSTag::Object,
    //                     (self as *mut JSObject).cast()
    //                 ));
    //                 for i in 0..strlen {
    //                     list[num_index as usize] = JSPropertyEnum {
    //                         atom: JSAtom::from_u32(i as u32),
    //                         is_enumerable: true,
    //                     };
    //                     debug_assert!(list[num_index as usize].atom != JS_ATOM_NULL);
    //                     num_index += 1;
    //                 }
    //             }
    //         } else {
    //             // Note: exotic keys are not reordered and comes after the object own properties.
    //             for i in 0..exotic_count {
    //                 let atom = (*tab_exotic.offset(i as isize)).atom;
    //                 let is_enumerable = (*tab_exotic.offset(i as isize)).is_enumerable;
    //                 let kind = atom.kind(ctx.rt());

    //                 if (flags & JS_GPN_ENUM_ONLY == 0 || is_enumerable)
    //                     && ((flags >> (kind as u8)) & 1) != 0
    //                 {
    //                     list[sym_index as usize] = JSPropertyEnum {
    //                         atom,
    //                         is_enumerable,
    //                     };
    //                     sym_index += 1;
    //                 } else {
    //                     JS_FreeAtom(ctx, atom);
    //                 }
    //             }

    //             js_free(ctx, tab_exotic.cast());
    //         }
    //     }

    //     debug_assert_eq!(num_index, num_keys_count);
    //     debug_assert_eq!(str_index, num_keys_count + str_keys_count);
    //     debug_assert_eq!(sym_index, atom_count);

    //     if num_keys_count != 0 && !num_sorted {
    //         // sort by integral key
    //         let arr = &mut list[..num_keys_count as usize];
    //         arr.sort_by(|p1, p2| {
    //             let atom1 = p1.atom;
    //             let atom2 = p2.atom;

    //             let mut v1 = 0u32;
    //             let mut v2 = 0u32;

    //             let atom1_is_integer = JS_AtomIsArrayIndex(ctx, &mut v1, atom1);
    //             let atom2_is_integer = JS_AtomIsArrayIndex(ctx, &mut v2, atom2);
    //             debug_assert!(atom1_is_integer && atom2_is_integer);

    //             v1.cmp(&v2)
    //         });
    //     }

    //     Ok(PropertyEnumList {
    //         ctx,
    //         list: list.into(),
    //     })
    // }
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

    let res = (*p).get_own_property_names_internal(ctx.as_mut().unwrap(), flags, true)?;
    let mut lst = res;
    *plen = lst.len() as u32;
    *ptab = lst.as_mut_ptr();
    Box::leak(lst);
    Ok(())
}

pub unsafe fn JS_GetOwnPropertyNames2(
    ctx: *mut JSContext,
    obj1: JSValueConst,
    flags: i32,
    kind: JSIteratorKindEnum,
) -> JSValue {
    let obj = JS_ToObject(ctx, obj1);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    let p = JS_VALUE_GET_OBJ(obj);
    let mut atoms: *mut PropertyEnum = std::ptr::null_mut();
    let mut len: u32 = 0;

    macro_rules! exception {
        () => {
            // js_free_prop_enum(ctx, atoms, len as usize);
            for item in Box::from_raw(
                std::slice::from_raw_parts_mut(atoms, len as usize) as *mut [PropertyEnum]
            ) {
                JS_FreeAtom(ctx, item.atom);
            }

            JS_FreeValue(ctx, obj);
            return JSValue::EXCEPTION;
        };
    }

    if JS_GetOwnPropertyNamesInternal(ctx, &mut atoms, &mut len, p, flags & !JS_GPN_ENUM_ONLY)
        .is_err()
    {
        exception!();
    }

    let ret_val = JS_NewArray(ctx);
    if ret_val.is_exception() {
        exception!();
    }

    let mut j: u32 = 0;
    for i in 0..len {
        let atom = (*atoms.offset(i as isize)).atom;
        if flags & JS_GPN_ENUM_ONLY != 0 {
            let mut desc: JSPropertyDescriptor = std::mem::zeroed();
            let res = JS_GetOwnPropertyInternal(ctx, &mut desc, p, atom);
            if res < 0 {
                exception!();
            }
            if res == 0 {
                continue;
            }
            js_free_desc(ctx, &mut desc);
            if desc.flags & JS_PROP_ENUMERABLE == 0 {
                continue;
            }
        }

        let val: JSValue;

        match kind {
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                val = atom.to_value(ctx.as_mut().unwrap(), false);
                if JS_IsException(val) {
                    exception!();
                }
            }
            JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                val = JS_GetProperty(ctx, obj, atom);
                if JS_IsException(val) {
                    exception!();
                }
            }
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                val = JS_NewArray(ctx);
                if JS_IsException(val) {
                    exception!();
                }
                let key: JSValue = atom.to_value(ctx.as_mut().unwrap(), false);
                if JS_IsException(key) {
                    JS_FreeValue(ctx, val);
                    exception!();
                }
                if JS_CreateDataPropertyUint32(ctx, val, 0, key, JS_PROP_THROW) < 0 {
                    JS_FreeValue(ctx, val);
                    exception!();
                }

                let value = JS_GetProperty(ctx, obj, atom);
                if JS_IsException(value) {
                    JS_FreeValue(ctx, val);
                    exception!();
                }
                if JS_CreateDataPropertyUint32(ctx, val, 1, value, JS_PROP_THROW) < 0 {
                    JS_FreeValue(ctx, val);
                    exception!();
                }
            }
        }

        if JS_CreateDataPropertyUint32(ctx, ret_val, j as _, val, 0) < 0 {
            exception!();
        }

        j += 1;
    }

    // js_free_prop_enum(ctx, atoms, len as _);
    for item in
        Box::from_raw(std::slice::from_raw_parts_mut(atoms, len as usize) as *mut [PropertyEnum])
    {
        JS_FreeAtom(ctx, item.atom);
    }

    JS_FreeValue(ctx, obj);

    ret_val
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

pub unsafe fn js_free_desc(ctx: *mut JSContext, desc: *mut JSPropertyDescriptor) {
    JS_FreeValue(ctx, (*desc).getter);
    JS_FreeValue(ctx, (*desc).setter);
    JS_FreeValue(ctx, (*desc).value);
}
