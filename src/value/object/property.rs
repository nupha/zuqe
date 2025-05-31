use std::{ffi::c_void, mem::ManuallyDrop, pin::Pin, ptr::NonNull};

use crate::{
    atom::{JSAtom, JS_AtomIsNumericIndex, __JS_AtomToUInt32, consts::*},
    context::JSContext,
    error::JS_ThrowReferenceErrorUninitialized,
    function::JSVarRef,
    gc::{GCObject, RefCounted},
    proxy::js_proxy_isExtensible,
    runtime::JSRuntime,
    utils::{unlikely, Node},
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_PROXY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{
        JSTag, JSValue, JSValueConst, JS_FreeValue, JS_FreeValueRT, JS_IsException,
        JS_IsUninitialized, JS_ToLengthFree, JS_ToUint32Free, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
    },
    js_autoinit_free, js_autoinit_func_table, js_autoinit_get_id, js_autoinit_get_realm,
    js_free_desc, JSObject, JSShapeProperty,
};

// Property flags
/// Indicates if a property can be modified or deleted
pub const JS_PROP_CONFIGURABLE: u32 = 1 << 0;
/// Indicates if a property's value can be changed
pub const JS_PROP_WRITABLE: u32 = 1 << 1;
/// Indicates if a property appears in enumeration of object's properties
pub const JS_PROP_ENUMERABLE: u32 = 1 << 2;
/// Combined flag for configurable, writable and enumerable properties
pub const JS_PROP_C_W_E: u32 = JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE | JS_PROP_ENUMERABLE;
/// Special flag for length property
pub const JS_PROP_LENGTH: u32 = 1 << 3;
/// Mask for property type
pub const JS_PROP_TMASK: u32 = 3 << 4;
/// Normal property type
pub const JS_PROP_NORMAL: u32 = 0 << 4;
/// Getter/setter property type
pub const JS_PROP_GETSET: u32 = 1 << 4;
/// Variable reference property type
pub const JS_PROP_VARREF: u32 = 2 << 4;
/// Auto-initialized property type
pub const JS_PROP_AUTOINIT: u32 = 3 << 4;

// Property definition flags
/// Base shift for property definition flags
pub const JS_PROP_HAS_SHIFT: u32 = 8;
/// Property has configurable attribute
pub const JS_PROP_HAS_CONFIGURABLE: u32 = 1 << 8;
/// Property has writable attribute
pub const JS_PROP_HAS_WRITABLE: u32 = 1 << 9;
/// Property has enumerable attribute
pub const JS_PROP_HAS_ENUMERABLE: u32 = 1 << 10;
/// Property has getter function
pub const JS_PROP_HAS_GET: u32 = 1 << 11;
/// Property has setter function
pub const JS_PROP_HAS_SET: u32 = 1 << 12;
/// Property has value
pub const JS_PROP_HAS_VALUE: u32 = 1 << 13;
/// Throw error on failed operation
pub const JS_PROP_THROW: u32 = 1 << 14;
/// Throw error in strict mode
pub const JS_PROP_THROW_STRICT: u32 = 1 << 15;
/// Prevent adding new properties
pub const JS_PROP_NO_ADD: u32 = 1 << 16;
/// Prevent exotic object behavior
pub const JS_PROP_NO_EXOTIC: u32 = 1 << 17;

// Property types
/// Regular identifier property
pub const PROP_TYPE_IDENT: i32 = 0;
/// Variable property
pub const PROP_TYPE_VAR: i32 = 1;
/// Getter property
pub const PROP_TYPE_GET: i32 = 2;
/// Setter property
pub const PROP_TYPE_SET: i32 = 3;
/// Generator property
pub const PROP_TYPE_STAR: i32 = 4;
/// Async property
pub const PROP_TYPE_ASYNC: i32 = 5;
/// Async generator property
pub const PROP_TYPE_ASYNC_STAR: i32 = 6;
/// Private property
pub const PROP_TYPE_PRIVATE: i32 = 1 << 4;

// Get property names flags
/// Include string properties
pub const JS_GPN_STRING_MASK: i32 = 1 << 0;
/// Include symbol properties
pub const JS_GPN_SYMBOL_MASK: i32 = 1 << 1;
/// Include private properties
pub const JS_GPN_PRIVATE_MASK: i32 = 1 << 2;
/// Only include enumerable properties
pub const JS_GPN_ENUM_ONLY: i32 = 1 << 4;
/// Set the is_enumerable field in JSPropertyEnum
pub const JS_GPN_SET_ENUM: i32 = 1 << 5;

