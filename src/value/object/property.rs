use std::ffi::{c_void, CStr};

use crate::{
    array::{convert_fast_array_to_array, set_array_length, JS_ToArrayLengthFree},
    atom::{
        JSAtom, JS_AtomIsNumericIndex, JS_AtomIsNumericIndex1, JS_FreeAtom, JS_NewAtom,
        JS_ValueToAtom, __JS_AtomToUInt32, consts::*, JS_ATOM_NULL,
    },
    context::{JSContext, JS_DupContext},
    error::JS_ThrowReferenceErrorUninitialized,
    function::JSVarRef,
    proxy::js_proxy_isExtensible,
    runtime::{js_autoinit_func_table, JSRuntime},
    types::unlikely,
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_PROXY,
    JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        set_value, JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT,
        JS_IsException, JS_IsObject, JS_IsUninitialized, JS_NumberIsNegativeOrMinusZero,
        JS_ToBoolFree, JS_ToLengthFree, JS_ToStringInternal, JS_ToUint32Free, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
    },
    js_free_desc, js_free_prop_enum,
    obj::{
        js_autoinit_free, js_autoinit_get_id, js_autoinit_get_realm, JSAutoInitIDEnum, JSObject,
        JS_IsFunction,
    },
    JSShapeProperty, JS_DefinePropertyValue, JS_GetOwnPropertyNamesInternal, JS_GetProperty,
};

// Property flags
pub const JS_PROP_CONFIGURABLE: u32 = 1 << 0;
pub const JS_PROP_WRITABLE: u32 = 1 << 1;
pub const JS_PROP_ENUMERABLE: u32 = 1 << 2;
pub const JS_PROP_C_W_E: u32 = JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_ENUMERABLE;
pub const JS_PROP_LENGTH: u32 = 1 << 3;
pub const JS_PROP_TMASK: u32 = 3 << 4;
pub const JS_PROP_NORMAL: u32 = 0 << 4;
pub const JS_PROP_GETSET: u32 = 1 << 4;
pub const JS_PROP_VARREF: u32 = 2 << 4;
pub const JS_PROP_AUTOINIT: u32 = 3 << 4;

// Property definition flags
pub const JS_PROP_HAS_SHIFT: u32 = 8;
pub const JS_PROP_HAS_CONFIGURABLE: u32 = 1 << 8;
pub const JS_PROP_HAS_WRITABLE: u32 = 1 << 9;
pub const JS_PROP_HAS_ENUMERABLE: u32 = 1 << 10;
pub const JS_PROP_HAS_GET: u32 = 1 << 11;
pub const JS_PROP_HAS_SET: u32 = 1 << 12;
pub const JS_PROP_HAS_VALUE: u32 = 1 << 13;
pub const JS_PROP_THROW: u32 = 1 << 14;
pub const JS_PROP_THROW_STRICT: u32 = 1 << 15;
pub const JS_PROP_NO_ADD: u32 = 1 << 16;
pub const JS_PROP_NO_EXOTIC: u32 = 1 << 17;

// Property types
pub const PROP_TYPE_IDENT: i32 = 0;
pub const PROP_TYPE_VAR: i32 = 1;
pub const PROP_TYPE_GET: i32 = 2;
pub const PROP_TYPE_SET: i32 = 3;
pub const PROP_TYPE_STAR: i32 = 4;
pub const PROP_TYPE_ASYNC: i32 = 5;
pub const PROP_TYPE_ASYNC_STAR: i32 = 6;
pub const PROP_TYPE_PRIVATE: i32 = (1 << 4);

// Get property names flags
pub const JS_GPN_STRING_MASK: i32 = (1 << 0);
pub const JS_GPN_SYMBOL_MASK: i32 = (1 << 1);
pub const JS_GPN_PRIVATE_MASK: i32 = (1 << 2);
/* only include the enumerable properties */
pub const JS_GPN_ENUM_ONLY: i32 = (1 << 4);
/* set theJSPropertyEnum.is_enumerable field */
pub const JS_GPN_SET_ENUM: i32 = (1 << 5);

pub struct JSPropertyEnum {
    pub is_enumerable: bool,
    pub atom: JSAtom,
}

pub struct JSPropertyDescriptor {
    pub flags: u32,
    pub value: JSValue,
    pub getter: JSValue,
    pub setter: JSValue,
}

#[derive(Clone, Copy)]
pub struct JSProperty {
    pub u: JSPropertyUnion,
}

