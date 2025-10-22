use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::{Atom, JS_AtomIsNumericIndex, consts::JS_ATOM_length},
    branch_hints::unlikely,
    context::JSContext,
    error::JS_ThrowReferenceErrorNotDefined,
    object::{
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT,
        JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_VARREF, JSObject, JSProperty,
    },
    typed_array::TypedArray,
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{
    JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_VALUE, JS_PROP_NO_ADD, JS_PROP_THROW,
    JS_PROP_WRITABLE,
};

impl JSObject {
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// `self` and its prototype chain is used to find property existance.
    /// in case property was not found, `target` is used to add the new property,
    /// this argument could be:
    /// - Some(obj): new property can be added on `obj`;
    /// - None: add new property is not allowed.
    pub(crate) fn set_property_internal(
        &self,
        ctx: &JSContext,
        prop: &Atom,
        val: JSValue,
        target: Option<&JSObject>,
        flags: u32,
    ) -> Result<bool, i32> {
        macro_rules! fail {
            ($msg:expr) => {
                return if ctx.can_throw(flags) {
                    ctx.throw_type_error($msg);
                    Err(-1)
                } else {
                    Ok(false)
                }
            };
        }

        macro_rules! fail_read_only {
            ($prop:expr) => {
                return if ctx.can_throw(flags) {
                    ctx.throw_type_error_read_only($prop.to_ref());
                    Err(-1)
                } else {
                    Ok(false)
                }
            };
        }

        // to check property existing
        for cur_obj in self.shape_proto_iter(true) {
            // 1. find shape property
            'shape: while let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                match prs_flags & JS_PROP_TMASK {
                    JS_PROP_NORMAL if target.is_some_and(|o| cur_obj.eq(o)) => {
                        // possible to update property value, when cur_obj == receiver
                        if unlikely(prs_flags & JS_PROP_LENGTH != 0) {
                            // set array length
                            debug_assert_eq!(cur_obj.class_id(), JS_CLASS_ARRAY);
                            debug_assert_eq!(prop.to_ref(), JS_ATOM_length);
                            return cur_obj.as_Array().unwrap().set_length(
                                ctx,
                                val.to_array_length(ctx, false)?,
                                ctx.can_throw(flags),
                            );
                        }

                        if unlikely(prs_flags & JS_PROP_WRITABLE != 0) {
                            cur_obj.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(val);
                            return Ok(true);
                        } else {
                            fail_read_only!(prop)
                        }
                    }

                    JS_PROP_VARREF if target.is_some_and(|o| cur_obj.eq(o)) => {
                        if unlikely(cur_obj.class_id() == JS_CLASS_MODULE_NS) {
                            fail_read_only!(prop)
                        } else if let Some(JSProperty::VarRef(vref)) =
                            cur_obj.0.prop_values.borrow_mut().get_mut(idx)
                        {
                            vref.set_value(val);
                            return Ok(true);
                        } else {
                            unreachable!()
                        }
                    }

                    JS_PROP_GETSET => {
                        let gs =
                            unsafe { cur_obj.0.prop_values.borrow().get_unchecked(idx).clone() };

                        return if let JSProperty::GetSet { setter, .. } = gs {
                            call_setter(
                                ctx,
                                setter.as_ref(),
                                &target.unwrap_or(self).clone().into(),
                                val,
                                flags,
                            )
                        } else {
                            unreachable!()
                        };
                    }

                    JS_PROP_LAZYINIT => {
                        unsafe {
                            cur_obj
                                .0
                                .prop_values
                                .borrow_mut()
                                .get_unchecked_mut(idx)
                                .instantiate(&cur_obj, idx, &cur_obj)?;
                        }
                        continue 'shape;
                    }

                    JS_PROP_NORMAL | JS_PROP_VARREF => {
                        // in case proto != this
                        if unlikely(prs_flags & JS_PROP_WRITABLE == 0) {
                            fail_read_only!(prop)
                        }
                        break;
                    }
                    _ => {
                        unreachable!();
                    }
                }
            }