pub struct JSPropertyDescriptor {
    pub flags: u32,
    pub value: JSValue,
    pub getter: JSValue,
    pub setter: JSValue,
}

pub struct JSProperty {
    pub u: JSPropertyUnion,
}

pub union JSPropertyUnion {
    /// JS_PROP_NORMAL
    pub value: JSValue,
    /// JS_PROP_GETSET
    pub getset: JSPropertyGetSet,
    pub var_ref: ManuallyDrop<NonNull<Pin<JSVarRef>>>,
    /// JS_PROP_AUTOINIT
    pub init: JSPropertyAutoInit,
}

#[derive(Clone, Copy)]
pub struct JSPropertyGetSet {
    pub getter: *mut JSObject, // NULL if undefined
    pub setter: *mut JSObject, // NULL if undefined
}

impl Default for JSPropertyGetSet {
    fn default() -> Self {
        Self {
            getter: std::ptr::null_mut(),
            setter: std::ptr::null_mut(),
        }
    }
}

#[derive(Clone, Copy)]
pub struct JSPropertyAutoInit {
    /// realm and init_id (JS_AUTOINIT_ID_x) in the 2 low bits
    pub realm_and_id: usize,
    // opaque pointer
    pub opaque: *mut c_void,
}

impl JSProperty {
    /// unset property value type.
    /// after calling this method, the property is in unknown type.
    pub(crate) unsafe fn unset_value(&mut self, ctx: &mut JSContext) {
        JS_FreeValue(ctx, self.u.value);
    }

    /// unset property getset type.
    /// after calling this method, the property is in unknown type.
    pub(crate) unsafe fn unset_getset(&mut self, ctx: &mut JSContext) {
        if !self.u.getset.getter.is_null() {
            JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, self.u.getset.getter.cast()));
        }
        if !self.u.getset.setter.is_null() {
            JS_FreeValue(ctx, JS_MKPTR!(JSTag::Object, self.u.getset.setter.cast()));
        }
    }

    /// unset property var_ref type.
    /// decreament var_ref, if ref count reaches zero, do var ref cleanup.
    /// after calling this method, the property is in unknown type.
    /// must ensure the property is var_ref union type.
    pub(crate) unsafe fn unset_var_ref(&mut self, ctx: &mut JSContext) {
        if (*self.u.var_ref).cast::<JSVarRef>().as_mut().dec_ref() == 0 {
            // when ref_count reaches 0, var_ref should be dropped:
            // perform clean up and remove from gc list only,
            // don't really drop because it's in a union.
            (*self.u.var_ref)
                .cast::<JSVarRef>()
                .as_mut()
                .finalize(ctx.rt_mut());

            Node::unlink((*self.u.var_ref).cast::<JSVarRef>().as_mut().gc_list_node());
        }
    }

    pub fn dispose(self, rt: &mut JSRuntime, prop_flags: u32) {
        if prop_flags & JS_PROP_TMASK != 0 {
            match prop_flags & JS_PROP_TMASK {
                JS_PROP_GETSET => unsafe {
                    if !self.u.getset.getter.is_null() {
                        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, self.u.getset.getter.cast()));
                    }
                    if !self.u.getset.setter.is_null() {
                        JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, self.u.getset.setter.cast()));
                    }
                },
                JS_PROP_VARREF => {
                    let vr_ptr = ManuallyDrop::into_inner(unsafe { self.u.var_ref });
                    unsafe {
                        JSVarRef::free(rt, vr_ptr.cast());
                    }
                }
                JS_PROP_AUTOINIT => unsafe {
                    js_autoinit_free(rt, &self);
                },
                _ => {}
            }
        } else {
            unsafe {
                JS_FreeValueRT(rt, self.u.value);
            }
        }
    }
}

#[deprecated(note = "use JSValue::has_property() instead")]
pub unsafe fn JS_HasProperty(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
) -> Result<bool, i32> {
    obj.has_property(ctx.as_mut().unwrap(), prop)
}

