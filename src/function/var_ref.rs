// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{GcNode, GcRef, GcTrace, GcTraceCtx};

use crate::{
    RuntimeImpl,
    context::ContextImpl,
    gcref_wrapper,
    stack_frame::{ExecMode, StackFrame},
    value::JSValue,
};

use super::AsyncState;

#[repr(u8)]
#[derive(Debug)]
pub enum VarRefImpl {
    /// var ref is detached, point to nowhere.
    Detached { is_arg: bool, value: JSValue },
    /// reference to a var slot in stack frame
    Bind {
        is_arg: bool,
        var_idx: u16,
        /// Raw pointer to var value, equivelent to
        /// StackFrame::arg_base_ptr[var_idx] or StackFrame::var_base_ptr[var_idx],
        /// for speed access only
        pvalue: NonNull<JSValue>,

        async_state: Option<AsyncState>,
    },
}

impl GcTrace for VarRefImpl {
    fn trace(&self, tr: &mut GcTraceCtx) {
        match self {
            Self::Bind {
                async_state,
                pvalue,
                ..
            } => {
                unsafe {
                    pvalue.as_ref().trace(tr);
                }
                if let Some(a) = async_state {
                    a.trace(tr);
                }
            }
            Self::Detached { value, .. } => {
                value.trace(tr);
            }
        }
    }
}

impl VarRefImpl {
    /// get variable value
    #[inline(always)]
    pub(crate) const fn value(&self) -> JSValue {
        match self {
            Self::Bind { pvalue, .. } => unsafe { *pvalue.as_ref() },
            Self::Detached { value, .. } => *value,
        }
    }

    /// update variable value
    pub(crate) fn set_value(&mut self, rt: &mut RuntimeImpl, new_value: JSValue) {
        match self {
            Self::Bind { pvalue, .. } => unsafe {
                *pvalue.as_mut() = new_value;
            },
            Self::Detached { value, .. } => {
                *value = new_value;
            }
        }

        if let Some(n) = new_value.gc_node_ptr() {
            rt.gc_heap_mut().bind(self.gc_head_ptr(), n);
        }
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
    const fn detach(&mut self) {
        if let Self::Bind { pvalue, is_arg, .. } = self {
            *self = Self::Detached {
                is_arg: *is_arg,
                value: unsafe { *pvalue.as_ref() },
            };
        }
    }

    const fn is_match(&self, var_idx: u16, is_arg: bool) -> bool {
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
    /// create a detached var ref
    pub(crate) fn create(ctx: &ContextImpl, value: JSValue) -> Result<Self, i32> {
        ctx.rt_ptr()
            .gc_alloc(VarRefImpl::Detached {
                value,
                is_arg: false,
            })
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })
    }

    /// create a bind var ref
    fn create_bind(
        ctx: &ContextImpl,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
        async_state: Option<AsyncState>,
    ) -> Result<Self, i32> {
        ctx.rt_ptr()
            .gc_alloc(VarRefImpl::Bind {
                is_arg,
                var_idx,
                pvalue,
                async_state,
            })
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })
    }
}

impl StackFrame {
    /// find or create VarRef specified by (var_id, is_arg).
    pub(crate) fn get_var_ref(
        &mut self,
        ctx: &ContextImpl,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<VarRef, i32> {
        // search in existing var refs
        if let Some(vr) = self
            .var_refs()
            .iter()
            .find(|x| x.is_match(var_idx, is_arg))
            .copied()
        {
            return Ok(vr);
        }

        // make new VarRef bind to target var_idx
        let vref = VarRef::create_bind(
            ctx,
            var_idx,
            is_arg,
            unsafe {
                NonNull::new_unchecked(if is_arg {
                    debug_assert!(var_idx < self.arg_count);
                    self.arg_base_ptr.offset(var_idx as isize)
                } else {
                    debug_assert!(var_idx < self.var_count);
                    self.var_base_ptr.offset(var_idx as isize)
                })
            },
            if self.mode.contains(ExecMode::ASYNC) {
                // The stack frame is detached and may be destroyed at any
                // time so its reference count must be increased. Calling
                // close_var_refs() when destroying the stack frame is not
                // possible because it would change the graph between the GC
                // objects. Another solution could be to temporarily detach
                // the VarRef of async functions during the GC. It would
                // have the advantage of allowing the release of unused stack
                // frames in a cycle.
                let a_state = self
                    .async_state
                    .upgrade(ctx.rt_ptr().gc_heap())
                    .map(AsyncState::from);

                debug_assert!(a_state.is_some());
                a_state
            } else {
                None
            },
        )?;

        self.var_refs_mut().push(vref);

        Ok(vref)
    }

    /// detach var ref (lexical) specified by (var_idx, is_arg)
    pub(crate) fn close_var_ref(&mut self, var_idx: u16, is_arg: bool) {
        if let Some(vr) = self
            .var_refs_mut()
            .iter_mut()
            .find(|v| v.is_match(var_idx, is_arg))
        {
            vr.detach();
        }
    }

    pub(crate) fn close_var_refs(&mut self) {
        for vr in self.var_refs_mut().iter_mut() {
            vr.detach();
        }
    }
}