            // 2. check exotic handlers
            let obj_flags = cur_obj.flags();
            if obj_flags.is_exotic() {
                if obj_flags.is_fast_array() {
                    if let Some(idx) = prop.to_small_int() {
                        if idx < cur_obj.fast_array_count() {
                            return if target.is_some_and(|o| cur_obj.eq(o)) {
                                cur_obj.set_property_value(ctx, &JSValue::Int(idx as _), val, flags)
                            } else {
                                break; // proto iter loop
                            };
                        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            return Ok(true); // typed array oob
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                    {
                        let ret = unsafe { JS_AtomIsNumericIndex(ctx, prop) };
                        if unlikely(ret < 0) {
                            return Err(-1);
                        }
                        if ret == 1 {
                            return Ok(true); // typed array oob
                        }
                    }
                } else if let Some(xm) = ctx.rt().class_registry[cur_obj.class_id() as usize]
                    .exotic
                    .as_ref()
                {
                    // apply exotic methods
                    if let Some(sp) = xm.set_property {
                        return sp(
                            ctx,
                            JSValue::Object(cur_obj.clone()),
                            prop,
                            val,
                            target.unwrap_or(self).clone().into(),
                            flags as _,
                        );
                    }

                    if let Some(f) = xm.get_own_property
                        && let Some(desc) = f(ctx, JSValue::Object(cur_obj.clone()), prop)?
                    {
                        if desc.flags & JS_PROP_GETSET != 0 {
                            return call_setter(
                                ctx,
                                desc.setter.as_object(),
                                &target.unwrap_or(self).clone().into(),
                                val,
                                flags,
                            );
                        }

                        if unlikely(desc.flags & JS_PROP_WRITABLE == 0) {
                            fail_read_only!(prop)
                        }

                        if target.is_some_and(|o| cur_obj.eq(o)) {
                            return cur_obj.define_property(
                                ctx,
                                prop,
                                val,
                                JSValue::UNDEFINED,
                                JSValue::UNDEFINED,
                                JS_PROP_HAS_VALUE,
                            );
                        }
                    }
                }
            }
        }

        //
        // add the property
        //

        if unlikely(flags & JS_PROP_NO_ADD != 0) {
            JS_ThrowReferenceErrorNotDefined(ctx, prop.to_ref());
            return Err(-1);
        }

        // target must be provided in order to add new property on it.
        if let Some(target) = target {
            if unlikely(!target.flags().extensible()) {
                fail!("object is not extensible")
            }

            if target == self {
                if target.flags().is_exotic() {
                    if let Some(idx) = prop.to_small_int()
                        && let Some(arr) = target.as_Array()
                        && arr.is_fast_array()
                        // && arr.class_id() == JS_CLASS_ARRAY // should exclude JS_CLASS_ARGUMENTS?
                        && idx == arr.fast_array_count()
                    {
                        return arr.add_fast_array_element(ctx, val, flags);
                    }
                } else {
                    target.add_own_property(
                        ctx,
                        prop.clone(),
                        JS_PROP_C_W_E,
                        JSProperty::Normal(val),
                    )?;
                    return Ok(true);
                }
            } else {
                // generic case: modify the property on `target` if it already exists
                if let Some(desc) = target.get_own_property(ctx, prop)? {
                    if unlikely(desc.flags & JS_PROP_GETSET != 0) {
                        fail!("setter is forbidden")
                    } else if unlikely(
                        desc.flags & JS_PROP_WRITABLE == 0
                            || target.class_id() == JS_CLASS_MODULE_NS,
                    ) {
                        fail_read_only!(prop);
                    } else {
                        return target.define_property(
                            ctx,
                            prop,
                            val,
                            JSValue::UNDEFINED,
                            JSValue::UNDEFINED,
                            JS_PROP_HAS_VALUE,
                        );
                    }
                }
            }

            // create property on `target`, returns Result<bool, i32>
            target.create_property(
                ctx,
                prop.clone(),
                val,
                JSValue::UNDEFINED,
                JSValue::UNDEFINED,
                flags
                    | JS_PROP_HAS_VALUE
                    | JS_PROP_HAS_ENUMERABLE
                    | JS_PROP_HAS_WRITABLE
                    | JS_PROP_HAS_CONFIGURABLE
                    | JS_PROP_C_W_E,
            )
        } else {
            // receiver is not provided, add property is not allowed
            fail!("not an object");
        }
    }

