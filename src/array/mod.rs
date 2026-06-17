// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::TryReserveError;

use fallible_collections::{FallibleVec, TryCollect};
use gc_lite::{GcNode, GcRef, GcTrace, GcTraceCtx};
use smallvec::SmallVec;

use crate::{
    IntOrFloat, JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY,
    atom::Atom,
    context::ScopedContext,
    number::MAX_SAFE_INTEGER,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LENGTH,
        JS_PROP_THROW, JS_PROP_WRITABLE, JSProperty, Object, ObjectImpl, ObjectPayload,
        PropertyKeyKind,
    },
    object_cast,
    string::StringData,
    utils::unlikely,
    value::JSValue,
};

#[repr(transparent)]
#[derive(Debug, Default)]
pub struct ArrayPayload {
    values: Vec<JSValue>,
}

impl GcTrace for ArrayPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        for v in self.values.iter() {
            v.trace(tr);
        }
    }
}

impl ArrayPayload {
    pub const fn new(values: Vec<JSValue>) -> Self {
        Self { values }
    }

    pub const fn len(&self) -> usize {
        self.values.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn reserve(&mut self, extra: usize) -> Result<(), TryReserveError> {
        self.values.try_reserve(extra)
    }

    #[inline(always)]
    pub fn values(&self) -> &[JSValue] {
        &self.values
    }

    #[inline(always)]
    pub fn reverse(&mut self) {
        self.values.reverse();
    }

    #[inline(always)]
    pub(crate) fn push(&mut self, val: JSValue) -> Result<(), TryReserveError> {
        self.values.try_push(val)
    }

    #[inline(always)]
    pub(crate) fn pop(&mut self) -> Option<JSValue> {
        self.values.pop()
    }

    #[inline(always)]
    pub(crate) fn extend(&mut self, vals: impl Iterator<Item = JSValue>) {
        self.values.extend(vals);
    }

    #[inline(always)]
    pub(crate) fn remove(&mut self, index: usize) -> JSValue {
        self.values.remove(index)
    }

    /// # Note
    /// handle array and value write-barrier or bind relation by yourself.
    #[inline(always)]
    pub(crate) fn set_nth(&mut self, index: usize, value: JSValue) {
        self.values[index] = value;
    }

    #[inline(always)]
    pub(crate) fn replace_values(&mut self, values: Vec<JSValue>) {
        self.values = values;
    }
}

/// Object representing an Array, Arguments instance.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct Array(GcRef<ObjectImpl>);

object_cast!(Array, JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS);

impl Array {
    #[inline(always)]
    pub fn is_fast_array(&self) -> bool {
        self.flags().is_fast_array()
    }

