// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::UnsafeCell, ptr::NonNull};

use gc_lite::{GcHeap, GcRef, GcTrace, GcTraceCtx};

use crate::{RuntimeImpl, context::ScopedContext, gc::GcHeapExt, gcref_wrapper, value::JSValue};

#[repr(u8)]
#[derive(Debug)]
pub enum VarRefImpl {
    /// A detached copy of a variable value. Used when a closure captures a
    /// local variable and the parent frame is taken (async suspension,
    /// generator yield, exception unwinding).
    Detached(JSValue),

    /// Reference to a local variable slot in an arena stack frame.
    Bind {
        is_arg: bool,
        var_idx: u16,
        pvalue: NonNull<JSValue>,
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
        }
    }
}

impl VarRefImpl {
    fn update_pvalue_for_heap_buf_offsets_impl(
        &mut self,
        heap_buf: &[UnsafeCell<JSValue>],
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
            *pvalue = unsafe { NonNull::new_unchecked(heap_buf[off].get()) };

            #[cfg(debug_assertions)]
            {
                let ptr: *const JSValue = pvalue.as_ptr() as *const JSValue;
                let buf_start: *const JSValue = heap_buf.as_ptr() as *const JSValue;
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
    pub(crate) const fn value(&self) -> JSValue {
        match self {
            Self::Bind { pvalue, .. } => unsafe { *pvalue.as_ref() },
            Self::Detached(value) => *value,
        }
    }

    const fn set_value_internal(&mut self, new_value: JSValue) {
        match self {
            Self::Bind { pvalue, .. } => unsafe { *pvalue.as_mut() = new_value },
            Self::Detached(value) => *value = new_value,
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
        heap_buf: &[UnsafeCell<JSValue>],
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
        rt.bind_node_value(self.gc_node_ptr(), new_value);
    }

    /// Detach the frame binding and remember the captured value on the VarRef
    /// node. The frame slot was a GC root; once the frame is freed the only
    /// edge to the value is this (possibly old) node. The barrier is a no-op
    /// without `gc_generations`.
    pub(crate) fn detach_with_barrier(&mut self, heap: &mut GcHeap) {
        unsafe { self.0.as_mut() }.detach();
        #[cfg(feature = "gc_generations")]
        {
            let value = unsafe { self.0.as_ref() }.value();
            heap.bind_value(self.gc_node_ptr(), &value);
        }
        #[cfg(not(feature = "gc_generations"))]
        let _ = heap;
    }

    pub(crate) fn create(ctx: &mut ScopedContext, value: JSValue) -> Result<Self, i32> {
        ctx.alloc_local(VarRefImpl::Detached(value))
            .map(Self)
            .map_err(|_| ctx.throw_out_of_memory())
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
        .map_err(|_| ctx.throw_out_of_memory())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Rebinding a VarRef to a heap frame must derive the new pointer through
    /// `UnsafeCell::get`, so writes through the VarRef stay legal even though
    /// the buffer is only reached through shared references.
    #[test]
    fn pvalue_points_into_unsafe_cell_heap_buf() {
        let buf = [
            std::cell::UnsafeCell::new(JSValue::from(10)),
            std::cell::UnsafeCell::new(JSValue::from(20)),
        ];
        let mut vr = VarRefImpl::Bind {
            is_arg: false,
            var_idx: 1,
            pvalue: NonNull::dangling(),
        };

        vr.update_pvalue_for_heap_buf_offsets_impl(&buf, 0, 0);
        assert_eq!(vr.value(), JSValue::from(20));

        vr.set_value_internal(JSValue::from(30));
        assert_eq!(unsafe { *buf[1].get() }, JSValue::from(30));
    }
}
