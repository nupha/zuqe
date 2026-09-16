// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;

use crate::{
    IntOrFloat, JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BIG_INT64_ARRAY,
    JS_CLASS_BIG_UINT64_ARRAY, JS_CLASS_FLOAT16_ARRAY, JS_CLASS_FLOAT32_ARRAY,
    JS_CLASS_FLOAT64_ARRAY, JS_CLASS_INT8_ARRAY, JS_CLASS_INT16_ARRAY, JS_CLASS_INT32_ARRAY,
    JS_CLASS_MODULE_NS, JS_CLASS_OBJECT, JS_CLASS_UINT8_ARRAY, JS_CLASS_UINT8C_ARRAY,
    JS_CLASS_UINT16_ARRAY, JS_CLASS_UINT32_ARRAY,
    atom::Atom,
    context::ScopedContext,
    error::{JS_ThrowReferenceErrorNotDefined, JS_ThrowReferenceErrorUninitialized},
    function::prop_str_to_atom,
    gc::GcHeapExt,
    object::{
        JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_WRITABLE, JS_PROP_LAZYINIT,
        JS_PROP_LENGTH, JS_PROP_NO_ADD, JS_PROP_NORMAL, JS_PROP_THROW, JS_PROP_THROW_STRICT,
        JS_PROP_TMASK, JS_PROP_VARREF, JSProperty, Object, ObjectImpl, ThrowPolicy,
    },
    utils::unlikely,
    value::JSValue,
};

use super::{JS_PROP_C_W_E, JS_PROP_GETSET, JS_PROP_HAS_VALUE, JS_PROP_WRITABLE};

#[inline(always)]
const fn set_flags(throw: ThrowPolicy, no_add: bool) -> u32 {
    let mut f = match throw {
        ThrowPolicy::Always => JS_PROP_THROW,
        ThrowPolicy::StrictOnly => JS_PROP_THROW_STRICT,
        ThrowPolicy::Never => 0,
    };
    if no_add {
        f |= JS_PROP_NO_ADD;
    }
    f
}