    #[inline(always)]
    pub fn payload(&self) -> &ArrayPayload {
        unsafe { self.0.as_ref().payload() }.array().unwrap()
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut ArrayPayload {
        let obj: &mut Object = self;
        obj.payload_mut().array_mut().unwrap()
    }

    /// get `length` property value
    pub fn get_length_prop(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        if let JSProperty::Normal(v) = &self.prop_slot(0) {
            v.to_length(ctx)
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            unsafe {
                std::hint::unreachable_unchecked();
            }
        }
    }

    /// Update `length` property of an array-like object.
    ///
    /// Returns:
    /// - true if length is updated,
    /// - false if can't update length property and JS_THROW flag is not set,
    /// - Err if can't update length property ahd JS_THOW flag is set.
    pub fn set_length(
        &mut self,
        ctx: &mut ScopedContext,
        length: u32,
        throw: bool,
    ) -> Result<bool, i32> {
        #[cfg(debug_assertions)]
        debug_assert!(
            self.shape().unwrap().prop_defs.first().unwrap().is_length(),
            "missing length property at index 0"
        );

        if !self
            .shape()
            .unwrap()
            .prop_defs
            .first()
            .unwrap() // <-- first prop is length, must exist.
            .is_writable()
        {
            return if throw {
                ctx.throw_type_error_read_only(Atom::CONST_length);
                Err(-1)
            } else {
                Ok(false)
            };
        }

        if self.is_fast_array() {
            // fast array
            let arr_data = self.as_Array_mut().unwrap().payload_mut();
            let old_len = arr_data.len() as u32;
            if length < old_len {
                arr_data.values.drain(length as usize..old_len as usize);
            }

            self.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(length)));
        } else {
            // sparse array
            // Note: prop `length` must be less than u32::MAX
            let mut cur_len = self.get_length_prop(ctx)? as u32;

            if length < cur_len {
                // delete index property
                let mut sh = self.shape().unwrap();
                let d = cur_len - length;
                let propdef_len = sh.len() as u32;

                if d <= propdef_len {
                    // faster to iterate
                    while cur_len > length {
                        let atom = Atom::from_i64(ctx, cur_len as i64 - 1)?;
                        let ret = self.delete_property_internal(ctx, atom, false);
                        if let Ok(false) = ret {
                            break; // property is not configurable
                        }
                        cur_len -= 1;
                    }
                } else {
                    // faster to iterate thru all the properties.
                    // Need two passes in case one of the property is not  configurable
                    cur_len = length;
                    for (_, prs) in sh.prop_defs.iter() {
                        if let Some(i) = prs.atom.to_array_index(ctx.rt())
                            && i >= cur_len
                            && (prs.flags() & JS_PROP_CONFIGURABLE == 0)
                        {
                            cur_len = i + 1;
                        }
                    }

                    // Hint: to avoid index conflict, remove from tail to head
                    for i in (0..propdef_len).rev() {
                        if let Some(prop) = sh.prop_defs.get(i as usize).map(|p| p.atom) {
                            // non-deleted prop
                            if let Some(idx) = prop.to_array_index(ctx.rt())
                                && idx >= cur_len
                            {
                                let _r = self.delete_property_internal(ctx, prop, false);
                                debug_assert!(_r.is_ok());

                                // WARNING: the shape may have been modified
                                sh = self.shape().unwrap();
                            }
                        }
                    }
                }
            } else {
                cur_len = length;
            }

            self.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(cur_len)));

