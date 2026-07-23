// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::Cell, ptr::NonNull};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    RuntimeImpl, atom::Atom, context::ScopedContext, gcref_wrapper, object::JSProperty,
    object::Object, value::JSValue,
};

#[repr(u8)]
#[derive(Debug)]
pub enum VarRefImpl {
    /// A detached copy of a variable value. Used when a closure captures a
    /// local variable and the parent frame is taken (async suspension,
    /// generator yield, exception unwinding).
    Detached(JSValue),

    /// Reference to a local variable slot in an arena stack frame.
    ///
    /// `pvalue` is a raw pointer into the arena frame's data buffer:
    ///   - For arguments: arg_base_ptr[var_idx]
    ///   - For locals:   var_base_ptr[var_idx]
    Bind {
        is_arg: bool,
        var_idx: u16,
        pvalue: NonNull<JSValue>,
    },

    /// Reference to a property slot in a global scope object, with inline-cache
    /// self-healing via ABA-safe `(shape_ptr, shape_uid)` pair (same pattern as
    /// `GET_FIELD_IC`'s `InlineCacheEntry`).
    ///
    /// Fast path (shape unchanged, >99.9%): `obj.prop_slot(slot_idx)` → O(1)
    /// Slow path (Shape changed): `obj.find_shape_property(name)` → re-cache → O(n)
    PropValue {
        obj: Object,
        name: Atom,
        shape_ptr: Cell<*const ()>,
        shape_uid: Cell<u32>,
        slot_idx: Cell<u16>,
    },
}

impl GcTrace for VarRefImpl {
    fn trace(&self, gcx: &mut GcTraceCtx) {
        match self {
            Self::Bind { pvalue, .. } => unsafe {
                pvalue.as_ref().trace(gcx);
            },
            Self::Detached(value) => {
                value.trace(gcx);
            }
            Self::PropValue { obj, name, .. } => {
                obj.trace(gcx);
                name.trace(gcx);
            }
        }
    }
}

impl VarRefImpl {
    fn update_pvalue_for_heap_buf_offsets_impl(
        &mut self,
        heap_buf: &[JSValue],
        arg_base_offset: u32,
        var_base_offset: u32,
    ) {
        if let Self::Bind {
            pvalue,
            is_arg,
            var_idx,
            ..
        } = self
        {
            let off = if *is_arg {
                arg_base_offset as usize + *var_idx as usize
            } else {
                var_base_offset as usize + *var_idx as usize
            };
            *pvalue = NonNull::from(&heap_buf[off]);

            #[cfg(debug_assertions)]
            {
                let ptr: *const JSValue = pvalue.as_ptr() as *const JSValue;
                let buf_start: *const JSValue = heap_buf.as_ptr();
                let buf_end = unsafe { buf_start.add(heap_buf.len()) };
                debug_assert!(
                    ptr >= buf_start && ptr < buf_end,
                    "VarRef::Bind update pvalue {:p} out of heap_buf [{:p}, {:p}) off={}",
                    ptr,
                    buf_start,
                    buf_end,
                    off
                );
            }
        }
    }

    #[inline(always)]
    pub(crate) fn value(&self) -> JSValue {
        match self {
            Self::Bind { pvalue, .. } => unsafe { *pvalue.as_ref() },
            Self::Detached(value) => *value,
            Self::PropValue {
                obj,
                name,
                shape_ptr,
                shape_uid,
                slot_idx,
            } => {
                if let Some(sh) = obj.shape() {
                    let current_ptr: *const () = (&**sh) as *const _ as *const ();
                    if current_ptr == shape_ptr.get() && sh.uid == shape_uid.get() {
                        let idx = slot_idx.get() as usize;
                        if let JSProperty::Normal(v) = obj.prop_slot(idx) {
                            return *v;
                        }
                    }
                    if let Some((idx, _)) = obj.find_shape_property(*name) {
                        shape_ptr.set(current_ptr);
                        shape_uid.set(sh.uid);
                        slot_idx.set(idx as u16);
                        if let JSProperty::Normal(v) = obj.prop_slot(idx) {
                            return *v;
                        }
                    }
                }
                JSValue::UNDEFINED
            }
        }
    }

    fn set_value_internal(&mut self, new_value: JSValue) {
        match self {
            Self::Bind { pvalue, .. } => unsafe { *pvalue.as_mut() = new_value },
            Self::Detached(value) => *value = new_value,
            Self::PropValue {
                obj,
                name,
                shape_ptr,
                shape_uid,
                slot_idx,
            } => {
                if let Some(sh) = obj.shape() {
                    let current_ptr: *const () = (&**sh) as *const _ as *const ();
                    if current_ptr == shape_ptr.get() && sh.uid == shape_uid.get() {
                        obj.set_prop_slot(slot_idx.get() as usize, JSProperty::Normal(new_value));
                        return;
                    }
                    if let Some((idx, _)) = obj.find_shape_property(*name) {
                        shape_ptr.set(current_ptr);
                        shape_uid.set(sh.uid);
                        slot_idx.set(idx as u16);
                        obj.set_prop_slot(idx, JSProperty::Normal(new_value));
                    }
                }
            }
        }
    }

    pub(crate) const fn detach(&mut self) {
        if let Self::Bind { pvalue, .. } = self {
            *self = Self::Detached(unsafe { *pvalue.as_ref() });
        }
    }

    pub(crate) const fn is_match(&self, var_idx: u16, is_arg: bool) -> bool {
        if let Self::Bind {
            is_arg: a,
            var_idx: i,
            ..
        } = self
        {
            *i == var_idx && *a == is_arg
        } else {
            false
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct VarRef(GcRef<VarRefImpl>);
gcref_wrapper!(VarRef, VarRefImpl);

impl VarRef {
    pub(crate) fn update_pvalue_for_heap_buf_offsets(
        &mut self,
        heap_buf: &[JSValue],
        arg_base_offset: u32,
        var_base_offset: u32,
    ) {
        unsafe {
            VarRefImpl::update_pvalue_for_heap_buf_offsets_impl(
                self.0.as_mut(),
                heap_buf,
                arg_base_offset,
                var_base_offset,
            );
        }
    }

    pub(crate) fn set_value(&mut self, rt: &mut RuntimeImpl, new_value: JSValue) {
        self.set_value_internal(new_value);
        if let Some(n) = new_value.as_gc_node() {
            rt.gc_heap_mut().bind(self.gc_node_ptr(), n);
        }
    }

    pub(crate) fn create(ctx: &mut ScopedContext, value: JSValue) -> Result<Self, i32> {
        ctx.alloc_local(VarRefImpl::Detached(value))
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })
    }

    pub(crate) fn create_bind(
        ctx: &mut ScopedContext,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
    ) -> Result<Self, i32> {
        ctx.alloc_local(VarRefImpl::Bind {
            is_arg,
            var_idx,
            pvalue,
        })
        .map(Self)
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })
    }

    pub(crate) fn create_prop_value(
        ctx: &mut ScopedContext,
        obj: Object,
        name: Atom,
        shape_ptr: *const (),
        shape_uid: u32,
        slot_idx: u16,
    ) -> Result<Self, i32> {
        ctx.alloc_local(VarRefImpl::PropValue {
            obj,
            name,
            shape_ptr: Cell::new(shape_ptr),
            shape_uid: Cell::new(shape_uid),
            slot_idx: Cell::new(slot_idx),
        })
        .map(Self)
        .map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })
    }
}