#[derive(Clone, Copy)]
pub union JSPropertyUnion {
    pub value: JSValue,           // JS_PROP_NORMAL
    pub getset: JSPropertyGetSet, // JS_PROP_GETSET
    pub var_ref: *mut JSVarRef,   // JS_PROP_VARREF, TODO: use NonNull<JSVarRef>
    pub init: JSPropertyAutoInit, // JS_PROP_AUTOINIT
}

#[derive(Clone, Copy)]
pub struct JSPropertyGetSet {
    pub getter: *mut JSObject, // NULL if undefined
    pub setter: *mut JSObject, // NULL if undefined
}

#[derive(Clone, Copy)]
pub struct JSPropertyAutoInit {
    /// realm and init_id (JS_AUTOINIT_ID_x) in the 2 low bits
    pub realm_and_id: usize,
    // opaque pointer
    pub opaque: *mut c_void,
}

pub unsafe fn JS_ToPropertyKey(ctx: *mut JSContext, val: JSValueConst) -> JSValue {
    JS_ToStringInternal(ctx, val, true)
}

pub unsafe fn JS_HasProperty(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> Result<bool, i32> {
    if JS_VALUE_GET_TAG(obj) != JSTag::Object as i32 {
        return Ok(false);
    }

    let mut p = JS_VALUE_GET_OBJ(obj);

    loop {
        if (*p).detailed.is_exotic() {
            let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
            if !em.is_null() && (*em).has_property.is_some() {
                // has_property can free the prototype
                let obj1 = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
                let ret = ((*em).has_property.unwrap())(ctx, obj1, prop);
                JS_FreeValue(ctx, obj1);

                return match ret {
                    -1 => Err(-1),
                    n => Ok(n != 0),
                };
            }
        }

        // JS_GetOwnPropertyInternal can free the prototype
        JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));
        let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, prop);
        JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, p.cast()));

        match ret {
            -1 => {
                return Err(-1);
            }
            0 => {
                if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&(*p).class_id()) {
                    match JS_AtomIsNumericIndex(ctx, prop) {
                        -1 => {
                            return Err(-1);
                        }
                        0 => {}
                        _ => {
                            return Ok(false);
                        }
                    }
                }

                p = (*p).shape().unwrap().proto;
                if p.is_null() {
                    break;
                }
            }
            _ => {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

/// Descriptor config object to JSPropertyDescriptor
pub unsafe fn js_obj_to_desc(
    ctx: *mut JSContext,
    d: *mut JSPropertyDescriptor,
    desc: JSValueConst,
) -> Result<(), i32> {
    if !JS_IsObject(desc) {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return Err(-1);
    }

    let mut val: JSValue = JSValue::UNDEFINED;
    let mut getter: JSValue = JSValue::UNDEFINED;
    let mut setter: JSValue = JSValue::UNDEFINED;
    let mut flags: u32 = 0;

    macro_rules! fail {
        () => {
            JS_FreeValue(ctx, val);
            JS_FreeValue(ctx, getter);
            JS_FreeValue(ctx, setter);
            return Err(-1);
        };
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_configurable).unwrap_or(true) {
        let prop = JS_GetProperty(ctx, desc, JS_ATOM_configurable);
        if JS_IsException(prop) {
            fail!();
        }
        flags |= JS_PROP_HAS_CONFIGURABLE;
        if JS_ToBoolFree(ctx, prop).unwrap_or(true) {
            flags |= JS_PROP_CONFIGURABLE;
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_writable).unwrap_or(true) {
        let prop = JS_GetProperty(ctx, desc, JS_ATOM_writable);
        if JS_IsException(prop) {
            fail!();
        }
        flags |= JS_PROP_HAS_WRITABLE;
        if JS_ToBoolFree(ctx, prop).unwrap_or(true) {
            flags |= JS_PROP_WRITABLE;
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_enumerable).unwrap_or(true) {
        let prop = JS_GetProperty(ctx, desc, JS_ATOM_enumerable);
        if JS_IsException(prop) {
            fail!();
        }
        flags |= JS_PROP_HAS_ENUMERABLE;
        if JS_ToBoolFree(ctx, prop).unwrap_or(true) {
            flags |= JS_PROP_ENUMERABLE;
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_value).unwrap_or(true) {
        flags |= JS_PROP_HAS_VALUE;
        val = JS_GetProperty(ctx, desc, JS_ATOM_value);
        if JS_IsException(val) {
            fail!();
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_get).unwrap_or(true) {
        flags |= JS_PROP_HAS_GET;
        getter = JS_GetProperty(ctx, desc, JS_ATOM_get);
        if getter.is_exception()
            || !(getter.is_undefined() || getter.is_function(ctx.as_ref().unwrap()))
        {
            (*ctx).JS_ThrowTypeError(c"invalid getter");
            fail!();
        }
    }

    if JS_HasProperty(ctx, desc, JS_ATOM_set).unwrap_or(true) {
        flags |= JS_PROP_HAS_SET;
        setter = JS_GetProperty(ctx, desc, JS_ATOM_set);
        if setter.is_exception()
            || !(setter.is_undefined() || setter.is_function(ctx.as_ref().unwrap()))
        {
            (*ctx).JS_ThrowTypeError(c"invalid setter");
            fail!();
        }
    }

    if (flags & (JS_PROP_HAS_SET | JS_PROP_HAS_GET)) != 0
        && (flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE)) != 0
    {
        (*ctx).JS_ThrowTypeError(c"cannot have setter/getter and value or writable");
        fail!();
    }

    (*d).flags = flags;
    (*d).value = val;
    (*d).getter = getter;
    (*d).setter = setter;

    Ok(())
}

impl JSObject {
    /// Grow prop values list to expected new length, extended elements will be initialized to value `undefined`.
    /// If new_length is less than or equal to current length, nothing to do.
    pub(super) unsafe fn grow_prop_value_slots(&mut self, new_length: usize) -> Result<(), i32> {
        if new_length > self.prop_values.len() {
            let grow = new_length - self.prop_values.len();
            if self.prop_values.try_reserve_exact(grow).is_err() {
                log::error!("enlarge object prop values slot failed");
                return Err(-1);
            }

            // init extended elements to `undefined` jsvalue
            self.prop_values.extend(
                std::iter::repeat(JSProperty {
                    u: JSPropertyUnion {
                        value: JSValue::UNDEFINED,
                    },
                })
                .take(grow),
            );
        }

        Ok(())
    }

    pub unsafe fn js_update_property_flags(
        &mut self,
        ctx: *mut JSContext,
        pprs: *mut *mut JSShapeProperty,
        flags: u32,
    ) -> i32 {
        if flags != (*(*pprs)).flags() {
            if self.js_shape_prepare_update(ctx, pprs) != 0 {
                return -1;
            }
            (*(*pprs)).set_flags(flags);
        }
        return 0;
    }
}

// /* Note: the property value is not initialized. Returns Err if memory error. */
// pub unsafe fn add_property(
//     ctx: &mut JSContext,
//     p: &mut JSObject,
//     prop: JSAtom,
//     prop_flags: u32,
// ) -> Result<*mut JSProperty, i32> {
//     let sh = p.shape.as_mut().unwrap();

//     if sh.is_hashed {
//         /* try to find an existing shape */
//         match find_hashed_shape_prop(ctx.rt.as_mut().unwrap(), sh, prop, prop_flags) {
//             Some(new_sh) => {
//                 /* matching shape found: use it */
//                 /*  the property array may need to be resized */
//                 if (*new_sh).prop_size != sh.prop_size {
//                     let new_prop = js_realloc(
//                         ctx,
//                         p.prop.cast(),
//                         std::mem::size_of::<JSProperty>() * (*new_sh).prop_size as usize,
//                     );
//                     if new_prop.is_null() {
//                         return Err(-1);
//                     }
//                     p.prop = new_prop.cast();
//                 }
//                 p.shape = js_dup_shape(new_sh);
//                 js_free_shape(ctx.rt, p.shape.as_mut().unwrap());

//                 return Ok(p.prop.offset((*new_sh).prop_count as isize - 1));
//             }
//             None => {
//                 /* if the shape is shared, clone it */
//                 let new_sh = sh.clone(ctx)?; //js_clone_shape(ctx, sh)?;
//                 let raw_sh = Box::into_raw(new_sh);
//                 /* hash the cloned shape */
//                 (*raw_sh).is_hashed = true;
//                 js_shape_hash_link(ctx.rt.as_mut().unwrap(), raw_sh);
//                 js_free_shape(ctx.rt, p.shape.as_mut().unwrap());
//                 p.shape = raw_sh;
//             }
//         };
//     }

//     debug_assert_eq!((*p.shape).header.ref_count, 1);

//     if add_shape_property(ctx, &mut p.shape, p, prop, prop_flags) != 0 {
//         Err(-1)
//     } else {
//         Ok(p.prop.add((*p.shape).prop_count as usize - 1))
//     }
// }

// pub unsafe fn delete_property(ctx: *mut JSContext, p: *mut JSObject, atom: JSAtom) -> i32 {
//     let mut sh = (*p).shape;
//     let mut prop = get_shape_prop(sh);

//     let h1 = (atom as isize) & (*sh).prop_hash_mask as isize;
//     let mut h = *prop_hash_end(sh).offset(-h1 - 1);
//     let mut lpr = std::ptr::null_mut::<JSShapeProperty>();

//     loop {
//         while h != 0 {
//             let mut pr = prop.offset(h as isize - 1);

//             if likely((*pr).atom == atom) {
//                 if (*pr).flags() & JS_PROP_CONFIGURABLE == 0 {
//                     return 0;
//                 }

//                 let lpr_idx = if !lpr.is_null() {
//                     lpr.offset_from(get_shape_prop(sh))
//                 } else {
//                     0
//                 };

//                 if js_shape_prepare_update(ctx, p, &mut pr) != 0 {
//                     return -1;
//                 }
//                 sh = (*p).shape;
//                 prop = get_shape_prop(sh);

//                 if !lpr.is_null() {
//                     lpr = prop.offset(lpr_idx);
//                     (*lpr).set_hash_next((*pr).hash_next());
//                 } else {
//                     *prop_hash_end(sh).offset(-h1 - 1) = (*pr).hash_next();
//                 }

//                 (*sh).deleted_prop_count += 1;

//                 let pr1 = (*p).prop.offset(h as isize - 1);
//                 free_property((*ctx).rt, pr1, (*pr).flags());
//                 JS_FreeAtom(ctx, (*pr).atom);
//                 (*pr).set_flags(0);
//                 (*pr).atom = JS_ATOM_NULL;
//                 (*pr1).u.value = JSValue::UNDEFINED;

//                 if (*sh).deleted_prop_count >= 8
//                     && (*sh).deleted_prop_count >= (*sh).prop_count as i32 / 2
//                 {
//                     compact_properties(ctx, p);
//                 }
//                 return 1;
//             }
//             lpr = pr;
//             h = (*pr).hash_next();
//         }

//         if (*p).detailed.is_exotic() {
//             if (*p).detailed.fast_array() {
//                 let mut idx: u32 = 0;
//                 if JS_AtomIsArrayIndex(ctx, &mut idx, atom) && idx < (*p).u.array.count {
//                     if (*p).class_id() == JS_CLASS_ARRAY
//                         || (*p).class_id() == JS_CLASS_ARGUMENTS
//                     {
//                         if idx == (*p).u.array.count - 1 {
//                             JS_FreeValue(ctx, *(*p).u.array.u.values.offset(idx as isize));
//                             (*p).u.array.count = idx;
//                             return 1;
//                         }
//                         if convert_fast_array_to_array(ctx, p) != 0 {
//                             return -1;
//                         }

//                         continue; // C2RS: goto redo simulated with continue
//                     } else {
//                         return 0;
//                     }
//                 }
//             } else {
//                 let class_id = (*p).class_id();
//                 let em = (*(*(*ctx).rt).class_array.offset(class_id as _)).exotic;
//                 if !em.is_null() {
//                     if let Some(cb) = (*em).delete_property {
//                         return cb(ctx, JS_MKPTR!(JS_TAG_OBJECT, p.cast()), atom);
//                     }
//                 }
//             }
//         }

//         return 1;
//     }
// }

// // remove the deleted properties.
// pub unsafe fn compact_properties(ctx: *mut JSContext, p: *mut JSObject) -> i32 {
//     let mut sh = (*p).shape;
//     debug_assert!(!(*sh).is_hashed);
//     debug_assert!(sh.is_aligned());

//     let new_size = std::cmp::max(
//         JS_PROP_INITIAL_SIZE,
//         (*sh).prop_count as u32 - (*sh).deleted_prop_count as u32,
//     );
//     assert!(new_size <= (*sh).prop_size as u32);

//     let mut new_hash_size = (*sh).prop_hash_mask + 1;
//     while (new_hash_size / 2) >= new_size {
//         new_hash_size = new_hash_size / 2;
//     }
//     let new_hash_mask = new_hash_size - 1;

//     // resize the hash table and the properties
//     let old_sh = sh;

//     // sh_alloc memory layou: prop_hash array + JSShape + JSProperty array
//     let sh_alloc = js_malloc(ctx, get_shape_size(new_hash_size as _, new_size as usize));
//     if sh_alloc.is_null() {
//         return -1;
//     }

//     sh = get_shape_from_alloc(sh_alloc.cast(), new_hash_size as _);
//     list_del(&mut (*old_sh).header.link);
//     std::ptr::copy_nonoverlapping(old_sh, sh, std::mem::size_of::<JSShape>());
//     list_add_tail(&mut (*sh).header.link, &mut (*(*ctx).rt).gc_obj_list);

//     // Initialize new hash table
//     let hash_start = prop_hash_end(sh).offset(-(new_hash_size as isize));
//     std::ptr::write_bytes(
//         hash_start as *mut u8,
//         0,
//         std::mem::size_of::<u32>() * new_hash_size as usize,
//     );

//     let mut old_pr = (*old_sh).prop.as_mut_ptr();
//     let mut pr = (*sh).prop.as_mut_ptr();
//     let prop = (*p).prop;
//     let mut j = 0;

//     for i in 0..(*sh).prop_count {
//         if (*old_pr).atom != JS_ATOM_NULL {
//             (*pr).atom = (*old_pr).atom;
//             (*pr).set_flags((*old_pr).flags());
//             let h = ((*old_pr).atom as usize) & (new_hash_mask as usize);
//             let hash_entry = prop_hash_end(sh).offset(-(h as isize + 1));
//             (*pr).set_hash_next(*hash_entry);
//             *hash_entry = j + 1;
//             *prop.offset(j as isize) = *prop.offset(i as isize);
//             j += 1;
//             pr = pr.offset(1);
//         }
//         old_pr = old_pr.offset(1);
//     }
//     assert_eq!(
//         j,
//         ((*sh).prop_count as u32 - (*sh).deleted_prop_count as u32)
//     );

//     (*sh).prop_hash_mask = new_hash_mask;
//     (*sh).prop_size = new_size as i32;
//     (*sh).deleted_prop_count = 0;
//     (*sh).prop_count = j as i32;

//     (*p).shape = sh;
//     js_free(ctx, get_alloc_from_shape(old_sh).cast());

//     // reduce the size of the object properties
//     let new_prop = js_realloc(
//         ctx,
//         (*p).prop as *mut _,
//         std::mem::size_of::<JSProperty>() * new_size as usize,
//     );
//     if !new_prop.is_null() {
//         (*p).prop = new_prop.cast();
//     }

//     0
// }

pub unsafe fn free_property(rt: *mut JSRuntime, pr: *mut JSProperty, prop_flags: u32) {
    if unlikely(prop_flags & JS_PROP_TMASK != 0) {
        match prop_flags & JS_PROP_TMASK {
            JS_PROP_GETSET => {
                if !(*pr).u.getset.getter.is_null() {
                    JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()));
                }
                if !(*pr).u.getset.setter.is_null() {
                    JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()));
                }
            }
            JS_PROP_VARREF => {
                if let Some(vr) = Box::from_raw((*pr).u.var_ref).free(rt) {
                    std::mem::forget(vr);
                }
            }
            JS_PROP_AUTOINIT => {
                js_autoinit_free(rt, pr);
            }
            _ => {}
        }
    } else {
        JS_FreeValueRT(rt, (*pr).u.value);
    }
}