            if cur_len > length {
                return if throw {
                    ctx.throw_type_error("not configurable");
                    Err(-1)
                } else {
                    Ok(false)
                };
            }
        }

        Ok(true)
    }

    /// set values to Array and enforce it to be fast array.
    pub(crate) fn set_fast_array_values(
        &mut self,
        ctx: &mut ScopedContext,
        values: Vec<JSValue>,
    ) -> Result<(), i32> {
        debug_assert!(
            self.shape().unwrap().prop_defs.first().unwrap().flags() & JS_PROP_LENGTH != 0,
            "Array.length property must be index 0"
        );

        if !self.is_fast_array() {
            // if `self` is sparse array, delete all index properties
            let mut del = SmallVec::<[u32; 64]>::new();
            self.own_property_names_foreach(ctx, JS_GPN_STR_OR_INT_MASK | JS_GPN_ENUM_ONLY, |_, p| {
                if let PropertyKeyKind::ArrayIndex(i) = p.kind {
                    del.push(i);
                }
                Ok(())
            })?;

            for i in del {
                self.delete_property_i64(ctx, i as _, JS_PROP_THROW)?;
            }
        }

        let len = values.len();

        // bind reference relation for all gc values
        let self_node = self.gc_node_ptr();
        let mut rt = self.rt_ptr();
        for v in values.iter() {
            if let Some(n) = v.gc_node_ptr() {
                rt.gc_heap_mut().bind(self_node, n);
            }
        }

        self.payload_mut().replace_values(values);
        self.set_flag_fast_array(true);

        // set array length, can't fail
        self.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(len as u32)));

        Ok(())
    }

    /// create an empty Array
    #[inline(always)]
    pub fn create(ctx: &mut ScopedContext) -> Result<Self, i32> {
        Self::from_values(ctx, Vec::new())
    }

    /// create Array with element values
    #[inline(always)]
    pub fn from_values(ctx: &mut ScopedContext, values: Vec<JSValue>) -> Result<Self, i32> {
        let len = values.len();

        let mut arr = ObjectImpl::create(
            ctx,
            ctx.array_shape.clone(),
            ObjectPayload::ARRAY(ArrayPayload::new(values)),
        )
        .map(|o| Array(o.0))
        .map_err(|_| -1)?;

        debug_assert!(arr.flags().is_exotic());
        debug_assert!(arr.flags().is_fast_array());

        if len > 0 {
            arr.set_length(ctx, len as u32, true)?;
        }

        Ok(arr)
    }

    /// create Array from a JSValue slice
    #[inline(always)]
    pub fn from_slice(ctx: &mut ScopedContext, values: &[JSValue]) -> Result<Self, i32> {
        if let Ok(lst) = values.iter().cloned().try_collect::<Vec<_>>()
            && let Ok(arr) = Self::from_values(ctx, lst)
        {
            Ok(arr)
        } else {
            ctx.throw_out_of_memory();
            Err(-1)
        }
    }

    /// create Array object with pre-allocated capacity.
    /// `cap` is allowed no greater than i32::MAX
    #[inline(always)]
    pub fn with_capacity(ctx: &mut ScopedContext, cap: usize) -> Result<Self, i32> {
        if cap <= i32::MAX as usize {
            let lst = alloc_fast_array_slots(ctx, cap)?;
            let mut fa = Self::from_values(ctx, lst)?;
            if cap > 0 {
                fa.set_length(ctx, cap as u32, true)?;
            }
            Ok(fa)
        } else {
            ctx.throw_range_error("invalid array length");
            Err(-1)
        }
    }

    /// sort array-like object `obj`.
    pub(crate) fn sort(
        ctx: &mut ScopedContext,
        obj: &mut Object,
        cmp: Option<JSValue>,
    ) -> Result<(), i32> {
        if let Some(f) = cmp {
            f.ensure_callable(ctx)?;
        }

        let arr_len = obj.get_length_prop(ctx)?;
        if unlikely(arr_len <= 1) {
            return Ok(());
        }

        // fast path
        if obj.is_fast_array()
            && let Some(arr_data) = obj.as_Array_mut().map(|a| a.payload_mut())
        {
            let length = arr_data.len();
            debug_assert!(length <= arr_len as usize);

            /// Rebuild internal storage with sorted elements, padding with `JSValue::UNDEFINED` to `length`.
            /// No write barrier needed: all elements come from the array itself,
            /// their reference relations were already established on initial insertion.
            fn rebuild(
                lst: &mut Vec<JSValue>,
                sorted: impl Iterator<Item = JSValue>,
                length: usize,
            ) {
                lst.clear();
                lst.extend(
                    sorted
                        .chain(std::iter::repeat(JSValue::UNDEFINED))
                        .take(length),
                );
            }

            if let Some(cmp) = cmp {
                let mut slots = arr_data
                    .values()
                    .iter()
                    .filter(|v| !v.is_undefined())
                    .copied()
                    .try_collect::<Vec<JSValue>>()
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;

                sort_values_by(ctx, &mut slots, cmp)?;
                rebuild(&mut arr_data.values, slots.into_iter(), length);
            } else {
                let mut slots = Vec::<(JSValue, StringData)>::new();
                slots.try_reserve_exact(length).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;

                for v in arr_data.values().iter().filter(|v| !v.is_undefined()) {
                    slots.push((*v, v.to_string(ctx)?.content().clone()));
                }

                sort_values_default(&mut slots);
                rebuild(
                    &mut arr_data.values,
                    slots.into_iter().map(|(v, _)| v),
                    length,
                );
            }

            return Ok(()); // early exit for fast array
        }

        //
        // slow path: `obj` is sparse array.
        //
        let mut undefined_count = 0;
        let mut first_missing: Option<usize> = None;

        if let Some(cmp) = cmp {
            let mut slots = alloc_fast_array_slots(ctx, arr_len as _)?;

            for i in 0..arr_len {
                if let Some(v) = obj.try_get_property_i64(ctx, i)? {
                    if v.is_undefined() {
                        undefined_count += 1;
                    } else {
                        slots.push(v);
                    }
                } else if first_missing.is_none() {
                    first_missing.replace(i as usize);
                }
            }

            sort_values_by(ctx, slots.as_mut_slice(), cmp)?;

            let slots_len = slots.len() as i64;
            for (i, v) in slots.into_iter().enumerate() {
                obj.set_property_u32(ctx, i as u32, v)?;
            }
            if undefined_count > 0 {
                for i in slots_len..(slots_len + undefined_count) {
                    obj.set_property_u32(ctx, i as u32, JSValue::UNDEFINED)?;
                }
            }
            for i in (slots_len + undefined_count)..arr_len {
                obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
            }
        } else {
            let mut slots = Vec::<(JSValue, StringData)>::with_capacity(16);
            slots.try_reserve_exact(arr_len as usize).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

            for i in 0..arr_len {
                if let Some(v) = obj.try_get_property_i64(ctx, i)? {
                    if v.is_undefined() {
                        undefined_count += 1;
                    } else {
                        let s = v.to_string(ctx)?;
                        slots.push((v, s.content().clone()));
                    }
                } else if first_missing.is_none() {
                    first_missing.replace(i as usize);
                }
            }

            sort_values_default(slots.as_mut_slice());

            let slots_len = slots.len() as i64;
            for (i, (v, _)) in slots.into_iter().enumerate() {
                obj.set_property_u32(ctx, i as u32, v)?;
            }
            if undefined_count > 0 {
                for i in slots_len..(slots_len + undefined_count) {
                    obj.set_property_u32(ctx, i as u32, JSValue::UNDEFINED)?;
                }
            }
            for i in (slots_len + undefined_count)..arr_len {
                obj.delete_property_i64(ctx, i, JS_PROP_THROW)?;
            }
        }

        Ok(())
    }
}

