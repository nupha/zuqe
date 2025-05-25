use std::ops::DerefMut;

use crate::{
    array::{add_fast_array_element, convert_fast_array_to_array},
    atom::{JSAtom, JS_AtomIsArrayIndex, JS_AtomIsNumericIndex, __JS_AtomToUInt32, consts::*},
    context::JSContext,
    value::object::{JSShape, JSShapeHandle},
    JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{set_value, JSTag, JSValue, JS_ToUint32, JS_VALUE_GET_OBJ},
    get_prop_flags, JSObject, JSProperty, JS_IsExtensible, JS_PROP_CONFIGURABLE, JS_PROP_C_W_E,
    JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_GET, JS_PROP_HAS_SET, JS_PROP_HAS_VALUE,
    JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE,
};

impl JSObject {
    /// Note: the property value is initialized to undefined. Returns Err if memory error.
    /// `prop` keeps ref count unchanged after calling.
    pub fn add_property(
        &mut self,
        ctx: &mut JSContext,
        prop: JSAtom,
        prop_flags: u32,
    ) -> Result<*mut JSProperty, i32> {
        let mut use_sh = JSShapeHandle::from(self.shape);

        if use_sh.is_hashed {
            // try to find an existing shape
            match ctx.rt().find_shape_with_prop(use_sh, prop, prop_flags) {
                Some(found) => {
                    // matching shape found: use it
                    let new_len = found.props.len();
                    if new_len != use_sh.props.len() {
                        debug_assert!(new_len > use_sh.props.len());
                        self.grow_prop_value_slots(new_len).inspect_err(|_| {
                            ctx.JS_ThrowOutOfMemory();
                        })?;
                    }

                    self.shape = found.dup().into();
                    JSShape::free(ctx.rt_mut(), use_sh);

                    return Ok(self.prop_values.get_mut(new_len - 1).unwrap());
                }
                None => {
                    // no matching found, clone current for update
                    let mut new_sh = use_sh.deref_mut().clone(ctx)?;
                    // register the new shape
                    new_sh.is_hashed = true;
                    ctx.rt_mut().register_shape(new_sh);

                    JSShape::free(ctx.rt_mut(), use_sh);

                    self.shape = new_sh.into();
                }
            };
        }

        debug_assert_eq!(self.shape().unwrap().header.ref_count, 1);

        // for unhashed or cloned shape: add property defination to shape
        unsafe {
            (*self.shape).add_shape_property(ctx, Some(self), prop, prop_flags)?;
        }

        let new_len = self.shape().unwrap().props.len();
        Ok(self.prop_values.get_mut(new_len - 1).unwrap())
    }