#[inline(always)]
pub unsafe fn find_own_property1(p: *mut JSObject, atom: JSAtom) -> *mut JSShapeProperty {
    (*p).shape_mut()
        .and_then(|sh| sh.props.get_mut(&atom))
        .map_or(std::ptr::null_mut(), |p| p as *mut JSShapeProperty)
}

#[inline(always)]
pub unsafe fn find_own_property(
    ppr: *mut *mut JSProperty,
    p: *mut JSObject,
    atom: JSAtom,
) -> *mut JSShapeProperty {
    if let Some((idx, _, prs)) = (*p).shape_mut().unwrap().props.get_full_mut(&atom) {
        *ppr = (*p).prop_values.get_mut(idx).unwrap();
        prs
    } else {
        *ppr = std::ptr::null_mut();
        std::ptr::null_mut()
    }
}

/* compute the property flags. For each flag: (JS_PROP_HAS_x forces
   it, otherwise def_flags is used)
   Note: makes assumption about the bit pattern of the flags
*/
#[inline(always)]
pub fn get_prop_flags(flags: u32, def_flags: u32) -> u32 {
    let mask = (flags >> JS_PROP_HAS_SHIFT) & JS_PROP_C_W_E;
    (flags & mask) | (def_flags & !mask)
}

// return FALSE if not OK
pub unsafe fn check_define_prop_flags(prop_flags: u32, flags: u32) -> bool {
    let has_accessor;
    let is_getset;

    if (prop_flags & JS_PROP_CONFIGURABLE) == 0 {
        if (flags & (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE))
            == (JS_PROP_HAS_CONFIGURABLE | JS_PROP_CONFIGURABLE)
        {
            return false;
        }
        if (flags & JS_PROP_HAS_ENUMERABLE) != 0
            && (flags & JS_PROP_ENUMERABLE) != (prop_flags & JS_PROP_ENUMERABLE)
        {
            return false;
        }
    }
    if (flags & (JS_PROP_HAS_VALUE | JS_PROP_HAS_WRITABLE | JS_PROP_HAS_GET | JS_PROP_HAS_SET)) != 0
    {
        if (prop_flags & JS_PROP_CONFIGURABLE) == 0 {
            has_accessor = (flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET)) != 0;
            is_getset = (prop_flags & JS_PROP_TMASK) == JS_PROP_GETSET;
            if has_accessor != is_getset {
                return false;
            }
            if !has_accessor && !is_getset && (prop_flags & JS_PROP_WRITABLE) == 0 {
                /* not writable: cannot set the writable bit */
                if (flags & (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE))
                    == (JS_PROP_HAS_WRITABLE | JS_PROP_WRITABLE)
                {
                    return false;
                }
            }
        }
    }
    true
}

