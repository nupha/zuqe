use std::ptr::NonNull;

use gc_lite::{GcRef, GcTracable};

use crate::{
    context::{ContextImpl, EvalMode},
    gcref_wrapper,
    stack_frame::StackFrame,
    value::JSValue,
};

use super::AsyncFuncState;

#[repr(u8)]
#[derive(Debug, Clone)]
enum VarRefState {
    /// var ref is detached
    Detached { value: JSValue },
    /// reference to a target var
    Bind {
        async_func: Option<AsyncFuncState>,
        var_idx: u16,
        /// raw pointer to var value, to slot var_idx
        pvalue: NonNull<JSValue>,
    },
}

unsafe impl GcTracable for VarRefState {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        match self {
            Self::Detached { value } => {
                value.trace(tracer);
            }
            Self::Bind {
                async_func: Some(a),
                pvalue,
                ..
            } => {
                unsafe {
                    pvalue.as_ref().trace(tracer);
                }
                a.trace(tracer);
            }
            _ => {}
        }
    }
}

#[derive(Debug)]
pub struct JSVarRefInner {
    state: VarRefState,
    is_arg: bool,
}

unsafe impl GcTracable for JSVarRefInner {
    #[inline(always)]
    fn trace(&self, tracer: &mut gc_lite::GcTracer) {
        self.state.trace(tracer);
    }
}

impl JSVarRefInner {
    /// create a detached var ref
    const fn new(value: JSValue) -> Self {
        Self {
            state: VarRefState::Detached { value },
            is_arg: false,
        }
    }

    /// create a bind var ref
    const fn new_bind(
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
        async_func: Option<AsyncFuncState>,
    ) -> Self {
        Self {
            state: VarRefState::Bind {
                var_idx,
                pvalue,
                async_func,
            },
            is_arg,
        }
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    #[inline(always)]
    pub const fn is_arg(&self) -> bool {
        self.is_arg
    }

    pub const fn set_arg(&mut self, b: bool) {
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
    pub const fn value(&self) -> JSValue {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => unsafe { *pvalue.as_ref() },
            VarRefState::Detached { value } => *value,
        }
    }

    /// set value of var
    #[inline]
    pub const fn set_value(&mut self, new_value: JSValue) {
        match &mut self.state {
            VarRefState::Bind { pvalue, .. } => unsafe {
                *pvalue.as_mut() = new_value;
            },
            VarRefState::Detached { value } => {
                *value = new_value;
            }
        }
    }

    #[inline(always)]
    pub const fn is_detached(&self) -> bool {
        matches!(&self.state, VarRefState::Bind { .. })
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
    pub const fn detach(&mut self) {
        if let VarRefState::Bind { pvalue, .. } = &self.state {
            let value = unsafe { *pvalue.as_ref() };
            // change state
            self.state = VarRefState::Detached { value };
        }
    }

    /// returns id of target bind var, if state is bind.
    pub const fn var_idx(&self) -> Option<u16> {
        if let VarRefState::Bind { var_idx, .. } = &self.state {
            Some(*var_idx)
        } else {
            None
        }
    }

    /// get async function state pointer
    pub const fn async_func(&self) -> Option<AsyncFuncState> {
        if let VarRefState::Bind { async_func, .. } = &self.state {
            *async_func
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct JSVarRef(GcRef<JSVarRefInner>);

gcref_wrapper!(JSVarRef, JSVarRefInner);

impl JSVarRef {
    /// create a detached var ref
    pub fn new(ctx: &ContextImpl, value: JSValue) -> Result<Self, i32> {
        let inner = JSVarRefInner::new(value);
        let gc_ref = ctx.rt_ptr().gc_alloc(inner).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        Ok(Self(gc_ref))
    }

    /// create a bind var ref
    pub fn new_bind(
        ctx: &ContextImpl,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
        async_func: Option<AsyncFuncState>,
    ) -> Result<Self, i32> {
        let inner = JSVarRefInner::new_bind(var_idx, is_arg, pvalue, async_func);
        let gc_ref = ctx.rt_ptr().gc_alloc(inner).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        Ok(Self(gc_ref))
    }
}

impl StackFrame {
    /// find VarRef by id and is_arg. if not found, create new.
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
            .cloned()
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
                    self.arg_base_ptr.offset(var_idx as isize)
                } else {
                    self.var_base_ptr.offset(var_idx as isize)
                })
            },
            if self.js_mode.contains(EvalMode::ASYNC) {
                // The stack frame is detached and may be destroyed at any
                // time so its reference count must be increased. Calling
                // close_var_refs() when destroying the stack frame is not
                // possible because it would change the graph between the GC
                // objects. Another solution could be to temporarily detach
                // the JSVarRef of async functions during the GC. It would
                // have the advantage of allowing the release of unused stack
                // frames in a cycle.

                let af = self
                    .async_func
                    .upgrade(ctx.rt_ptr().gc_heap())
                    .map(AsyncFuncState::from);

                debug_assert!(af.is_some());
                af
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