impl JSObject {
    /// Grows the property values array to accommodate more properties.
    /// Takes a new length and extends the array with undefined values if needed.
    /// Returns error if memory allocation fails.
    pub(super) fn grow_prop_value_slots(&mut self, new_length: usize) -> Result<(), i32> {
        if new_length > self.prop_values.len() {
            let delta = new_length - self.prop_values.len();
            if self.prop_values.try_reserve_exact(delta).is_err() {
                return Err(-1);
            }

            // init extended elements to undefined
            for _ in 0..delta {
                self.prop_values.push(JSProperty {
                    u: JSPropertyUnion {
                        value: JSValue::UNDEFINED,
                    },
                });
            }
        }

        Ok(())
    }

    /// Updates the flags of a property in the object's shape.
    /// Takes a context pointer, property shape pointer pointer, and new flags.
    /// Returns 0 on success, -1 on failure.
    pub unsafe fn js_update_property_flags(
        &mut self,
        ctx: *mut JSContext,
        pprs: *mut *mut JSShapeProperty,
        flags: u32,
    ) -> i32 {
        if flags != (*(*pprs)).flags() {
            if self.shape_prepare_update(ctx, pprs) != 0 {
                return -1;
            }
            (*(*pprs)).set_flags(flags);
        }

        0
    }

    /// Finds shape property of the object by its atom name.
    /// Returns immutable reference to property if found, None if not found.
    #[inline(always)]
    pub fn find_own_property1(&self, atom: JSAtom) -> Option<&JSShapeProperty> {
        self.shape().and_then(|sh| sh.props.get(&atom))
    }

    /// Finds shape property of the object by its atom name.
    /// Returns mutable reference to property if found, None if not found.
    #[inline(always)]
    pub fn find_own_property1_mut(&mut self, atom: JSAtom) -> Option<&mut JSShapeProperty> {
        self.shape_mut().and_then(|sh| sh.props.get_mut(&atom))
    }

    /// Finds both shape property and property of an object by atom name.
    #[inline(always)]
    pub fn find_own_property(&self, atom: JSAtom) -> Option<(&JSShapeProperty, &JSProperty)> {
        self.shape()
            .and_then(|sh| sh.props.get_full(&atom))
            .map(|(i, _, prs)| {
                let pr = self.prop_values.get(i).unwrap();
                (prs, pr)
            })
    }

    /// Finds both shape property and property of an object by atom name.
    #[inline(always)]
    pub fn find_own_property_mut(
        &mut self,
        atom: JSAtom,
    ) -> Option<(&mut JSShapeProperty, &mut JSProperty)> {
        unsafe { self.shape.as_mut() }
            .and_then(|sh| sh.props.get_full_mut(&atom))
            .map(|(i, _, prs)| {
                let pr = self.prop_values.get_mut(i).unwrap();
                (prs, pr)
            })
    }
}

impl JSValue {
    pub fn has_property(self, ctx: &mut JSContext, prop: JSAtom) -> Result<bool, i32> {
        if JS_VALUE_GET_TAG(self) != JSTag::Object as i32 {
            return Ok(false);
        }

        let mut p = unsafe { self.object_unchecked() };

        loop {
            if p.detailed.is_exotic() {
                if let Some(f) = unsafe {
                    ctx.rt().class_array[p.class_id() as usize]
                        .exotic
                        .as_ref()
                        .and_then(|x| x.has_property)
                } {
                    // has_property can free the prototype, dup it
                    let obj1 = JS_MKPTR!(JSTag::Object, (p as *mut JSObject).cast()).dup();

                    unsafe {
                        let ret = f(ctx, obj1, prop);
                        JS_FreeValue(ctx, obj1);

                        return match ret {
                            -1 => Err(-1),
                            n => Ok(n != 0),
                        };
                    }
                }
            }

            unsafe {
                // JS_GetOwnPropertyInternal can free the prototype
                let o = JS_MKPTR!(JSTag::Object, (p as *mut JSObject).cast());
                o.dup();
                let ret = JS_GetOwnPropertyInternal(ctx, std::ptr::null_mut(), p, prop);
                JS_FreeValue(ctx, o);

                match ret {
                    -1 => {
                        return Err(-1);
                    }
                    0 => {
                        if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY).contains(&p.class_id())
                        {
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

                        // walk up proto
                        if let Some(up) = p.shape().unwrap().proto.as_mut() {
                            p = up;
                        } else {
                            break;
                        }
                    }
                    _ => {
                        return Ok(true);
                    }
                }
            }
        }

        Ok(false)
    }