/* warning: 'prs' is reallocated after it */
pub unsafe fn JS_AutoInitProperty(
    ctx: *mut JSContext,
    p: *mut JSObject,
    prop: JSAtom,
    pr: *mut JSProperty,
    mut prs: *mut JSShapeProperty,
) -> i32 {
    if p.as_mut().unwrap().js_shape_prepare_update(ctx, &mut prs) != 0 {
        return -1;
    }

    let realm = js_autoinit_get_realm(pr);
    let func = js_autoinit_func_table[js_autoinit_get_id(pr) as usize];

    /* 'func' shall not modify the object properties 'pr' */
    let val = func(realm, p, prop, (*pr).u.init.opaque);

    js_autoinit_free((*ctx).rt_mut(), pr);

    // clear AUTO INIT flag
    let f = (*prs).flags() & (!JS_PROP_TMASK);
    (*prs).set_flags(f);

    if JS_IsException(val) {
        (*pr).u.value = JSValue::UNDEFINED;
        -1
    } else {
        (*pr).u.value = val;
        0
    }
}

pub unsafe fn JS_DefineAutoInitProperty(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    id: JSAutoInitIDEnum,
    opaque: *mut std::ffi::c_void,
    flags: u32,
) -> Result<bool, i32> {
    if JS_VALUE_GET_TAG(this_obj) != JSTag::Object as i32 {
        return Ok(false);
    }

    let p = JS_VALUE_GET_OBJ(this_obj).as_mut().unwrap();

    let mut pr = std::ptr::null_mut::<JSProperty>();
    if !find_own_property(std::ptr::addr_of_mut!(pr), p, prop).is_null() {
        // property already exists
        unreachable!();
    }

    // Specialized CreateProperty
    let pr = p
        .add_property(
            ctx.as_mut().unwrap(),
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_AUTOINIT,
        )?
        .as_mut()
        .unwrap();

    pr.u.init.realm_and_id = JS_DupContext(ctx) as usize;
    debug_assert!(((*pr).u.init.realm_and_id & 3) == 0);

    pr.u.init.realm_and_id |= id as usize; // id: 0,1,2
    debug_assert!(matches!((*pr).u.init.realm_and_id & 3, 0 | 1 | 2));

    pr.u.init.opaque = opaque;

    Ok(true)
}

