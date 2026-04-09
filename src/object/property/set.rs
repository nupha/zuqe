// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY, JS_CLASS_BIG_UINT64_ARRAY,
    JS_CLASS_FLOAT32_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY,
    JS_CLASS_INT32_ARRAY, JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY,
    JS_CLASS_UINT8C_ARRAY, JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::Atom,
    context::ScopedContext,
    error::JS_ThrowReferenceErrorNotDefined,
    object::{
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT,
        JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_VARREF, JSProperty, Object,
        ObjectImpl,
    },
    utils::unlikely,
    value::{JS_GetPrototypePrimitive, JSValue},
};

use super::{
    JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_VALUE, JS_PROP_NO_ADD, JS_PROP_THROW,
    JS_PROP_WRITABLE,
};

impl ObjectImpl {
    /// 'flags' is a bitmask of JS_PROP_NO_ADD, JS_PROP_THROW or JS_PROP_THROW_STRICT.
    /// If JS_PROP_NO_ADD is set,  the new property is not added and an error is raised.
    ///
    /// `self` and its prototype chain is used to find property existance.
    /// in case property was not found, `receiver` is used to add the new property,
    /// this argument could be:
    /// - Some(obj): new property can be added on `obj`;
    /// - None: add new property is not allowed.
    pub(crate) fn set_property_internal(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: JSValue,
        receiver: Option<Object>,
        flags: u32,
    ) -> Result<bool, i32> {
        macro_rules! fail {
            ($msg:expr) => {
                return if ctx.rt().can_throw(flags) {
                    ctx.throw_type_error($msg);
                    Err(-1)
                } else {
                    Ok(false)
                }
            };
        }

        macro_rules! fail_read_only {
            ($prop:expr) => {
                return if ctx.rt().can_throw(flags) {
                    ctx.throw_type_error_read_only($prop);
                    Err(-1)
                } else {
                    Ok(false)
                }
            };
        }

        // to check property existing
        for mut cur_obj in self.shape_proto_iter(true) {
            // 1. find shape property
            'shape: while let Some((idx, prs_flags)) = cur_obj.find_shape_property(prop) {
                match prs_flags & JS_PROP_TMASK {
                    JS_PROP_NORMAL if receiver.is_some_and(|o| cur_obj == o) => {
                        // possible to update property value, when cur_obj == receiver
                        if unlikely(prs_flags & JS_PROP_LENGTH != 0) {
                            // set array length
                            debug_assert_eq!(cur_obj.class_id(), JS_CLASS_ARRAY);
                            debug_assert_eq!(prop, Atom::CONST_length);

                            let arr_len = val.to_array_length(ctx, false)?;
                            let can_throw = ctx.rt().can_throw(flags);

                            return unsafe {
                                cur_obj
                                    .as_Array_mut_unchecked()
                                    .set_length(ctx, arr_len, can_throw)
                            };
                        }

                        if unlikely(prs_flags & JS_PROP_WRITABLE != 0) {
                            cur_obj.set_prop_slot(idx, JSProperty::Normal(val));
                            return Ok(true);
                        } else {
                            fail_read_only!(prop)
                        }
                    }

                    JS_PROP_VARREF if receiver.is_some_and(|o| cur_obj == o) => {
                        if unlikely(cur_obj.class_id() == JS_CLASS_MODULE_NS) {
                            fail_read_only!(prop)
                        } else if let JSProperty::VarRef(vref) = cur_obj.prop_slot_mut(idx) {
                            vref.set_value(val);
                            return Ok(true);
                        } else {
                            unreachable!()
                        }
                    }

                    JS_PROP_GETSET => {
                        return if let JSProperty::GetSet { setter, .. } = cur_obj.prop_slot(idx) {
                            // protect self
                            // ctx.gc_heap_mut().protect_node_v2(self.gc_head_ptr());
                            call_setter(
                                ctx,
                                *setter,
                                receiver.unwrap_or(Object::from_ref(self)).into(),
                                val,
                                flags,
                            )
                        } else {
                            unreachable!()
                        };
                    }

                    JS_PROP_LAZYINIT => {
                        let val = {
                            let this = cur_obj;
                            cur_obj.prop_slot_mut(idx).instantiate(this)
                        };

                        // modify shape property kind to NORMAL
                        cur_obj
                            .modify_shape_property_index_flags(ctx, idx, |f| f & !JS_PROP_TMASK)?;
                        // update prop slot value
                        cur_obj.set_prop_slot(
                            idx,
                            JSProperty::Normal(if let Ok(v) = val {
                                v
                            } else {
                                JSValue::UNDEFINED
                            }),
                        );

                        val?;
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
                            if receiver.is_some_and(|o| cur_obj == o) {
                                // set property on receiver object.
                                return cur_obj.set_property_value(
                                    ctx,
                                    JSValue::Int(idx as _),
                                    val,
                                    flags,
                                );
                            } else {
                                break; // proto iter loop
                            }
                        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            // index out of bound
                            // ECMA: if receiver object is same to self, consume the value.
                            if receiver.is_some_and(|o| cur_obj == o) {
                                let _ = val.to_number(ctx)?;
                            }
                            return Ok(true);
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                        && prop.is_numeric_index(ctx)?
                    {
                        // ECMA: if receiver object is same to self, consume the value.
                        if receiver.is_some_and(|o| cur_obj == o) {
                            let _ = val.to_number(ctx)?;
                        }
                        return Ok(true);
                    }
                } else if let Some(xm) = ctx.rt_ptr().class_registry[cur_obj.class_id() as usize]
                    .exotic
                    .as_ref()
                {
                    // apply exotic methods
                    if let Some(f) = xm.set_property {
                        return f(
                            ctx,
                            JSValue::Object(cur_obj),
                            prop,
                            val,
                            receiver.unwrap_or(Object::from_ref(self)).into(),
                            flags as _,
                        );
                    }

                    if let Some(f) = xm.get_own_property
                        && let Some(desc) = f(ctx, JSValue::Object(cur_obj), prop)?
                    {
                        if desc.flags & JS_PROP_GETSET != 0 {
                            // protect self before calling setter
                            // ctx.gc_heap_mut().protect_node_v2(self.gc_head_ptr());

                            return call_setter(
                                ctx,
                                desc.setter.as_object().copied(),
                                receiver.unwrap_or(Object::from_ref(self)).into(),
                                val,
                                flags,
                            );
                        }

                        if unlikely(desc.flags & JS_PROP_WRITABLE == 0) {
                            fail_read_only!(prop)
                        }

                        if receiver.is_some_and(|o| cur_obj == o) {
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
            JS_ThrowReferenceErrorNotDefined(ctx, prop);
            return Err(-1);
        }

        // receiver must be provided in order to add new property on it.
        if let Some(mut target) = receiver {
            if unlikely(!target.flags().extensible()) {
                fail!("object is not extensible")
            }

            if target.gc_node_ptr() == self.gc_head_ptr() {
                if target.flags().is_exotic() {
                    if let Some(idx) = prop.to_small_int()
                        && let Some(arr) = target.as_Array_mut()
                        && arr.is_fast_array()
                        // && arr.class_id() == JS_CLASS_ARRAY // should exclude JS_CLASS_ARGUMENTS?
                        && idx == arr.fast_array_count()
                    {
                        return arr.add_fast_array_element(ctx, val, flags);
                    }
                } else {
                    target.add_own_property(ctx, prop, JS_PROP_C_W_E, JSProperty::Normal(val))?;
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
                prop,
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

    #[inline(always)]
    pub fn set_property(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: JSValue,
    ) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, Some(Object::from_ref(self)), JS_PROP_THROW)
    }

    /// set property value with integral index key, usually used for fast arrays.
    ///
    /// Returns:
    /// - Some: when the property was set,
    /// - None: when the property was not set, perform slow path is needed.
    fn set_property_index(
        &mut self,
        ctx: &mut ScopedContext,
        index: usize,
        val: JSValue,
        flags: u32,
    ) -> Option<Result<bool, i32>> {
        match self.class_id() {
            JS_CLASS_ARRAY => {
                if self.flags().extensible() {
                    let self_xref = self.gc_ref().node_info().partition_id();
                    let this = Object::from_ref(self);

                    if self.is_fast_array()
                        && let Some(arr_data) = self.payload_mut().array_mut()
                    {
                        match index.cmp(&arr_data.len()) {
                            std::cmp::Ordering::Less => {
                                // update existing
                                arr_data.set_nth(ctx.rt_mut(), index, val, self_xref);
                                Some(Ok(true)) // fast array exit
                            }
                            std::cmp::Ordering::Equal => {
                                // add an element to list

                                // Hint: 遵照js规范，
                                // 即使Array，它的原型链上可能还链接着其他对象。这些原型对象也可能具有数字索引的属性。
                                // 因此，在向当前数组添加新元素时，必须确保原型链上没有定义同名的属性（即相同的索引），
                                // 否则会违反 JavaScript 的属性查找规则。

                                // Note: can't use ::prototype_iter() here, because of payload borrow conflict.
                                let mut proto = this.shape().unwrap().proto();
                                while let Some(p) = proto {
                                    let sh1 = p.shape().unwrap();
                                    match p.class_id() {
                                        JS_CLASS_ARRAY => {
                                            if !p.flags().is_fast_array() {
                                                return None;
                                            }
                                        }
                                        JS_CLASS_OBJECT => {
                                            if sh1.has_small_array_index {
                                                return None;
                                            }
                                        }
                                        _ => {
                                            return None;
                                        }
                                    }
                                    proto = sh1.proto;
                                }

                                // ok to append array element
                                Some(self.add_fast_array_element(ctx, val, flags))
                            }
                            std::cmp::Ordering::Greater => {
                                None // fall to slow path, destruct to sparse array
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }

            JS_CLASS_ARGUMENTS => {
                let self_xref = self.gc_ref().node_info().partition_id();

                if self.is_fast_array()
                    && let Some(arr_data) = self.payload_mut().array_mut()
                {
                    if index < arr_data.len() {
                        arr_data.set_nth(ctx.rt_mut(), index, val, self_xref);
                        Some(Ok(true))
                    } else {
                        None
                    }
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
                let _ = Object::from_ref(self)
                    .as_TypedArray_mut()
                    .unwrap()
                    .set_element_int(index as _, v as _);
                Some(Ok(true))
            }
            JS_CLASS_INT8_ARRAY
            | JS_CLASS_UINT8_ARRAY
            | JS_CLASS_INT16_ARRAY
            | JS_CLASS_UINT16_ARRAY
            | JS_CLASS_INT32_ARRAY
            | JS_CLASS_UINT32_ARRAY => {
                let Ok(i) = val.to_number(ctx).map(|n| n.to_i32()) else {
                    return Some(Err(-1));
                };
                let _ = Object::from_ref(self)
                    .as_TypedArray_mut()
                    .unwrap()
                    .set_element_int(index as _, i);
                Some(Ok(true))
            }
            JS_CLASS_FLOAT32_ARRAY | JS_CLASS_FLOAT64_ARRAY => {
                let Ok(d) = val.to_number(ctx).map(|n| n.to_f64()) else {
                    return Some(Err(-1));
                };
                let _ = Object::from_ref(self)
                    .as_TypedArray_mut()
                    .unwrap()
                    .set_element_float(index as _, d);
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
        &mut self,
        ctx: &mut ScopedContext,
        prop: JSValue,
        val: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        // check if prop is a u32 (a.k.a. index key)
        let index_key: Option<usize> = match prop {
            JSValue::Int(i) if i >= 0 => Some(i as usize),
            JSValue::Float64(f) if f >= 0.0 => {
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
            && let Some(r) = self.set_property_index(ctx, idx, val, flags)
        {
            // early exit fast path
            return r;
        }

        // slow path
        let atom = prop.to_atom(ctx)?;
        self.set_property_internal(ctx, atom, val, Some(Object::from_ref(self)), flags)
    }

    pub fn set_property_u32(
        &mut self,
        ctx: &mut ScopedContext,
        idx: u32,
        val: JSValue,
    ) -> Result<bool, i32> {
        self.set_property_index(ctx, idx as usize, val, JS_PROP_THROW)
            .unwrap_or_else(|| {
                // slow path
                let atom = Atom::from_i64(ctx.rt_ptr(), idx as i64)?;
                self.set_property_internal(
                    ctx,
                    atom,
                    val,
                    Some(Object::from_ref(self)),
                    JS_PROP_THROW,
                )
            })
    }

    #[inline]
    pub fn set_property_i64(
        &mut self,
        ctx: &mut ScopedContext,
        idx: i64,
        val: JSValue,
    ) -> Result<bool, i32> {
        if (0..=u32::MAX as i64).contains(&idx) {
            self.set_property_u32(ctx, idx as u32, val)
        } else {
            let atom = Atom::from_i64(ctx.rt_ptr(), idx)?;
            self.set_property(ctx, atom, val)
        }
    }

    #[inline]
    pub fn set_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
        val: JSValue,
    ) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.set_property(ctx, atom, val)
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
        ctx: &mut ScopedContext,
        prop: Atom,
        val: Self,
        receiver: Option<Object>,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(o) = self {
            o.set_property_internal(ctx, prop, val, receiver.or(Some(*o)), flags)
        } else if let JSValue::Object(p) = JS_GetPrototypePrimitive(ctx, self) {
            p.set_property_internal(ctx, prop, val, receiver, flags)
        } else if unlikely(ctx.rt().can_throw(flags)) {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else {
            Ok(false)
        }
    }

    #[inline(always)]
    pub fn set_property(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: Self,
    ) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, None, JS_PROP_THROW)
    }

    #[inline]
    pub fn set_property_value(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Self,
        val: Self,
        flags: u32,
    ) -> Result<bool, i32> {
        if let Self::Object(o) = self {
            o.set_property_value(ctx, prop, val, flags)
        } else {
            let atom = prop.to_atom(ctx)?;
            self.set_property_internal(ctx, atom, val, None, flags)
        }
    }

    #[inline]
    pub fn set_property_u32(
        &mut self,
        ctx: &mut ScopedContext,
        idx: u32,
        val: Self,
    ) -> Result<bool, i32> {
        if let Self::Object(ob) = self {
            ob.set_property_u32(ctx, idx, val)
        } else {
            self.set_property_value(ctx, JSValue::from(idx), val, JS_PROP_THROW)
        }
    }

    #[inline]
    pub fn set_property_i64(
        &mut self,
        ctx: &mut ScopedContext,
        idx: i64,
        val: Self,
    ) -> Result<bool, i32> {
        if let Some(ob) = self.as_object_mut() {
            ob.set_property_i64(ctx, idx, val)
        } else {
            self.set_property_value(ctx, JSValue::from(idx), val, JS_PROP_THROW)
        }
    }

    #[inline]
    pub fn set_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
        val: Self,
    ) -> Result<bool, i32> {
        let atom = Atom::new(ctx.rt_ptr(), prop)?;
        self.set_property(ctx, atom, val)
    }
}

#[inline(always)]
fn call_setter(
    ctx: &mut ScopedContext,
    setter: Option<Object>,
    this_obj: JSValue,
    value: JSValue,
    flags: u32,
) -> Result<bool, i32> {
    if let Some(func) = setter {
        func.call(ctx, this_obj, std::slice::from_ref(&value))?;
        Ok(true)
    } else if unlikely(ctx.rt().can_throw(flags)) {
        ctx.throw_type_error("no setter for property");
        Err(-1)
    } else {
        Ok(false)
    }
}
