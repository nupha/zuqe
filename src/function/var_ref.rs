use std::ptr::NonNull;

use gc_lite::{GcHead, GcPartitionId, GcRef, GcTracable, GcTraceOp};

use crate::{
    context::ContextImpl,
    gcref_wrapper,
    stack_frame::{ExecMode, StackFrame},
    value::JSValue,
};

use super::AsyncState;

#[repr(u8)]
#[derive(Debug, Clone)]
enum VarRefState {
    /// var ref is detached
    Detached { value: JSValue },
    /// reference to a target var
    Bind {
        async_func: Option<AsyncState>,
        var_idx: u16,
        /// raw pointer to var value, to slot var_idx
        pvalue: NonNull<JSValue>,
    },
}

unsafe impl GcTracable for VarRefState {
    fn trace(&self, tr: GcTraceOp) {
        match self {
            Self::Detached { value } => {
                value.trace(tr);
            }
            Self::Bind {
                async_func: Some(a),
                pvalue,
                ..
            } => {
                a.trace(tr);
                unsafe {
                    pvalue.as_ref().trace(tr);
                }
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct VarRefImpl {
    state: VarRefState,
    is_arg: bool,
}

unsafe impl GcTracable for VarRefImpl {
    fn trace(&self, tr: GcTraceOp) {
        self.state.trace(tr);
    }
}

impl VarRefImpl {
    #[inline(always)]
    pub(crate) fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub(crate) const fn is_arg(&self) -> bool {
        self.is_arg
    }

    pub(crate) const fn set_arg(&mut self, b: bool) {
        self.is_arg = b;
    }

    /// get raw pointer to value
    pub(crate) fn value_ptr(&self) -> NonNull<JSValue> {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => *pvalue,
            VarRefState::Detached { value } => NonNull::from(value),
        }
    }

    /// get value of the var.
    #[inline]
    pub(crate) const fn value(&self) -> JSValue {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => unsafe { *pvalue.as_ref() },
            VarRefState::Detached { value } => *value,
        }
    }

    /// update value of variable
    #[inline]
    pub(crate) fn set_value(&mut self, ctx: &ContextImpl, new_value: JSValue, xref: GcPartitionId) {
        match &mut self.state {
            VarRefState::Bind { pvalue, .. } => unsafe {
                *pvalue.as_mut() = new_value;
            },
            VarRefState::Detached { value } => {
                *value = new_value;
            }
        }

        if let Some(n) = new_value.gc_node_ptr() {
            ctx.rt_ptr().gc_heap_mut().set_xref(xref, n);
        }
    }

    #[inline(always)]
    pub(crate) const fn is_detached(&self) -> bool {
        matches!(&self.state, VarRefState::Bind { .. })
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
    pub(crate) const fn detach(&mut self) {
        if let VarRefState::Bind { pvalue, .. } = &self.state {
            let value = unsafe { *pvalue.as_ref() };
            self.state = VarRefState::Detached { value };
        }
    }

    /// returns id of target bind var, if state is bind.
    pub(crate) const fn var_idx(&self) -> Option<u16> {
        if let VarRefState::Bind { var_idx, .. } = &self.state {
            Some(*var_idx)
        } else {
            None
        }
    }

    /// get async function state pointer
    pub(crate) const fn async_func(&self) -> Option<AsyncState> {
        if let VarRefState::Bind { async_func, .. } = &self.state {
            *async_func
        } else {
            None
        }
    }

    pub(crate) fn gc_nodes(&self) -> impl Iterator<Item = NonNull<GcHead>> {
        let mut nodes: [Option<NonNull<GcHead>>; 2] = [None; 2];

        match self.state {
            VarRefState::Detached { value } => {
                if let Some(h) = value.gc_node_ptr() {
                    nodes[0] = Some(h);
                }
            }
            VarRefState::Bind {
                async_func, pvalue, ..
            } => {
                let mut i = 0;
                if let Some(a) = async_func.as_ref() {
                    nodes[i] = Some(a.gc_ref().node_ptr());
                    i += 1;
                }
                if let Some(p) = unsafe { pvalue.as_ref().gc_node_ptr() } {
                    nodes[i] = Some(p);
                }
            }
        }

        nodes.into_iter().flatten()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JSVarRef(GcRef<VarRefImpl>);

gcref_wrapper!(JSVarRef, VarRefImpl);

impl JSVarRef {
    /// create a detached var ref
    pub(crate) fn new(ctx: &ContextImpl, value: JSValue) -> Result<Self, i32> {
        ctx.rt_ptr()
            .gc_alloc(VarRefImpl {
                state: VarRefState::Detached { value },
                is_arg: false,
            })
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })
    }

    /// create a bind var ref
    pub fn new_bind(
        ctx: &ContextImpl,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
        async_func: Option<AsyncState>,
    ) -> Result<Self, i32> {
        ctx.rt_ptr()
            .gc_alloc(VarRefImpl {
                state: VarRefState::Bind {
                    var_idx,
                    pvalue,
                    async_func,
                },
                is_arg,
            })
            .map(Self)
            .map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })
    }
}

impl StackFrame {
    /// find or create VarRef by (var_id, is_arg).
    pub(crate) fn get_var_ref(
        &mut self,
        ctx: &ContextImpl,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<JSVarRef, i32> {
        // search existing var refs
        if let Some(vr) = self
            .var_ref_list
            .iter()
            .find(|v| v.var_idx() == Some(var_idx) && v.is_arg() == is_arg)
            .copied()
        {
            return Ok(vr);
        }

        // make new VarRef bind to target var_idx
        let vref = JSVarRef::new_bind(
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
                // the JSVarRef of async functions during the GC. It would
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

        self.var_ref_list.push(vref);

        Ok(vref)
    }

    pub(crate) fn close_var_refs(&mut self) {
        for var_ref in self.var_ref_list.iter_mut() {
            var_ref.detach();
        }
    }

    pub(crate) fn close_lexical_var(&mut self, var_idx: u16, is_arg: bool) {
        if let Some(i) = self
            .var_ref_list
            .iter()
            .position(|v| v.var_idx() == Some(var_idx) && v.is_arg() == is_arg)
        {
            // remove the VarRef and make it detached
            self.var_ref_list.swap_remove(i).detach();
        }
    }
}