    /// copy and assign enumerable properties from `source` value to `self`.
    /// equivelent to js `Object::assign(self, source)`
    pub unsafe fn copy_properties_from(
        self,
        ctx: &mut JSContext,
        source: Self,
        excluded: Self,
        setprop: bool,
    ) -> Result<(), i32> {
        if JS_VALUE_GET_TAG(source) != JSTag::Object as i32 {
            return Ok(());
        }
        let pexcl = if JS_VALUE_GET_TAG(excluded) == JSTag::Object as i32 {
            JS_VALUE_GET_OBJ(excluded)
        } else {
            std::ptr::null_mut()
        };

        let src_obj = JS_VALUE_GET_OBJ(source);
        let mut gpn_flags = JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK | JS_GPN_ENUM_ONLY;

        if (*src_obj).detailed.is_exotic() {
            if ctx.rt().class_array[(*src_obj).class_id() as usize]
                .exotic
                .as_ref()
                .and_then(|x| x.get_own_property_names)
                .is_some()
            {
                // cannot use JS_GPN_ENUM_ONLY with e.g. proxies because it
                // introduces a visible change
                gpn_flags &= !JS_GPN_ENUM_ONLY;
            }
        }

        (*src_obj).own_property_names_foreach(ctx, gpn_flags, |cx, name| {
            let mut desc: JSPropertyDescriptor = std::mem::zeroed();

            if !pexcl.is_null() {
                let r = JS_GetOwnPropertyInternal(cx, std::ptr::null_mut(), pexcl, name.atom);
                if r < 0 {
                    return Err(-1);
                } else if r != 0 {
                    return Ok(()); // is excluded prop, skip
                }
            }

            if gpn_flags & JS_GPN_ENUM_ONLY == 0 {
                // test if the property is enumerable
                let ret = JS_GetOwnPropertyInternal(cx, &mut desc, src_obj, name.atom);
                match ret.cmp(&0) {
                    std::cmp::Ordering::Less => {
                        return Err(-1);
                    }
                    std::cmp::Ordering::Equal => {
                        return Ok(()); // prop not exist, skip
                    }
                    std::cmp::Ordering::Greater => {}
                }

                let is_enumerable = desc.flags & JS_PROP_ENUMERABLE != 0;
                js_free_desc(cx, &mut desc);
                if !is_enumerable {
                    return Ok(()); // skip non enumerable
                }
            }

            let val = source.get_property(cx, name.atom);
            if val.is_exception() {
                return Err(-1);
            }
            if setprop {
                self.set_property(cx, name.atom, val)?;
            } else {
                self.define_property_value(cx, name.atom, val, JS_PROP_C_W_E)?;
            }

            Ok(())
        })
    }
}

#[deprecated(note = "use JSProperty::dispose() instead")]
pub unsafe fn free_property(rt: *mut JSRuntime, pr: *mut JSProperty, prop_flags: u32) {
    unreachable!("use JSProperty::dispose() instead");

    // if unlikely(prop_flags & JS_PROP_TMASK != 0) {
    //     match prop_flags & JS_PROP_TMASK {
    //         JS_PROP_GETSET => {
    //             if !(*pr).u.getset.getter.is_null() {
    //                 JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*pr).u.getset.getter.cast()));
    //             }
    //             if !(*pr).u.getset.setter.is_null() {
    //                 JS_FreeValueRT(rt, JS_MKPTR!(JSTag::Object, (*pr).u.getset.setter.cast()));
    //             }
    //         }
    //         JS_PROP_VARREF => {
    //             if let Some(vr) = Box::from_raw((*pr).u.var_ref).free(rt) {
    //                 std::mem::forget(vr);
    //             }
    //         }
    //         JS_PROP_AUTOINIT => {
    //             js_autoinit_free(rt, pr);
    //         }
    //         _ => {}
    //     }
    // } else {
    //     JS_FreeValueRT(rt, (*pr).u.value);
    // }
}

/// Computes the effective property flags by combining input flags with default flags
/// If a flag is specified in JS_PROP_HAS_x, it takes precedence over def_flags
#[inline(always)]
pub const fn get_prop_flags(flags: u32, def_flags: u32) -> u32 {
    let mask = (flags >> JS_PROP_HAS_SHIFT) & JS_PROP_C_W_E;
    (flags & mask) | (def_flags & !mask)
}

/// Finds a property in an object by its atom name
#[deprecated(note = "use JSObject::find_own_property1() instead")]
pub unsafe fn find_own_property1(p: *mut JSObject, atom: JSAtom) -> *mut JSShapeProperty {
    (*p).find_own_property1_mut(atom)
        .map_or(std::ptr::null_mut(), |p| p as *mut JSShapeProperty)
}