/* return Err() if exception (Proxy object only) or TRUE/FALSE */
pub unsafe fn JS_IsExtensible(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    if unlikely(JS_VALUE_GET_TAG(obj) != JSTag::Object as i32) {
        return Ok(false);
    }

    let p = JS_VALUE_GET_OBJ(obj);
    if unlikely((*p).class_id() == JS_CLASS_PROXY) {
        js_proxy_isExtensible(ctx, obj)
    } else {
        Ok((*p).detailed.extensible())
    }
}

/* Return -1 if exception,
FALSE if the property does not exist, TRUE if it exists. If TRUE is
returned, the property descriptor 'desc' is filled present. */
pub unsafe fn JS_GetOwnPropertyInternal(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    p: *mut JSObject,
    prop: JSAtom,
) -> i32 {
    let mut pr: *mut JSProperty = std::ptr::null_mut();
    let mut prs: *mut JSShapeProperty;

    loop {
        prs = find_own_property(&mut pr as *mut *mut JSProperty, p, prop);
        if !prs.is_null() {
            if !desc.is_null() {
                (*desc).flags = (*prs).flags() & JS_PROP_C_W_E;
                (*desc).getter = JSValue::UNDEFINED;
                (*desc).setter = JSValue::UNDEFINED;
                (*desc).value = JSValue::UNDEFINED;
                if unlikely((*prs).flags() & JS_PROP_TMASK != 0) {
                    match (*prs).flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => {
                            (*desc).flags |= JS_PROP_GETSET;
                            if !(*pr).u.getset.getter.is_null() {
                                (*desc).getter =
                                    JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()).dup();
                            }
                            if !(*pr).u.getset.setter.is_null() {
                                (*desc).setter =
                                    JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()).dup();
                            }
                        }
                        JS_PROP_VARREF => {
                            let val = *(*(*pr).u.var_ref).pvalue;
                            if unlikely(JS_IsUninitialized(val)) {
                                JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
                                return -1;
                            }
                            (*desc).value = val.dup();
                        }
                        JS_PROP_AUTOINIT => {
                            /* Instantiate property and retry */
                            if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                                return -1;
                            }
                            continue; // C2RS: Replaced goto retry with loop+continue
                        }
                        _ => {}
                    }
                } else {
                    (*desc).value = (*pr).u.value.dup();
                }
            } else {
                /* for consistency, send the exception even if desc is NULL */
                if unlikely((*prs).flags() & JS_PROP_TMASK == JS_PROP_VARREF) {
                    if unlikely(JS_IsUninitialized(*(*(*pr).u.var_ref).pvalue)) {
                        JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
                        return -1;
                    }
                } else if ((*prs).flags() & JS_PROP_TMASK) == JS_PROP_AUTOINIT {
                    /* nothing to do: delay instantiation until actual value and/or attributes are read */
                }
            }
            return 1;
        }

        if (*p).detailed.is_exotic() {
            if (*p).detailed.fast_array() {
                /* specific case for fast arrays */
                if prop.is_int_type() {
                    let idx = __JS_AtomToUInt32(prop);
                    if idx < (*p).array_length_unchecked() as u32 {
                        if !desc.is_null() {
                            (*desc).flags =
                                JS_PROP_WRITABLE | JS_PROP_ENUMERABLE | JS_PROP_CONFIGURABLE;
                            (*desc).getter = JSValue::UNDEFINED;
                            (*desc).setter = JSValue::UNDEFINED;
                            (*desc).value = JS_MKPTR!(JSTag::Object, p.cast())
                                .get_property_u32(ctx.as_mut().unwrap(), idx);
                        }
                        return 1;
                    }
                }
            } else {
                let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
                if !em.is_null() {
                    if let Some(cb) = (*em).get_own_property {
                        return cb(ctx, desc, JS_MKPTR!(JSTag::Object, p.cast()), prop);
                    }
                }
            }
        }

        break;
    }

    0
}

