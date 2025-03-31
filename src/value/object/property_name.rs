use crate::{
    array::JS_NewArray,
    atom::{JSAtom, JSAtomKindEnum, JS_AtomIsArrayIndex, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    context::JSContext,
    error::JS_ThrowReferenceErrorUninitialized,
    iterator::JSIteratorKindEnum,
    mem::{js_free, js_malloc},
    types::unlikely,
    value::{
        js_string_obj_get_length, JSTag, JSValue, JSValueConst, JS_FreeValue, JS_IsException,
        JS_IsUninitialized, JS_VALUE_GET_OBJ,
    },
    JS_CLASS_STRING, JS_MKPTR,
};

use super::{
    obj::{JSObject, JS_ToObject},
    property::{
        JSPropertyDescriptor, JSPropertyEnum, JS_CreateDataPropertyUint32,
        JS_GetOwnPropertyInternal, JS_GetProperty, JS_GPN_ENUM_ONLY, JS_GPN_SET_ENUM,
        JS_GPN_STRING_MASK, JS_PROP_ENUMERABLE, JS_PROP_THROW, JS_PROP_TMASK, JS_PROP_VARREF,
    },
};

// ptab and its atoms must be freed by the user.
pub unsafe fn JS_GetOwnPropertyNamesInternal(
    ctx: *mut JSContext,
    ptab: *mut *mut JSPropertyEnum,
    plen: *mut u32,
    p: *mut JSObject,
    flags: i32,
) -> Result<(), i32> {
    // clear pointer for consistency in case of failure
    *ptab = std::ptr::null_mut();
    *plen = 0;

    // compute the number of returned properties

    let mut num_keys_count: u32 = 0;
    let mut str_keys_count: u32 = 0;
    let mut sym_keys_count: u32 = 0;
    let mut num_key: u32 = 0;

    for (i, (key, prs)) in (*p).shape_mut().unwrap().props.iter_mut().enumerate() {
        let atom = *key;
        let is_enumerable = ((*prs).flags() & JS_PROP_ENUMERABLE) != 0;
        let kind = atom.kind((*ctx).rt());

        if ((flags & JS_GPN_ENUM_ONLY) == 0 || is_enumerable) && (flags >> (kind as u8)) & 1 != 0 {
            // need to raise an exception in case of the module
            // name space (implicit GetOwnProperty)
            if unlikely(((*prs).flags() & JS_PROP_TMASK) == JS_PROP_VARREF)
                && (flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY)) != 0
            {
                let var_ref = (*p).prop_values.get_mut(i as usize).unwrap().u.var_ref;
                if unlikely(JS_IsUninitialized(*(*var_ref).pvalue)) {
                    JS_ThrowReferenceErrorUninitialized(ctx, atom);
                    return Err(-1);
                }
            }

            if JS_AtomIsArrayIndex(ctx, &mut num_key, atom) {
                num_keys_count += 1;
            } else if kind == JSAtomKindEnum::JS_ATOM_KIND_STRING {
                str_keys_count += 1;
            } else {
                sym_keys_count += 1;
            }
        }
    }

    let mut exotic_keys_count: u32 = 0;
    let mut tab_exotic = std::ptr::null_mut::<JSPropertyEnum>();
    let mut exotic_count: u32 = 0;

    if (*p).detailed.is_exotic() {
        if (*p).detailed.fast_array() {
            if (flags & JS_GPN_STRING_MASK) != 0 {
                num_keys_count += (*p).u.array.count;
            }
        } else if (*p).class_id() == JS_CLASS_STRING {
            if (flags & JS_GPN_STRING_MASK) != 0 {
                num_keys_count +=
                    js_string_obj_get_length(ctx, JS_MKPTR!(JSTag::Object, p.cast())) as u32;
            }
        } else {
            let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
            if !em.is_null() && (*em).get_own_property_names.is_some() {
                if ((*em).get_own_property_names.unwrap())(
                    ctx,
                    &mut tab_exotic,
                    &mut exotic_count,
                    JS_MKPTR!(JSTag::Object, p.cast()),
                ) != 0
                {
                    return Err(-1);
                }

                for i in 0..exotic_count {
                    let atom = (*tab_exotic.offset(i as isize)).atom;
                    let kind = atom.kind((*ctx).rt());

                    if ((flags >> (kind as u8)) & 1) != 0 {
                        let mut is_enumerable = false;
                        if (flags & (JS_GPN_SET_ENUM | JS_GPN_ENUM_ONLY)) != 0 {
                            // set the "is_enumerable" field if necessary
                            let mut desc: JSPropertyDescriptor = std::mem::zeroed();
                            let res = JS_GetOwnPropertyInternal(ctx, &mut desc, p, atom);
                            if res < 0 {
                                js_free_prop_enum(ctx, tab_exotic, exotic_count as _);
                                return Err(-1);
                            }
                            if res != 0 {
                                is_enumerable = (desc.flags & JS_PROP_ENUMERABLE) != 0;
                                js_free_desc(ctx, &mut desc);
                            }
                            (*tab_exotic.offset(i as isize)).is_enumerable = is_enumerable;
                        }

                        if (!(flags & JS_GPN_ENUM_ONLY) != 0) || is_enumerable {
                            exotic_keys_count += 1;
                        }
                    }
                }
            }
        }
    }

    // fill them

    let atom_count = num_keys_count + str_keys_count + sym_keys_count + exotic_keys_count;
    // avoid allocating 0 bytes
    let tab_atom = js_malloc(
        ctx,
        std::mem::size_of::<JSPropertyEnum>() * std::cmp::max(atom_count, 1) as usize,
    ) as *mut JSPropertyEnum;
    if tab_atom.is_null() {
        js_free_prop_enum(ctx, tab_exotic, exotic_count as _);
        return Err(-1);
    }

    let mut num_index = 0;
    let mut str_index = num_keys_count;
    let mut sym_index = str_index + str_keys_count;
    let mut num_sorted = false;

    for (key, prs) in (*p).shape_mut().unwrap().props.iter_mut() {
        let atom = *key;
        let is_enumerable = ((*prs).flags() & JS_PROP_ENUMERABLE) != 0;
        let kind = atom.kind((*ctx).rt());

        let j: u32;

        if (!(flags & JS_GPN_ENUM_ONLY) != 0 || is_enumerable) && (flags >> (kind as u8)) & 1 != 0 {
            if JS_AtomIsArrayIndex(ctx, &mut num_key, atom) {
                j = num_index;
                num_index += 1;
                num_sorted = false;
            } else if kind == JSAtomKindEnum::JS_ATOM_KIND_STRING {
                j = str_index;
                str_index += 1;
            } else {
                j = sym_index;
                sym_index += 1;
            };

            (*tab_atom.offset(j as isize)).atom = JS_DupAtom(ctx, atom);
            (*tab_atom.offset(j as isize)).is_enumerable = is_enumerable;
        }
    }

    if (*p).detailed.is_exotic() {
        if (*p).detailed.fast_array() {
            if (flags & JS_GPN_STRING_MASK) != 0 {
                for i in 0..(*p).u.array.count {
                    (*tab_atom.offset(num_index as isize)).atom = JSAtom::from_u32(i);
                    if (*tab_atom.offset(num_index as isize)).atom == JS_ATOM_NULL {
                        js_free_prop_enum(ctx, tab_atom, num_index as _);
                        return Err(-1);
                    }
                    (*tab_atom.offset(num_index as isize)).is_enumerable = true;
                    num_index += 1;
                }
            }
        } else if (*p).class_id() == JS_CLASS_STRING {
            if (flags & JS_GPN_STRING_MASK) != 0 {
                let len = js_string_obj_get_length(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
                for i in 0..len {
                    (*tab_atom.offset(num_index as isize)).atom = JSAtom::from_u32(i as u32);
                    if (*tab_atom.offset(num_index as isize)).atom == JS_ATOM_NULL {
                        js_free_prop_enum(ctx, tab_atom, num_index as _);
                        return Err(-1);
                    }
                    (*tab_atom.offset(num_index as isize)).is_enumerable = true;
                    num_index += 1;
                }
            }
        } else {
            // Note: exotic keys are not reordered and comes after the object own properties.
            for i in 0..exotic_count {
                let atom = (*tab_exotic.offset(i as isize)).atom;
                let is_enumerable = (*tab_exotic.offset(i as isize)).is_enumerable;
                let kind = atom.kind((*ctx).rt());

                if (!(flags & JS_GPN_ENUM_ONLY) != 0 || is_enumerable)
                    && ((flags >> (kind as u8)) & 1) != 0
                {
                    (*tab_atom.offset(sym_index as isize)).atom = atom;
                    (*tab_atom.offset(sym_index as isize)).is_enumerable = is_enumerable;
                    sym_index += 1;
                } else {
                    JS_FreeAtom(ctx, atom);
                }
            }
            js_free(ctx, tab_exotic.cast());
        }
    }

    assert!(num_index == num_keys_count);
    assert!(str_index == num_keys_count + str_keys_count);
    assert!(sym_index == atom_count);

    if num_keys_count != 0 && !num_sorted {
        // sort by integral key
        let arr = std::slice::from_raw_parts_mut(tab_atom, num_keys_count as usize);
        arr.sort_by(|p1, p2| {
            let atom1 = (*p1).atom;
            let atom2 = (*p2).atom;

            let mut v1 = 0u32;
            let mut v2 = 0u32;

            let atom1_is_integer = JS_AtomIsArrayIndex(ctx, &mut v1, atom1);
            let atom2_is_integer = JS_AtomIsArrayIndex(ctx, &mut v2, atom2);
            debug_assert!(atom1_is_integer && atom2_is_integer);

            v1.cmp(&v2)
        });
    }
    *ptab = tab_atom;
    *plen = atom_count;

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
    let mut atoms: *mut JSPropertyEnum = std::ptr::null_mut();
    let mut len: u32 = 0;

    macro_rules! exception {
        () => {
            js_free_prop_enum(ctx, atoms, len as usize);
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

    js_free_prop_enum(ctx, atoms, len as _);
    JS_FreeValue(ctx, obj);

    ret_val
}

pub unsafe fn js_free_prop_enum(ctx: *mut JSContext, tab: *mut JSPropertyEnum, len: usize) {
    if !tab.is_null() {
        for i in 0..len {
            JS_FreeAtom(ctx, (*tab.add(i)).atom);
        }
        js_free(ctx, tab.cast());
    }
}

pub unsafe fn js_free_desc(ctx: *mut JSContext, desc: *mut JSPropertyDescriptor) {
    JS_FreeValue(ctx, (*desc).getter);
    JS_FreeValue(ctx, (*desc).setter);
    JS_FreeValue(ctx, (*desc).value);
}
