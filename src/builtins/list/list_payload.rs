// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::TryReserveError;

use fallible_collections::FallibleVec;
use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    JS_CLASS_LIST,
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    object::{JSProperty, Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

/// Trait for converting a JSValue to a Rust type.
pub trait FromJSValue {
    fn from_value(ctx: &mut ScopedContext, val: JSValue) -> Result<Self, i32>
    where
        Self: std::marker::Sized;
}

/// Trait for converting a Rust type to a JSValue.
pub trait ToJSValue {
    fn to_value(&self, ctx: &mut ScopedContext) -> JSResult;
}

/// Trait for elements that can be stored in a List.
pub trait ListElement: FromJSValue + ToJSValue + std::fmt::Debug + 'static {}

/// Payload for List objects: a dense vector of typed elements without holes.
#[derive(Debug)]
pub struct ListPayload {
    values: Vec<Box<dyn ListElement>>,
}

impl Default for ListPayload {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl GcTrace for ListPayload {
    fn trace(&self, tr: &mut GcTraceCtx) {
        for v in self.values.iter() {
            v.trace(tr);
        }
    }
}

impl ListPayload {
    pub fn new(values: Vec<Box<dyn ListElement>>) -> Self {
        Self { values }
    }

    pub const fn len(&self) -> usize {
        self.values.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    pub fn values(&self) -> &[Box<dyn ListElement>] {
        &self.values
    }

    #[inline(always)]
    pub fn values_mut(&mut self) -> &mut [Box<dyn ListElement>] {
        &mut self.values
    }

    #[inline(always)]
    pub fn reverse(&mut self) {
        self.values.reverse();
    }

    #[inline(always)]
    pub fn push(&mut self, val: Box<dyn ListElement>) -> Result<(), TryReserveError> {
        self.values.try_push(val)
    }

    #[inline(always)]
    pub fn pop(&mut self) -> Option<Box<dyn ListElement>> {
        self.values.pop()
    }

    #[inline(always)]
    pub fn remove(&mut self, index: usize) -> Box<dyn ListElement> {
        self.values.remove(index)
    }

    #[inline(always)]
    pub fn set_nth(&mut self, index: usize, value: Box<dyn ListElement>) {
        self.values[index] = value;
    }

    pub fn replace_values(&mut self, values: Vec<Box<dyn ListElement>>) {
        self.values = values;
    }

    pub fn extend(&mut self, vals: impl Iterator<Item = Box<dyn ListElement>>) {
        self.values.extend(vals);
    }
}

/// Object representing a List instance.
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct List(GcRef<ObjectImpl>);

object_cast!(List, JS_CLASS_LIST);

impl List {
    #[inline(always)]
    pub fn is_fast_array(&self) -> bool {
        self.flags().is_fast_array()
    }

    #[inline(always)]
    pub fn payload(&self) -> &ListPayload {
        unsafe { self.0.as_ref().payload() }.list().unwrap()
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut ListPayload {
        self.deref_mut().payload_mut().list_mut().unwrap()
    }

    /// Get `length` property value.
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

    /// Update `length` property of a List object.
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
            .unwrap()
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
            let list_data = self.payload_mut();
            let old_len = list_data.len() as u32;
            if length < old_len {
                list_data.values.drain(length as usize..old_len as usize);
            }
            self.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(length)));
        } else {
            // List should always be fast array; fall back to generic length update.
            self.set_prop_slot_non_gc(0, JSProperty::Normal(JSValue::from(length)));
        }

        Ok(true)
    }

    /// Create an empty List.
    #[inline(always)]
    pub fn create(ctx: &mut ScopedContext) -> Result<Self, i32> {
        Self::from_values(ctx, Vec::new())
    }

    /// Create List with element values.
    #[inline(always)]
    pub fn from_values(
        ctx: &mut ScopedContext,
        values: Vec<Box<dyn ListElement>>,
    ) -> Result<Self, i32> {
        let len = values.len();

        let mut list = ObjectImpl::create(
            ctx,
            ctx.list_shape.clone(),
            ObjectPayload::LIST(ListPayload::new(values)),
        )
        .map(|o| List(o.0))
        .map_err(|_| -1)?;

        debug_assert!(list.flags().is_exotic());
        debug_assert!(list.flags().is_fast_array());

        if len > 0 {
            list.set_length(ctx, len as u32, true)?;
        }

        Ok(list)
    }

    /// Create List object with pre-allocated capacity.
    #[inline(always)]
    pub fn with_capacity(ctx: &mut ScopedContext, cap: usize) -> Result<Self, i32> {
        if cap <= i32::MAX as usize {
            let mut lst = Vec::new();
            if cap > 0 {
                lst.try_reserve_exact(cap).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            let mut list = Self::from_values(ctx, lst)?;
            if cap > 0 {
                list.set_length(ctx, cap as u32, true)?;
            }
            Ok(list)
        } else {
            ctx.throw_range_error("invalid list length");
            Err(-1)
        }
    }
}