fn sort_values_default(values: &mut [(JSValue, StringData)]) {
    values.sort_by(|a, b| {
        // compare string content
        a.1.cmp(&b.1)
    });
}

fn sort_values_by(
    ctx: &mut ScopedContext,
    values: &mut [JSValue],
    cmpfn: JSValue,
) -> Result<(), i32> {
    let mut has_err = false;

    values.sort_by(|a, b| {
        if !has_err {
            match cmpfn.call(ctx, JSValue::UNDEFINED, &[*a, *b]) {
                Ok(r) => match r {
                    JSValue::Int(n) => {
                        if n == 0 {
                            std::cmp::Ordering::Equal
                        } else if n > 0 {
                            std::cmp::Ordering::Greater
                        } else {
                            std::cmp::Ordering::Less
                        }
                    }
                    r => {
                        if let Ok(f) = r.to_number(ctx).map(|n| n.to_f64()) {
                            if f == 0.0 {
                                std::cmp::Ordering::Equal
                            } else if f > 0.0 {
                                std::cmp::Ordering::Greater
                            } else {
                                std::cmp::Ordering::Less
                            }
                        } else {
                            has_err = true;
                            std::cmp::Ordering::Equal
                        }
                    }
                },
                Err(_) => {
                    has_err = true;
                    std::cmp::Ordering::Equal
                }
            }
        } else {
            std::cmp::Ordering::Equal
        }
    });

    if has_err { Err(-1) } else { Ok(()) }
}