    /// `prop` is not consumed.
    #[inline(always)]
    pub fn set_property(&self, ctx: &JSContext, prop: &Atom, val: JSValue) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, Some(self), JS_PROP_THROW)
    }

    /// set property value with integral index key, usually used for fast arrays.
    ///
    /// Returns:
    /// - Some: when the property was set,
    /// - None: when the property was not set, perform slow path is needed.
    fn set_property_index(
        &self,
        ctx: &JSContext,
        idx: usize,
        val: JSValue,
        flags: u32,
    ) -> Option<Result<bool, i32>> {
        match self.class_id() {
            JS_CLASS_ARRAY => {
                if self.flags().extensible() {
                    let ret = self
                        .with_fast_array_values_mut(|lst| -> Option<Result<bool, i32>> {
                            match idx.cmp(&lst.len()) {
                                std::cmp::Ordering::Less => {
                                    // update existing
                                    unsafe {
                                        *lst.get_unchecked_mut(idx) = val.clone();
                                    }
                                    Some(Ok(true))
                                }
                                std::cmp::Ordering::Equal => {
                                    // add an element to list

                                    // Hint: 遵照js规范，
                                    // 即使Array，它的原型链上可能还链接着其他对象。这些原型对象也可能具有数字索引的属性。
                                    // 因此，在向当前数组添加新元素时，必须确保原型链上没有定义同名的属性（即相同的索引），
                                    // 否则会违反 JavaScript 的属性查找规则。

                                    // Note: can't use ::prototype_iter() here, because of payload borrow conflict.
                                    let mut up = self.shape().unwrap().0.borrow().proto.clone();
                                    while let Some(p) = up {
                                        let sh1 = p.shape().unwrap();
                                        match p.class_id() {
                                            JS_CLASS_ARRAY => {
                                                if !p.flags().is_fast_array() {
                                                    return None;
                                                }
                                            }
                                            JS_CLASS_OBJECT => {
                                                if sh1.0.borrow().has_small_array_index {
                                                    return None;
                                                }
                                            }
                                            _ => {
                                                return None;
                                            }
                                        }
                                        up = sh1.0.borrow().proto.clone();
                                    }

                                    Some(Ok(false)) // <-- indicated to append element
                                }
                                std::cmp::Ordering::Greater => {
                                    // go slow path, destruct to sparse array
                                    None
                                }
                            }
                        })
                        .flatten();

                    if let Some(Ok(false)) = ret {
                        // append element
                        Some(self.add_fast_array_element(ctx, val, flags))
                    } else {
                        ret
                    }
                } else {
                    None
                }
            }

            JS_CLASS_ARGUMENTS => {
                if self
                    .with_fast_array_values_mut(|lst| -> bool {
                        if idx < lst.len() {
                            unsafe {
                                *lst.get_unchecked_mut(idx) = val.clone();
                            }
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap()
                {
                    // early exit for fast array
                    Some(Ok(true))
                } else {
                    None
                }
            }

            //
            // typed array
            //
            JS_CLASS_UINT8C_ARRAY => {
                let Ok(v) = val.coerce_to_u8_clamp(ctx) else {
                    return Some(Err(-1));
                };
                let _ = TypedArray(self.clone()).set_element_int(idx as _, v as _);
                Some(Ok(true))
            }
            JS_CLASS_INT8_ARRAY
            | JS_CLASS_UINT8_ARRAY
            | JS_CLASS_INT16_ARRAY
            | JS_CLASS_UINT16_ARRAY
            | JS_CLASS_INT32_ARRAY
            | JS_CLASS_UINT32_ARRAY => {
                let Ok(v) = val.to_i32(ctx) else {
                    return Some(Err(-1));
                };
                let _ = TypedArray(self.clone()).set_element_int(idx as _, v);
                Some(Ok(true))
            }
            JS_CLASS_FLOAT32_ARRAY | JS_CLASS_FLOAT64_ARRAY => {
                let Ok(v) = val.to_f64(ctx) else {
                    return Some(Err(-1));
                };
                let _ = TypedArray(self.clone()).set_element_float(idx as _, v);
                Some(Ok(true))
            }
            JS_CLASS_BIG_INT64_ARRAY | JS_CLASS_BIG_UINT64_ARRAY => {
                unimplemented!()
            }

            _ => None,
        }
    }

    /// flags can be JS_PROP_THROW or JS_PROP_THROW_STRICT
    pub fn set_property_value(
        &self,
        ctx: &JSContext,
        prop: &JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // check if prop is a u32 (a.k.a. index key)
        let index_key: Option<usize> = match prop {
            JSValue::Int(i) if *i >= 0 => Some(*i as usize),
            JSValue::Float64(f) if *f >= 0.0 => {
                if f.fract() == 0.0 {
                    let n = f.trunc() as i64;
                    if n <= u32::MAX as i64 {
                        Some(n as usize)
                    } else {
                        None
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(idx) = index_key
            && let Some(r) = self.set_property_index(ctx, idx, val.clone(), flags)
        {
            // early exit for fast path
            return r;
        }

        // slow path
        let atom = prop.to_atom(ctx)?;
        self.set_property_internal(ctx, &atom, val, Some(self), flags)
    }

    pub fn set_property_u32(&self, ctx: &JSContext, idx: u32, val: JSValue) -> Result<bool, i32> {
        self.set_property_index(ctx, idx as usize, val.clone(), JS_PROP_THROW)
            .unwrap_or_else(|| {
                // slow path
                let atom = Atom::from_i64(ctx.rt(), idx as i64)?;
                self.set_property_internal(ctx, &atom, val, Some(self), JS_PROP_THROW)
            })
    }

    #[inline]
    pub fn set_property_i64(&self, ctx: &JSContext, idx: i64, val: JSValue) -> Result<bool, i32> {
        if (0..=u32::MAX as i64).contains(&idx) {
            self.set_property_u32(ctx, idx as u32, val)
        } else {
            let atom = Atom::from_i64(ctx.rt(), idx)?;
            self.set_property(ctx, &atom, val)
        }
    }

    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt(), prop)?;
        self.set_property(ctx, &atom, val)
    }
}

impl JSValue {
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// In case of `Reflect.set(obj, val, [receiver])`, `receiver` could be null, undefined or Some(object).
    /// For other cases,  `receiver` should be None, suggesting it's same to `self`.
    pub(crate) fn set_property_internal(
        &self,
        ctx: &JSContext,
        prop: &Atom,
        val: Self,
        receiver: Option<&JSObject>,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(o) = self {
            o.set_property_internal(ctx, prop, val, receiver.or(Some(o)), flags)
        } else if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, self) {
            p.set_property_internal(ctx, prop, val, receiver, flags)
        } else if unlikely(ctx.can_throw(flags)) {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else {
            Ok(false)
        }
    }

    /// `prop` is not consumed.
    #[inline(always)]
    pub fn set_property(&self, ctx: &JSContext, prop: &Atom, val: JSValue) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, None, JS_PROP_THROW)
    }

    #[inline]
    pub fn set_property_value(
        &self,
        ctx: &JSContext,
        prop: &Self,
        val: Self,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(o) = self {
            o.set_property_value(ctx, prop, val, flags)
        } else {
            let atom = prop.to_atom(ctx)?;
            self.set_property_internal(ctx, &atom, val, None, flags)
        }
    }

    #[inline]
    pub fn set_property_u32(&self, ctx: &JSContext, idx: u32, val: JSValue) -> Result<bool, i32> {
        if let Self::Object(ob) = self {
            ob.set_property_u32(ctx, idx, val)
        } else {
            self.set_property_value(ctx, &JSValue::from(idx), val, JS_PROP_THROW)
        }
    }

    #[inline]
    pub fn set_property_i64(&self, ctx: &JSContext, idx: i64, val: JSValue) -> Result<bool, i32> {
        if let Some(ob) = self.as_object() {
            ob.set_property_i64(ctx, idx, val)
        } else {
            self.set_property_value(ctx, &JSValue::from(idx), val, JS_PROP_THROW)
        }
    }

    pub fn set_property_str(&self, ctx: &JSContext, prop: &str, val: JSValue) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt(), prop)?;
        self.set_property(ctx, &atom, val)
    }
}

fn call_setter(
    ctx: &JSContext,
    setter: Option<&JSObject>,
    this_obj: &JSValue,
    val: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    if let Some(func) = setter {
        JSValue::Object(func.clone()).call(ctx, this_obj, std::slice::from_ref(&val))?;
        Ok(true)
    } else if unlikely(ctx.can_throw(flags)) {
        ctx.throw_type_error("no setter for property");
        Err(-1)
    } else {
        Ok(false)
    }
}
