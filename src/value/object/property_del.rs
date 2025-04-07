use crate::{
    array::convert_fast_array_to_array,
    atom::{
        JSAtom, JS_AtomIsArrayIndex, JS_FreeAtom, JS_NewAtomInt64, JS_ATOM_MAX_INT, JS_ATOM_NULL,
    },
    context::{is_strict_mode, JSContext},
    value::{JSTag, JSValueConst, JS_DupValue, JS_IsException, JS_VALUE_GET_OBJ},
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY, JS_MKPTR,
};

use super::{
    super::{JSValue, JS_FreeValue},
    free_property,
    obj::JS_ToObject,
    JSObject, JSShapeProperty, JS_PROP_CONFIGURABLE, JS_PROP_THROW, JS_PROP_THROW_STRICT,
};

impl JSObject {
    pub unsafe fn delete_property(
        &mut self,
        ctx: &mut JSContext,
        atom: JSAtom,
    ) -> Result<bool, i32> {
        let sh = self.shape_mut().unwrap();
        let mut idx_and_flags: Option<(usize, u32)> = None;

        if let Some((idx, _, prs)) = sh.props.get_full_mut(&atom) {
            let flags = prs.flags();
            if flags & JS_PROP_CONFIGURABLE == 0 {
                return Ok(false);
            }

            let mut prs = prs as *mut JSShapeProperty;
            if self.js_shape_prepare_update(ctx, std::ptr::addr_of_mut!(prs)) != 0 {
                return Err(-1);
            }

            // self.shape could be changed in js_shape_prepare_update()

            idx_and_flags = Some((idx, flags));
        }

        if let Some((idx, flags)) = idx_and_flags {
            // remove and free property value
            let mut val = self.prop_values.remove(idx);
            free_property((*ctx).rt_mut(), &mut val, flags);

            // remove shape property of current self.shape
            let prs = self.shape_mut().unwrap().props.shift_remove(&atom).unwrap();
            JS_FreeAtom(ctx, prs.atom);

            return Ok(true);
        }

        loop {
            if self.detailed.is_exotic() {
                if self.detailed.fast_array() {
                    let mut idx: u32 = 0;

                    if JS_AtomIsArrayIndex(ctx, &mut idx, atom) {
                        match self.class_id() {
                            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS => {
                                let lst = self.fast_array_values_mut_unchecked();
                                if idx == lst.len() as u32 - 1 {
                                    JS_FreeValue(ctx, lst.pop().unwrap());
                                    return Ok(true);
                                } else if idx < lst.len() as u32 {
                                    // convert to slow array, then delete property
                                    if convert_fast_array_to_array(ctx, self) != 0 {
                                        return Err(-1);
                                    }
                                    continue;
                                }
                            }
                            JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY => {
                                if idx < self.array_length_unchecked() as u32 {
                                    // can't delete element of a TypedArray
                                    return Ok(false);
                                }
                            }
                            _ => {
                                unreachable!()
                            }
                        }
                    }
                } else {
                    // use exotic delete_property callback
                    let em = ctx.rt().class_array[self.class_id() as usize].exotic;
                    if !em.is_null() {
                        if let Some(cb) = (*em).delete_property {
                            return match cb(
                                ctx,
                                JS_MKPTR!(JSTag::Object, self as *mut JSObject as _),
                                atom,
                            ) {
                                0 => Ok(false),
                                1 => Ok(true),
                                -1 => Err(-1),
                                _ => unreachable!(),
                            };
                        }
                    }
                }
            }

            break;
        }

        return Ok(true);
    }
}

impl JSValue {
    /// 如果属性不可配置或无效对象则返回 false, 出现异常时返回 Err.
    /// flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT
    pub fn delete_property(
        self,
        ctx: &mut JSContext,
        prop: JSAtom,
        flags: u32,
    ) -> Result<bool, i32> {
        let obj1 = unsafe { JS_ToObject(ctx, self) };
        if obj1.is_exception() {
            return Err(-1);
        }

        unsafe {
            let p = JS_VALUE_GET_OBJ(obj1);
            let res = (*p).delete_property(ctx, prop);
            JS_FreeValue(ctx, obj1);

            if res? {
                Ok(true)
            } else if flags & JS_PROP_THROW != 0
                || ((flags & JS_PROP_THROW_STRICT) != 0 && is_strict_mode(ctx))
            {
                ctx.JS_ThrowTypeError(c"could not delete property");
                Err(-1)
            } else {
                Ok(false)
            }
        }
    }

    pub fn delete_property_i64(
        self,
        ctx: &mut JSContext,
        idx: i64,
        flags: u32,
    ) -> Result<bool, i32> {
        if (idx as u64) <= JS_ATOM_MAX_INT as u64 {
            // fast path for fast arrays
            return self.delete_property(ctx, JSAtom::from_u32(idx as u32), flags);
        }

        let prop = ctx.rt_mut().add_atom_i64(idx);
        if prop == JS_ATOM_NULL {
            return Err(-1);
        }

        unsafe {
            let res = self.delete_property(ctx, prop, flags);
            JS_FreeAtom(ctx, prop);
            res
        }
    }
}

/* 返回 -1、FALSE 或 TRUE。如果不可配置或无效对象则返回 FALSE。出现异常时返回 -1。
flags 可以是 0、JS_PROP_THROW 或 JS_PROP_THROW_STRICT */
#[deprecated(note = "use JSValue::delete_property() instead")]
pub unsafe fn JS_DeleteProperty(
    ctx: *mut JSContext,
    obj: JSValueConst,
    prop: JSAtom,
    flags: u32,
) -> Result<bool, i32> {
    obj.delete_property(ctx.as_mut().unwrap(), prop, flags)

    // let obj1 = JS_ToObject(ctx, obj);
    // if JS_IsException(obj1) {
    //     return Err(-1);
    // }

    // let p = JS_VALUE_GET_OBJ(obj1);
    // let res = p.as_mut().unwrap().delete_property(ctx, prop);
    // JS_FreeValue(ctx, obj1);
    // if res < 0 {
    //     return Err(-1);
    // } else if res != 0 {
    //     return Ok(true);
    // } else if flags & JS_PROP_THROW != 0
    //     || ((flags & JS_PROP_THROW_STRICT) != 0 && is_strict_mode(ctx))
    // {
    //     ctx.as_mut()
    //         .unwrap()
    //         .JS_ThrowTypeError(c"could not delete property");

    //     Err(-1)
    // } else {
    //     Ok(false)
    // }
}

#[deprecated(note = "use JSValue::delete_property_i64() instead")]
pub unsafe fn JS_DeletePropertyInt64(
    ctx: *mut JSContext,
    obj: JSValueConst,
    idx: i64,
    flags: u32,
) -> Result<bool, i32> {
    obj.delete_property_i64(ctx.as_mut().unwrap(), idx, flags)
}