impl ObjectImpl {
    /// return Err if exception (proxy case) or TRUE/FALSE
    pub fn is_array(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        let class_id = self.class_id();

        #[cfg(feature = "builtin_proxy")]
        if class_id == crate::JS_CLASS_PROXY {
            unsafe { Object::from_ref(self).as_Proxy_unchecked().is_array(ctx) }
        } else {
            Ok(class_id == JS_CLASS_ARRAY)
        }

        #[cfg(not(feature = "builtin_proxy"))]
        Ok(class_id == JS_CLASS_ARRAY)
    }

    /// if `self` is a fast array, convert it to sparse array,
    /// valid elements (non UNINITIALIZED) will be added as property with integral name.
    ///
    /// Note: for Array or Arguments object only.
    pub(crate) fn destruct_fast_array(&mut self, ctx: &mut ScopedContext) -> Result<(), i32> {
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS
        ));

        if self.is_fast_array() {
            // take array list out
            let items = std::mem::take(&mut self.payload_mut().array_mut().unwrap().values);
            let mut arr_obj = Object::from_ref(self);

            for (i, v) in items.into_iter().enumerate() {
                // add_property cannot fail here
                // but __JS_AtomFromUInt32(i) fails for i > i32::MAX
                //
                // Note: special JSValue::UNINITIALIZED is used to indicate not adding this as property
                if !v.is_uninitialized() {
                    arr_obj.add_own_property(
                        ctx,
                        Atom::new_small_int(i as _),
                        JS_PROP_C_W_E,
                        JSProperty::Normal(v),
                    )?;
                }
            }

            self.set_flag_fast_array(false);
        }

        Ok(())
    }

    /// Preconditions: 'self' is JS_CLASS_ARRAY, is fast_array and is extensible.
    /// returns Err if fail, true if append successfully, false if can't append due to read-only but flags indicates don't throw exception.
    pub(crate) fn add_fast_array_element(
        &mut self,
        ctx: &mut ScopedContext,
        value: JSValue,
        flags: u32,
    ) -> Result<bool, i32> {
        debug_assert!(self.is_fast_array());
        debug_assert!(matches!(
            self.class_id(),
            JS_CLASS_ARRAY | JS_CLASS_ARGUMENTS
        ));
        // debug_assert_eq!(self.class_id(), JS_CLASS_ARRAY); // should we prevent class ARGUMENTS?

        // extend the array by one
        // XXX: convert to slow array if new_len > u32::MAX elements
        let new_len = self.payload().array().unwrap().len() + 1;

        // first element in `prop_values` is the array length.
        // update the length if necessary. We assume that if the length is
        // not an integer, then if it >= 2^31.
        // Note: check array length to int in advance, to avoid prop_values borrow conflict.
        let len: Option<i32> = if let JSProperty::Normal(JSValue::Int(n)) = self.prop_slot(0) {
            Some(*n)
        } else {
            #[cfg(debug_assertions)]
            unreachable!();
            #[cfg(not(debug_assertions))]
            None
        };

        if let Some(arr_len) = len
            && new_len > arr_len as usize
        {
            // grow list
            let r = self
                .shape()
                .and_then(|sh| sh.prop_defs.first().map(|p| p.flags()))
                .unwrap();

            if unlikely(r & JS_PROP_WRITABLE == 0) {
                return if ctx.rt().can_throw(flags) {
                    ctx.throw_type_error_read_only(Atom::CONST_length);
                    Err(-1)
                } else {
                    Ok(false)
                };
            } else {
                // update `length`
                self.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::Int(new_len as i32)));
            }
        }

        // add new element to list
        self.payload_mut()
            .array_mut()
            .unwrap()
            .push(value)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;

        // bind reference relation
        if let Some(n) = value.gc_node_ptr() {
            let self_node = self.gc_head_ptr();
            ctx.rt_ptr().gc_heap_mut().bind(self_node, n);
        }

        Ok(true)
    }

    /// Get fast array list length:
    /// - for Array or Argument instance, return elements count (not property `length`);
    /// - for TypedArray or DataView instance, return elements count;
    /// - __Panic__ when object is not an Array, Argument, TypedArray or DataView.
    pub(crate) fn fast_array_count(&self) -> u32 {
        if let Some(ap) = self.payload().array() {
            // ARRAY or ARGUMENT
            ap.len() as u32
        } else if let Some(ta) = self.payload().typed_array() {
            // typed array or dataview
            ta.count() as _
        } else {
            unreachable!("expect argument, array, typed array or dataview")
        }
    }
}

