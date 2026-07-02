// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    RuntimeImpl, context::ScopedContext, gcref_wrapper, stack_frame::StackFrame, value::JSValue,
};

#[repr(u8)]
#[derive(Debug)]
pub enum VarRefImpl {
    /// var ref is detached, point to nowhere.
    Detached(JSValue),
    /// reference to a var slot in stack frame
    Bind {
        is_arg: bool,
        var_idx: u16,
        /// Raw pointer to var value, equivelent to
        /// StackFrame::arg_base_ptr[var_idx] or StackFrame::var_base_ptr[var_idx],
        /// for speed access only
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
    /// Internal helper that updates pvalue for a heap buffer transfer.
    /// Defined on VarRefImpl (&mut self) to avoid Rust 2024 match ergonomics
    /// issues when matching through GcRef::as_mut() indirection.
    pub(crate) fn update_pvalue_for_heap_buf_impl(
        &mut self,
        heap_buf: &Box<[JSValue]>,
        head: &StackFrame,
    ) {
        Self::update_pvalue_for_heap_buf_offsets_impl(
            self,
            heap_buf,
            head.arg_base_offset,
            head.var_base_offset,
        )
    }

    /// Same as update_pvalue_for_heap_buf_impl but takes offsets directly
    /// to avoid borrow-checker conflicts with FrameHead field access.
    pub(crate) fn update_pvalue_for_heap_buf_offsets_impl(
        &mut self,
        heap_buf: &Box<[JSValue]>,
        arg_base_offset: u32,
        var_base_offset: u32,
    ) {
        match self {
            Self::Bind {
                pvalue,
                is_arg,
                var_idx,
                ..
            } => {
                let off = if *is_arg {
                    arg_base_offset as usize + *var_idx as usize
                } else {
                    var_base_offset as usize + *var_idx as usize
                };
                *pvalue = NonNull::from(&heap_buf[off]);
            }
            _ => {}
        }
    }

    /// get variable value
    #[inline(always)]
    pub(crate) const fn value(&self) -> JSValue {
        match self {
            Self::Bind { pvalue, .. } => unsafe { *pvalue.as_ref() },
            Self::Detached(value) => *value,
        }
    }

    /// update variable value
    const fn set_value_internal(&mut self, new_value: JSValue) {
        match self {
            Self::Bind { pvalue, .. } => unsafe {
                *pvalue.as_mut() = new_value;
            },
            Self::Detached(value) => {
                *value = new_value;
            }
        }
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
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

// TODO: how about VarRef not to be a gc node?
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct VarRef(GcRef<VarRefImpl>);

gcref_wrapper!(VarRef, VarRefImpl);

impl VarRef {
    /// Update the raw pointer (pvalue) of a Bind VarRef to point into a new
    /// heap-allocated buffer. Called during take_frame when an async frame's
    /// data is transferred from the arena to an independent Heap Box.
    ///
    /// The offset within the data region is computed from the frame header's
    /// arg_base_offset or var_base_offset plus the variable index.
    pub(crate) fn update_pvalue_for_heap_buf(
        &mut self,
        heap_buf: &Box<[JSValue]>,
        head: &StackFrame,
    ) {
        // delegate to inner impl to avoid Rust 2024 match ergonomics issues
        // with GcRef::as_mut() indirection
        unsafe {
            VarRefImpl::update_pvalue_for_heap_buf_impl(self.0.as_mut(), heap_buf, head);
        }
    }

    /// Variant of update_pvalue_for_heap_buf that takes the base offsets
    /// directly instead of a &FrameHead reference. This avoids borrow-checker
    /// conflicts when the caller needs to mutably borrow FrameHead.var_ref_list
    /// while simultaneously reading FrameHead offset fields.
    pub(crate) fn update_pvalue_for_heap_buf_offsets(
        &mut self,
        heap_buf: &Box<[JSValue]>,
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

    /// update variable value
    pub(crate) fn set_value(&mut self, rt: &mut RuntimeImpl, new_value: JSValue) {
        self.set_value_internal(new_value);
        if let Some(n) = new_value.gc_node_ptr() {
            rt.gc_heap_mut().bind(self.gc_node_ptr(), n);
        }
    }

    /// create a detached var ref
    pub(crate) fn create(ctx: &mut ScopedContext, value: JSValue) -> Result<Self, i32> {
        ctx.alloc_local(VarRefImpl::Detached(value))
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })
    }

    /// create a bind var ref
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
}