    /// A detailed version of add_property().
    /// Both `prop`, `val`, `getter` and `setter` are not consumed.
    pub unsafe fn create_property(
        &mut self,
        ctx: &mut JSContext,
        prop: JSAtom,
        val: JSValue,
        getter: JSValue,
        setter: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        macro_rules! slow_array {
            ($p:expr, $idx:expr) => {
                /* update the length field */
                let plen = (*$p).prop_values.first_mut().unwrap();
                let mut len = JS_ToUint32(ctx, (*plen).u.value)?;
                if ($idx + 1) > len {
                    // prop #0 of Array is length
                    let prop0_flags = (*(*$p).shape).props.first().unwrap().1.flags();
                    if prop0_flags & JS_PROP_WRITABLE == 0 {
                        return if (*ctx).JS_ThrowTypeErrorReadOnly(flags, JS_ATOM_length) {
                            Ok(false)
                        } else {
                            Err(-1)
                        };
                    }

                    /* XXX: should update the length after defining
                    the property */
                    len = $idx + 1;
                    set_value(ctx, &mut (*plen).u.value, JSValue::from_u32(len));
                }
            };
        }

        macro_rules! convert_to_array {
            ($p:expr, $idx:expr) => {
                if convert_fast_array_to_array(ctx, $p) != 0 {
                    return Err(-1);
                }
                slow_array!($p, $idx);
            };
        }

        // add a new property or modify an existing exotic one
        if self.detailed.is_exotic() {
            match self.class_id() {
                JS_CLASS_ARRAY => {
                    let mut idx: u32 = 0;

                    if self.detailed.fast_array() {
                        if prop.is_int_type() {
                            idx = __JS_AtomToUInt32(prop);
                            if idx == self.array_length_unchecked() as u32 {
                                if !self.detailed.extensible() {
                                    return if (*ctx).JS_ThrowTypeErrorOrFalse(
                                        flags as _,
                                        c"object is not extensible",
                                    ) {
                                        Err(-1)
                                    } else {
                                        Ok(false)
                                    };
                                }

                                if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
                                    convert_to_array!(self, idx);
                                }
                                let prop_flags = get_prop_flags(flags, 0);
                                if prop_flags != JS_PROP_C_W_E {
                                    convert_to_array!(self, idx);
                                }
                                return add_fast_array_element(ctx, self, val.dup(), flags);
                            } else {
                                convert_to_array!(self, idx);
                            }
                        } else if JS_AtomIsArrayIndex(ctx, &mut idx, prop) {
                            /* convert the fast array to normal array */
                            convert_to_array!(self, idx);
                        }
                    } else if JS_AtomIsArrayIndex(ctx, &mut idx, prop) {
                        slow_array!(self, idx);
                    }
                }
                JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                    let ret = JS_AtomIsNumericIndex(ctx, prop);
                    if ret != 0 {
                        if ret < 0 {
                            return Err(-1);
                        }
                        return if ctx.JS_ThrowTypeErrorOrFalse(
                            flags,
                            c"cannot create numeric index in typed array",
                        ) {
                            Err(-1)
                        } else {
                            Ok(false)
                        };
                    }
                }
                cid => {
                    if (flags & JS_PROP_NO_EXOTIC) == 0 {
                        let em = ctx.rt().class_array[cid as usize].exotic;
                        if !em.is_null() {
                            if let Some(cb) = (*em).define_own_property {
                                let r = cb(
                                    ctx,
                                    JS_MKPTR!(JSTag::Object, (self as *mut JSObject).cast()),
                                    prop,
                                    val,
                                    getter,
                                    setter,
                                    flags as _,
                                );
                                return if r < 0 { Err(-1) } else { Ok(r != 0) };
                            }

                            if !JS_IsExtensible(
                                ctx,
                                JS_MKPTR!(JSTag::Object, (self as *mut JSObject).cast()),
                            )? {
                                return if ctx.JS_ThrowTypeErrorOrFalse(
                                    flags as _,
                                    c"object is not extensible",
                                ) {
                                    Err(-1)
                                } else {
                                    Ok(false)
                                };
                            }
                        }
                    }
                }
            }
        }

        if !self.detailed.extensible() {
            return if ctx.JS_ThrowTypeErrorOrFalse(flags, c"object is not extensible") {
                Err(-1)
            } else {
                Ok(false)
            };
        }

        let prop_flags = if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
            (flags & (JS_PROP_CONFIGURABLE | JS_PROP_ENUMERABLE)) | JS_PROP_GETSET
        } else {
            flags & JS_PROP_C_W_E
        };
        let pr = self.add_property(ctx, prop, prop_flags)?;

        if flags & (JS_PROP_HAS_GET | JS_PROP_HAS_SET) != 0 {
            // getset property
            (*pr).u.getset.getter = std::ptr::null_mut();
            if (flags & JS_PROP_HAS_GET) != 0 && getter.is_callable(ctx) {
                (*pr).u.getset.getter = JS_VALUE_GET_OBJ(getter.dup());
            }
            (*pr).u.getset.setter = std::ptr::null_mut();
            if (flags & JS_PROP_HAS_SET) != 0 && setter.is_callable(ctx) {
                (*pr).u.getset.setter = JS_VALUE_GET_OBJ(setter.dup());
            }
        } else {
            // normal property
            (*pr).u.value = if (flags & JS_PROP_HAS_VALUE) != 0 {
                val.dup()
            } else {
                JSValue::UNDEFINED
            };
        }

        Ok(true)
    }
}

/// `val` is duplicated inside
#[deprecated(note = "use JSObject::create_property() instead")]
pub unsafe fn JS_CreateProperty(
    ctx: *mut JSContext,
    p: *mut JSObject,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    (*p).create_property(ctx.as_mut().unwrap(), prop, val, getter, setter, flags)
}