impl JSValue {
    /// check if `self` is an Array object
    /// return Err if exception (proxy case) or TRUE/FALSE
    #[inline]
    pub fn is_array(&self, ctx: &mut ScopedContext) -> Result<bool, i32> {
        self.as_object().map_or(Ok(false), |o| o.is_array(ctx))
    }

    /// test if value is fast array object (could be one of Array, Argument, TypedArray fast array).
    #[inline]
    pub fn is_fast_array(&self) -> bool {
        self.as_object().is_some_and(|o| o.is_fast_array())
    }

    /// coerce value to array length, which is in range \[0, 2^32-1\].
    pub fn to_array_length(
        &self,
        ctx: &mut ScopedContext,
        is_array_ctor: bool,
    ) -> Result<u32, i32> {
        let len: u32;

        match self {
            Self::NULL => {
                len = 0;
            }
            Self::Int(v) => {
                let n = *v;
                if n < 0 {
                    ctx.throw_range_error("invalid array length");
                    return Err(-1);
                }
                len = n as u32;
            }
            Self::Bool(b) => {
                len = *b as u32;
            }
            Self::Float64(d) => {
                len = *d as u32;
                if len as f64 != *d {
                    // f64 can't convert to u32
                    ctx.throw_range_error("invalid array length");
                    return Err(-1);
                }
            }
            _ => {
                if is_array_ctor {
                    match self.to_number(ctx)? {
                        IntOrFloat::Int64(n) => {
                            if n < 0 {
                                ctx.throw_range_error("invalid array length");
                                return Err(-1);
                            }
                            len = n as u32;
                        }
                        IntOrFloat::Float64(d) => {
                            len = d as u32;
                            if len as f64 != d {
                                // f64 can't convert to u32
                                ctx.throw_range_error("invalid array length");
                                return Err(-1);
                            }
                        }
                    }
                } else {
                    // 1) ECMA legacy behavior: must do ::to_number() conversion twice and compare
                    len = self.to_number(ctx)?.to_u32();
                    if len != JSValue::from(self.to_number(ctx)?).to_array_length(ctx, false)? {
                        ctx.throw_range_error("invalid array length");
                        return Err(-1);
                    }
                    // 2) non-ECMA compatible, but cleaner
                    // len = self.to_number(ctx)?.to_u32();
                }
            }
        }

        Ok(len)
    }

    /// coerce value to array index, which is in range \[0, MAX_SAFE_INTEGER\].
    pub fn to_index(&self, ctx: &mut ScopedContext) -> Result<i64, i32> {
        let i = self.to_number(ctx)?.to_i64_saturating();
        if (0..=MAX_SAFE_INTEGER).contains(&i) {
            Ok(i)
        } else {
            ctx.throw_range_error("invalid array index");
            Err(-1)
        }
    }
}

pub(crate) fn alloc_fast_array_slots(
    ctx: &mut ScopedContext,
    cap: usize,
) -> Result<Vec<JSValue>, i32> {
    if cap > i32::MAX as usize {
        ctx.throw_range_error("invalid array length");
        Err(-1)
    } else {
        let mut lst = Vec::<JSValue>::new();
        if cap > 0 {
            lst.try_reserve_exact(std::cmp::max(cap, 0) as usize)
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
        }
        Ok(lst)
    }
}