impl ObjectImpl {
    /// Core `[[Set]]` implementation.
    ///
    /// Throw policy: `ThrowPolicy::Always` for explicit `Set(..., true)`;
    /// `ThrowPolicy::StrictOnly` for generic PutValue/`[[Set]]`.
    /// If `no_add` is true, new properties are never created (strict
    /// SetMutableBinding semantics).
    ///
    /// `self` and its prototype chain are searched for an existing property.
    /// When none is found, `receiver` (if `Some`) receives the new property.
    /// A `None` receiver means "do not add".
    pub(crate) fn set_property_internal(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: JSValue,
        receiver: Option<Object>,
        throw: ThrowPolicy,
        no_add: bool,
    ) -> Result<bool, i32> {
        let can_throw = throw.can_throw(ctx.rt().is_strict_mode());

        macro_rules! fail {
            ($msg:expr) => {
                return if can_throw {
                    Err(ctx.throw_type_error($msg))
                } else {
                    Ok(false)
                }
            };
        }
        macro_rules! fail_read_only {
            ($prop:expr) => {
                return if can_throw {
                    Err(ctx.throw_type_error_read_only($prop))
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
                        if unlikely(prs_flags & JS_PROP_LENGTH != 0) {
                            debug_assert_eq!(cur_obj.class_id(), JS_CLASS_ARRAY);
                            debug_assert_eq!(prop, Atom::CONST_length);

                            let length = val.to_array_length(ctx, false)?;

                            return unsafe {
                                cur_obj
                                    .as_Array_mut_unchecked()
                                    .set_length(ctx, length, can_throw)
                            };
                        } else if prs_flags & JS_PROP_WRITABLE != 0 {
                            cur_obj.set_prop_slot(idx, JSProperty::Normal(val));
                            return Ok(true);
                        } else {
                            fail_read_only!(prop)
                        }
                    }

                    JS_PROP_VARREF if receiver.is_some_and(|o| cur_obj == o) => {
                        if unlikely(cur_obj.class_id() == JS_CLASS_MODULE_NS) {
                            let v = cur_obj
                                .prop_slot(idx)
                                .value(ctx, receiver.unwrap().into())?;
                            if unlikely(v.is_uninitialized()) {
                                return Err(JS_ThrowReferenceErrorUninitialized(ctx, prop));
                            }
                            fail_read_only!(prop)
                        } else if let JSProperty::VarRef(vref) = cur_obj.prop_slot_mut(idx) {
                            vref.set_value(&mut ctx.rt_ptr(), val);
                            return Ok(true);
                        } else {
                            unreachable!()
                        }
                    }

                    JS_PROP_GETSET => {
                        return if let JSProperty::GetSet { setter, .. } = cur_obj.prop_slot(idx) {
                            call_setter(
                                ctx,
                                *setter,
                                receiver.unwrap_or(Object::from_ref(self)).into(),
                                val,
                                throw,
                            )
                        } else {
                            unreachable!()
                        };
                    }

                    JS_PROP_LAZYINIT => {
                        let pd = cur_obj.replace_prop_slot(idx, JSProperty::default());
                        debug_assert!(matches!(pd, JSProperty::LazyInit { .. }));

                        let idx = cur_obj
                            .modify_shape_property_index_flags(ctx, idx, |f| f & !JS_PROP_TMASK)?;

                        let val = pd.instantiate(prop, Object::from_ref(self));
                        cur_obj.set_prop_slot(idx, JSProperty::Normal(val?));

                        continue 'shape;
                    }

                    JS_PROP_NORMAL | JS_PROP_VARREF => {
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
                                return cur_obj.set_property_value(
                                    ctx,
                                    JSValue::from(idx as i32),
                                    val,
                                    throw,
                                    no_add,
                                );
                            } else {
                                break;
                            }
                        } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                            .contains(&cur_obj.class_id())
                        {
                            if receiver.is_some_and(|o| cur_obj == o) {
                                let _ = val.to_numeric(ctx)?;
                            }
                            return Ok(true);
                        }
                    } else if (JS_CLASS_UINT8C_ARRAY..=JS_CLASS_FLOAT64_ARRAY)
                        .contains(&cur_obj.class_id())
                        && prop.is_numeric_index(ctx)?
                    {
                        if receiver.is_some_and(|o| cur_obj == o) {
                            let _ = val.to_numeric(ctx)?;
                        }
                        return Ok(true);
                    }
                } else if let Some(xm) = ctx.rt().class_def(cur_obj.class_id()).unwrap().exotic {
                    if let Some(f) = xm.set_property {
                        return f(
                            ctx,
                            JSValue::from(cur_obj),
                            prop,
                            val,
                            receiver.unwrap_or(Object::from_ref(self)).into(),
                            set_flags(throw, no_add) as _,
                        );
                    }

                    if let Some(f) = xm.get_own_property
                        && let Some(desc) = f(ctx, JSValue::from(cur_obj), prop)?
                    {
                        if desc.flags & JS_PROP_GETSET != 0 {
                            return call_setter(
                                ctx,
                                desc.setter.as_object(),
                                receiver.unwrap_or(Object::from_ref(self)).into(),
                                val,
                                throw,
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
        // property is not defined, add the property
        //
        if unlikely(no_add) {
            return Err(JS_ThrowReferenceErrorNotDefined(ctx, prop));
        }

        if let Some(mut target) = receiver {
            if target.gc_node_ptr() != self.gc_head_ptr()
                && let Some(desc) = target.get_own_property(ctx, prop)?
            {
                if unlikely(desc.flags & JS_PROP_GETSET != 0) {
                    fail!("setter is forbidden")
                } else if unlikely(
                    desc.flags & JS_PROP_WRITABLE == 0 || target.class_id() == JS_CLASS_MODULE_NS,
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

            if unlikely(!target.flags().extensible()) {
                fail!("object is not extensible")
            }

            if target.gc_node_ptr() == self.gc_head_ptr() {
                if target.flags().is_exotic() {
                    if let Some(idx) = prop.to_small_int()
                        && let Some(arr) = target.as_Array_mut()
                        && arr.is_fast_array()
                        && arr.class_id() == JS_CLASS_ARRAY
                        && idx == arr.fast_array_count()
                    {
                        return arr.add_fast_array_element(ctx, val, set_flags(throw, no_add));
                    }
                } else {
                    target.add_own_property(ctx, prop, JS_PROP_C_W_E, JSProperty::Normal(val))?;
                    return Ok(true);
                }
            }

            target.create_property(
                ctx,
                prop,
                val,
                JSValue::UNDEFINED,
                JSValue::UNDEFINED,
                set_flags(throw, no_add)
                    | JS_PROP_HAS_VALUE
                    | JS_PROP_HAS_ENUMERABLE
                    | JS_PROP_HAS_WRITABLE
                    | JS_PROP_HAS_CONFIGURABLE
                    | JS_PROP_C_W_E,
            )
        } else {
            fail!("not an object");
        }
    }

    /// Internal value-keyed set (not a user-facing convenience).
    /// Bytecode needs `no_add` for strict `SetMutableBinding`; always-throw
    /// stays reserved for explicit `Set(..., true)` ops.
    pub(crate) fn set_property_value(
        &mut self,
        ctx: &mut ScopedContext,
        prop: JSValue,
        val: JSValue,
        throw: ThrowPolicy,
        no_add: bool,
    ) -> Result<bool, i32> {
        let index_key: Option<usize> = if let Some(num) = prop.as_number() {
            match num {
                IntOrFloat::Int64(i) if i >= 0 => Some(i as usize),
                IntOrFloat::Float64(f) if f >= 0.0 && f.fract() == 0.0 => {
                    let n = f.trunc() as i64;
                    if n <= u32::MAX as i64 {
                        Some(n as usize)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        } else {
            None
        };

        if let Some(idx) = index_key
            && let Some(r) = self.set_property_u32_internal(ctx, idx, val, throw, no_add)
        {
            return r;
        }

        let atom = prop.to_atom(ctx)?;
        self.set_property_internal(ctx, atom, val, Some(Object::from_ref(self)), throw, no_add)
    }

    /// Generic `[[Set]]` with receiver = `self`. New properties are created as
    /// **{writable:true, enumerable:true, configurable:true}** per ECMA
    /// `[[Set]]` → `CreateDataProperty`.
    ///
    /// This is the spec's `Set(O, P, V, true)` flavor used by engine-internal
    /// algorithms (Array methods, Object.assign, RegExp `lastIndex`, ...), so
    /// failure always throws regardless of strict mode. Generic JS assignment
    /// (`PutValue`) uses `set_property_value` with `ThrowPolicy::StrictOnly`.
    #[inline(always)]
    pub fn set_property(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: JSValue,
    ) -> Result<bool, i32> {
        self.set_property_internal(
            ctx,
            prop,
            val,
            Some(Object::from_ref(self)),
            ThrowPolicy::Always,
            false,
        )
    }

    /// Set property by integral index, fast-path for dense arrays.
    /// Returns `Some(r)` on fast path; `None` when slow path is needed.
    fn set_property_u32_internal(
        &mut self,
        ctx: &mut ScopedContext,
        index: usize,
        val: JSValue,
        throw: ThrowPolicy,
        no_add: bool,
    ) -> Option<Result<bool, i32>> {
        match self.class_id() {
            JS_CLASS_ARRAY => {
                if self.flags().extensible() {
                    let this = Object::from_ref(self);

                    if self.is_fast_array()
                        && let Some(arr_data) = self.payload_array_mut()
                    {
                        match index.cmp(&arr_data.len()) {
                            std::cmp::Ordering::Less => {
                                arr_data.set_nth(index, val);
                                let self_node = self.gc_head_ptr();
                                self.rt_ptr().gc_heap_mut().bind_value(self_node, &val);
                                Some(Ok(true))
                            }
                            std::cmp::Ordering::Equal => {
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

                                Some(self.add_fast_array_element(
                                    ctx,
                                    val,
                                    set_flags(throw, no_add),
                                ))
                            }
                            std::cmp::Ordering::Greater => None,
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            }

            JS_CLASS_ARGUMENTS => {
                if self.is_fast_array()
                    && let Some(arr_data) = self.payload_array_mut()
                {
                    if index < arr_data.len() {
                        arr_data.set_nth(index, val);
                        let self_node = self.gc_head_ptr();
                        self.rt_ptr().gc_heap_mut().bind_value(self_node, &val);
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
                let Ok(v) = val.to_number(ctx).map(|n| n.to_u8_clamped()) else {
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
            JS_CLASS_FLOAT16_ARRAY | JS_CLASS_FLOAT32_ARRAY | JS_CLASS_FLOAT64_ARRAY => {
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
                #[cfg(feature = "big_int")]
                {
                    let Ok(bi) = crate::builtins::bigint::to_big_int(ctx, val) else {
                        return Some(Err(-1));
                    };
                    let bi = unsafe { bi.as_bigint_unchecked() };
                    let bits = if self.class_id() == JS_CLASS_BIG_INT64_ARRAY {
                        crate::bigint::to_bigint64(&bi) as u64
                    } else {
                        crate::bigint::to_biguint64(&bi)
                    };
                    let _ = Object::from_ref(self)
                        .as_TypedArray_mut()
                        .unwrap()
                        .set_element_u64(index as _, bits);
                    Some(Ok(true))
                }
                #[cfg(not(feature = "big_int"))]
                {
                    Some(Err(ctx.throw_type_error(
                        "BigInt64Array, BigUint64Array is not supported",
                    )))
                }
            }

            _ => None,
        }
    }

    /// Index-keyed `[[Set]]` convenience. See `set_property` for attribute/throw rules.
    pub fn set_property_u32(
        &mut self,
        ctx: &mut ScopedContext,
        idx: u32,
        val: JSValue,
    ) -> Result<bool, i32> {
        self.set_property_u32_internal(ctx, idx as usize, val, ThrowPolicy::Always, false)
            .unwrap_or_else(|| {
                let atom = Atom::new_i64(ctx, idx as i64)?;
                self.set_property_internal(
                    ctx,
                    atom,
                    val,
                    Some(Object::from_ref(self)),
                    ThrowPolicy::Always,
                    false,
                )
            })
    }

    /// Signed index `[[Set]]` wrapper (ObjectImpl). See `set_property` for rules.
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
            let atom = Atom::new_i64(ctx, idx)?;
            self.set_property(ctx, atom, val)
        }
    }

    /// String-keyed `[[Set]]` wrapper (ObjectImpl). See `set_property` for rules.
    #[inline]
    pub fn set_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
        val: JSValue,
    ) -> Result<bool, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.set_property(ctx, atom, val)
    }
}

impl JSValue {
    /// Core `[[Set]]` with explicit receiver.
    /// `no_add = true` means strict SetMutableBinding (no new property).
    /// `receiver = None` means do not add (same as `Some(obj)` where `obj` is
    /// a fresh prototype-less object, but the common case is set-property on
    /// `self` so we short-circuit).
    pub(crate) fn set_property_internal(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: Self,
        receiver: Option<Object>,
        throw: ThrowPolicy,
        no_add: bool,
    ) -> Result<bool, i32> {
        if let Some(o) = self.as_object() {
            o.set_property_internal(ctx, prop, val, receiver.or(Some(o)), throw, no_add)
        } else if self.is_undefined_or_null() {
            Err(ctx.throw_type_error(if self.is_undefined() {
                "cannot set property of undefined"
            } else {
                "cannot set property of null"
            }))
        } else {
            if let Some(p) = self.get_prototype_primitive(ctx).as_object() {
                p.set_property_internal(ctx, prop, val, receiver, throw, no_add)
            } else if unlikely(throw.can_throw(ctx.rt().is_strict_mode())) {
                Err(ctx.throw_type_error_not_an_object())
            } else {
                Ok(false)
            }
        }
    }

    /// Engine-internal value-keyed set (see `ObjectImpl::set_property_value`).
    #[inline]
    pub(crate) fn set_property_value(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Self,
        val: Self,
        throw: ThrowPolicy,
        no_add: bool,
    ) -> Result<bool, i32> {
        if let Some(mut o) = self.as_object() {
            o.set_property_value(ctx, prop, val, throw, no_add)
        } else {
            let atom = prop.to_atom(ctx)?;
            self.set_property_internal(ctx, atom, val, None, throw, no_add)
        }
    }

    /// Generic `[[Set]]` (receiver = `self`). New properties are created as
    /// {writable, enumerable, configurable} per `CreateDataProperty` semantics.
    /// Engine-internal `Set(..., true)`: failure always throws.
    #[inline(always)]
    pub fn set_property(
        &self,
        ctx: &mut ScopedContext,
        prop: Atom,
        val: Self,
    ) -> Result<bool, i32> {
        self.set_property_internal(ctx, prop, val, None, ThrowPolicy::Always, false)
    }

    /// Index-keyed `[[Set]]` wrapper. See `set_property` for attribute/throw rules.
    #[inline]
    pub fn set_property_u32(
        &mut self,
        ctx: &mut ScopedContext,
        idx: u32,
        val: Self,
    ) -> Result<bool, i32> {
        if let Some(mut ob) = self.as_object() {
            ob.set_property_u32(ctx, idx, val)
        } else {
            self.set_property_value(ctx, idx.into(), val, ThrowPolicy::Always, false)
        }
    }

    /// Signed index `[[Set]]` wrapper (JSValue). See `set_property` for rules.
    #[inline]
    pub fn set_property_i64(
        &mut self,
        ctx: &mut ScopedContext,
        idx: i64,
        val: Self,
    ) -> Result<bool, i32> {
        if let Some(mut ob) = self.as_object() {
            ob.set_property_i64(ctx, idx, val)
        } else {
            self.set_property_value(ctx, JSValue::from(idx), val, ThrowPolicy::Always, false)
        }
    }

    /// String-keyed `[[Set]]` wrapper (JSValue). See `set_property` for rules.
    #[inline]
    pub fn set_property_str(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
        val: Self,
    ) -> Result<bool, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.set_property(ctx, atom, val)
    }
}

#[inline(always)]
fn call_setter(
    ctx: &mut ScopedContext,
    setter: Option<Object>,
    this_obj: JSValue,
    value: JSValue,
    throw: ThrowPolicy,
) -> Result<bool, i32> {
    if let Some(func) = setter {
        func.call(ctx, this_obj, std::slice::from_ref(&value))?;
        Ok(true)
    } else if unlikely(throw.can_throw(ctx.rt().is_strict_mode())) {
        Err(ctx.throw_type_error("no setter for property"))
    } else {
        Ok(false)
    }
}
