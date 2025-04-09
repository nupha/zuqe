use smallvec::SmallVec;

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
    /// Call closure on each property name of the object, name may occure in undefined order.
    /// Note: property name atom is not duplicated in calling `callback`,
    /// if you want to keep the atom referenced, dup it by yourself.
    /// When encounter error in callback or getting property names, returns Err and abort iteration immediately.
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

                // Note: exotic keys are not reordered and comes after the object own properties.
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

    /// Get own property names list.
    /// Returned list elements' atom is duplicated, and should be freed by caller.
    pub unsafe fn get_own_property_names_internal(
        &self,
        ctx: &mut JSContext,
        flags: i32,
    ) -> Result<Box<[PropertyEnum]>, i32> {
        let mut lst = Vec::<PropertyEnum>::with_capacity(16);
        let r = self.own_property_names_foreach(ctx, flags, |cx, pp| {
            if lst.len() == lst.capacity() {
                if lst.try_reserve_exact(16).is_err() {
                    cx.JS_ThrowOutOfMemory();
                    return Err(-1);
                }
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

        Ok(lst.into_boxed_slice())
    }

    /// Collect own property names of `self` object, return an array of keys, values, or key-values.
    /// returns Err when fail.
    /// Equivelent to js expression: Object.keys, Object.values, Object.entries
    pub fn get_own_property_names2(
        &self,
        ctx: &mut JSContext,
        flags: i32,
        kind: JSIteratorKindEnum,
    ) -> JSValue {
        // result array
        let ret_array = unsafe { JS_NewArray(ctx) };
        if ret_array.is_exception() {
            return JSValue::EXCEPTION;
        }

        let mut idx: u32 = 0;

        let r = unsafe {
            self.own_property_names_foreach(ctx, flags & !JS_GPN_ENUM_ONLY, |cx, name| {
                if flags & JS_GPN_ENUM_ONLY != 0 {
                    let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                    let res = JS_GetOwnPropertyInternal(
                        cx,
                        &mut desc,
                        self as *const JSObject as *mut JSObject,
                        name.atom,
                    );
                    if res < 0 {
                        return Err(-1);
                    }
                    if res == 0 {
                        return Ok(());
                    }
                    js_free_desc(cx, &mut desc);
                    if desc.flags & JS_PROP_ENUMERABLE == 0 {
                        return Ok(()); // skip non enumerable
                    }
                }

                let val: JSValue;
                match kind {
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY => {
                        val = name.atom.to_value(cx, false);
                        if val.is_exception() {
                            return Err(-1);
                        }
                    }
                    JSIteratorKindEnum::JS_ITERATOR_KIND_VALUE => {
                        val = JS_MKPTR!(
                            JSTag::Object,
                            (self as *const JSObject as *mut JSObject).cast()
                        )
                        .get_property(cx, name.atom);
                        if val.is_exception() {
                            return Err(-1);
                        }
                    }
                    JSIteratorKindEnum::JS_ITERATOR_KIND_KEY_AND_VALUE => {
                        val = JS_NewArray(cx);
                        if val.is_exception() {
                            return Err(-1);
                        }
                        let key = name.atom.to_value(cx, false);
                        if key.is_exception() {
                            JS_FreeValue(cx, val);
                            return Err(-1);
                        }
                        if val
                            .create_data_property_u32(cx, 0, key, JS_PROP_THROW)
                            .is_err()
                        {
                            JS_FreeValue(cx, val);
                            return Err(-1);
                        }

                        let value = JS_MKPTR!(
                            JSTag::Object,
                            (self as *const JSObject as *mut JSObject).cast()
                        )
                        .get_property(cx, name.atom);
                        if value.is_exception() {
                            JS_FreeValue(cx, val);
                            return Err(-1);
                        }
                        if val
                            .create_data_property_u32(cx, 1, value, JS_PROP_THROW)
                            .is_err()
                        {
                            JS_FreeValue(cx, val);
                            return Err(-1);
                        }
                    }
                }

                // append to result array
                ret_array.create_data_property_u32(cx, idx as _, val, 0)?;

                idx += 1;
                Ok(())
            })
        };

        if r.is_err() {
            unsafe {
                JS_FreeValue(ctx, ret_array);
            }
            JSValue::EXCEPTION
        } else {
            ret_array
        }
    }
}

impl JSValue {
    /// Get own property names and return an array of keys, values, or key-values.
    /// returns Err when fail.
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
            unsafe { (*JS_VALUE_GET_OBJ(*obj)).get_own_property_names2(ctx, flags, kind) }
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