/// returned value has its ref_count incremented by 1
pub unsafe fn JS_GetOwnProperty(
    ctx: *mut JSContext,
    desc: *mut JSPropertyDescriptor,
    obj: JSValueConst,
    prop: JSAtom,
) -> i32 {
    if JS_VALUE_GET_TAG(obj) != JSTag::Object as i32 {
        ctx.as_mut().unwrap().JS_ThrowTypeErrorNotAnObject();
        return -1;
    }
    return JS_GetOwnPropertyInternal(ctx, desc, JS_VALUE_GET_OBJ(obj), prop);
}

pub unsafe fn JS_CopyDataProperties(
    ctx: *mut JSContext,
    target: JSValueConst,
    source: JSValueConst,
    excluded: JSValueConst,
    setprop: bool,
) -> Result<(), i32> {
    if JS_VALUE_GET_TAG(source) != JSTag::Object as i32 {
        return Ok(());
    }

    let mut tab_atom: *mut JSPropertyEnum = std::ptr::null_mut();
    let mut tab_atom_count: u32 = 0;

    let mut desc: JSPropertyDescriptor = std::mem::zeroed();

    let pexcl = if JS_VALUE_GET_TAG(excluded) == JSTag::Object as i32 {
        JS_VALUE_GET_OBJ(excluded)
    } else {
        std::ptr::null_mut()
    };

    let p = JS_VALUE_GET_OBJ(source);

    let mut gpn_flags = JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;
    if (*p).detailed.is_exotic() {
        let em = (*ctx).rt().class_array[(*p).class_id() as usize].exotic;
        // cannot use JS_GPN_ENUM_ONLY with e.g. proxies because it
        // introduces a visible change
        if !em.is_null() && (*em).get_own_property_names.is_some() {
            gpn_flags &= !JS_GPN_ENUM_ONLY;
        }
    }

    JS_GetOwnPropertyNamesInternal(ctx, &mut tab_atom, &mut tab_atom_count, p, gpn_flags)?;

    for i in 0..tab_atom_count {
        if !pexcl.is_null() {
            let r = JS_GetOwnPropertyInternal(
                ctx,
                std::ptr::null_mut(),
                pexcl,
                (*tab_atom.offset(i as isize)).atom,
            );
            if r != 0 {
                // ERR or TRUE
                if r < 0 {
                    // C2RS: goto exception replaced with early return logic
                    js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
                    return Err(-1);
                }
                continue;
            }
        }

        if gpn_flags & JS_GPN_ENUM_ONLY == 0 {
            // test if the property is enumerable
            let ret =
                JS_GetOwnPropertyInternal(ctx, &mut desc, p, (*tab_atom.offset(i as isize)).atom);
            if ret < 0 {
                // C2RS: goto exception replaced with early return logic
                js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
                return Err(-1);
            }
            if ret == 0 {
                continue;
            }

            let is_enumerable = (desc.flags & JS_PROP_ENUMERABLE) != 0;
            js_free_desc(ctx, &mut desc);
            if !is_enumerable {
                continue;
            }
        }

        let val = JS_GetProperty(ctx, source, (*tab_atom.offset(i as isize)).atom);
        if JS_IsException(val) {
            // C2RS: goto exception replaced with early return logic
            js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
            return Err(-1);
        }

        let ret = if setprop {
            target
                .set_property(
                    ctx.as_mut().unwrap(),
                    (*tab_atom.offset(i as isize)).atom,
                    val,
                )
                .map_or(-1, |b| b as i32)
        } else {
            JS_DefinePropertyValue(
                ctx,
                target,
                (*tab_atom.offset(i as isize)).atom,
                val,
                JS_PROP_C_W_E,
            )
        };
        if ret < 0 {
            js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
            return Err(-1);
        }
    }

    js_free_prop_enum(ctx, tab_atom, tab_atom_count as _);
    Ok(())
}

/// Get object `length` property as u32
pub unsafe fn js_get_length32(ctx: *mut JSContext, obj: JSValueConst) -> Result<u32, i32> {
    let len_val = JS_GetProperty(ctx, obj, JS_ATOM_length);
    if len_val.is_exception() {
        Err(-1)
    } else {
        JS_ToUint32Free(ctx, len_val)
    }
}

/// Get object `length` property as i64
pub unsafe fn js_get_length64(ctx: *mut JSContext, obj: JSValueConst) -> Result<i64, i32> {
    let len_val = JS_GetProperty(ctx, obj, JS_ATOM_length);
    if len_val.is_exception() {
        Err(-1)
    } else {
        JS_ToLengthFree(ctx, len_val)
    }
}

/* Note: the property value is not initialized. Return NULL if memory
error. */
#[deprecated(note = "use JSObject::add_property() instead")]
pub unsafe fn add_property(
    ctx: *mut JSContext,
    p: *mut JSObject,
    prop: JSAtom,
    prop_flags: u32,
) -> *mut JSProperty {
    (*p).add_property(ctx.as_mut().unwrap(), prop, prop_flags)
        .unwrap_or(std::ptr::null_mut())
}