/// Finds a property and its value in an object by atom name
/// Sets ppr to the property value pointer if found, null otherwise
/// Returns the property shape pointer if found, null otherwise
#[inline(always)]
#[deprecated(note = "use JSObject::find_own_property() instead")]
pub unsafe fn find_own_property(
    ppr: *mut *mut JSProperty,
    p: *mut JSObject,
    atom: JSAtom,
) -> *mut JSShapeProperty {
    (*p).find_own_property_mut(atom)
        .map(|(prs, pr)| {
            *ppr = pr;
            prs as *mut JSShapeProperty
        })
        .unwrap_or(std::ptr::null_mut())
}

/// Checks if property flags are valid for property definition
/// Returns false if the flags are invalid
pub const unsafe fn check_define_prop_flags(prop_flags: u32, flags: u32) -> bool {
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
    if (*p).shape_prepare_update(ctx, &mut prs) != 0 {
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
    loop {
        if let Some((prs, pr)) = (*p).find_own_property_mut(prop) {
            if !desc.is_null() {
                (*desc).flags = prs.flags() & JS_PROP_C_W_E;
                (*desc).getter = JSValue::UNDEFINED;
                (*desc).setter = JSValue::UNDEFINED;
                (*desc).value = JSValue::UNDEFINED;

                if prs.flags() & JS_PROP_TMASK != 0 {
                    match prs.flags() & JS_PROP_TMASK {
                        JS_PROP_GETSET => {
                            (*desc).flags |= JS_PROP_GETSET;
                            if !pr.u.getset.getter.is_null() {
                                (*desc).getter =
                                    JS_MKPTR!(JSTag::Object, pr.u.getset.getter.cast()).dup();
                            }
                            if !pr.u.getset.setter.is_null() {
                                (*desc).setter =
                                    JS_MKPTR!(JSTag::Object, pr.u.getset.setter.cast()).dup();
                            }
                        }
                        JS_PROP_VARREF => {
                            let val = *(*pr.u.var_ref).cast::<JSVarRef>().as_ref().pvalue;
                            if unlikely(JS_IsUninitialized(val)) {
                                JS_ThrowReferenceErrorUninitialized(ctx, prs.atom);
                                return -1;
                            }
                            (*desc).value = val.dup();
                        }
                        JS_PROP_AUTOINIT => {
                            // Instantiate property and retry
                            if JS_AutoInitProperty(ctx, p, prop, pr, prs) != 0 {
                                return -1;
                            }
                            continue; // retry
                        }
                        _ => {}
                    }
                } else {
                    (*desc).value = pr.u.value.dup();
                }
            } else {
                /* for consistency, send the exception even if desc is NULL */
                if prs.flags() & JS_PROP_TMASK == JS_PROP_VARREF {
                    if JS_IsUninitialized(*(pr.u.var_ref).cast::<JSVarRef>().as_ref().pvalue) {
                        JS_ThrowReferenceErrorUninitialized(ctx, prs.atom);
                        return -1;
                    }
                } else if prs.flags() & JS_PROP_TMASK == JS_PROP_AUTOINIT {
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
        (*ctx).JS_ThrowTypeErrorNotAnObject();
        -1
    } else {
        JS_GetOwnPropertyInternal(ctx, desc, JS_VALUE_GET_OBJ(obj), prop)
    }
}

#[deprecated(note = "use JSValue::copy_properties_from() instead")]
pub unsafe fn JS_CopyDataProperties(
    ctx: *mut JSContext,
    target: JSValueConst,
    source: JSValueConst,
    excluded: JSValueConst,
    setprop: bool,
) -> Result<(), i32> {
    target.copy_properties_from(ctx.as_mut().unwrap(), source, excluded, setprop)
}

/// Get object `length` property as u32
pub unsafe fn js_get_length32(ctx: *mut JSContext, obj: JSValueConst) -> Result<u32, i32> {
    let len_val = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_length);
    if len_val.is_exception() {
        Err(-1)
    } else {
        JS_ToUint32Free(ctx, len_val)
    }
}

/// Get object `length` property as i64
pub unsafe fn js_get_length64(ctx: *mut JSContext, obj: JSValueConst) -> Result<i64, i32> {
    let len_val = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_length);
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
