use std::{
    cell::RefCell,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    context::{JS_MODE_ASYNC, JSContext},
    gc::GCObjectHeader,
    object::GCObjectWeakref,
    runtime::JSRuntime,
    stack_frame::StackFrame,
    value::JSValue,
};

use super::ObjAsyncState;

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum VarRefState {
    /// var ref is detached
    Detached { value: JSValue },

    /// reference to a target var
    Bind {
        var_idx: u16,
        /// raw pointer to variable value
        pvalue: NonNull<JSValue>,
        async_func: Option<ObjAsyncState>,
    },
}

#[derive(Debug)]
pub(crate) struct JSVarRefInner {
    pub header: GCObjectHeader,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,

    pub state: VarRefState,
}

impl Drop for JSVarRefInner {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl JSVarRefInner {
    /// create a detached var ref
    pub fn new(rt: &mut JSRuntime, value: JSValue) -> Self {
        Self {
            header: GCObjectHeader::new(rt, 1),
            bitfield: 0,
            state: VarRefState::Detached { value },
        }
    }

    /// create a bind var ref
    pub fn new_bind(
        rt: &mut JSRuntime,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
        async_func: Option<ObjAsyncState>,
    ) -> Self {
        let mut vr = Self {
            header: GCObjectHeader::new(rt, 1),
            bitfield: 0,
            state: VarRefState::Bind {
                var_idx,
                pvalue,
                async_func,
            },
        };
        vr.set_arg(is_arg);

        vr
    }

    #[inline(always)]
    pub fn id(&self) -> usize {
        self as *const Self as usize
    }

    pub fn is_arg(&self) -> bool {
        self.bitfield & 0b0100_0000 != 0
    }

    pub fn set_arg(&mut self, v: bool) {
        self.bitfield = (self.bitfield & !0b0100_0000) | ((v as u8) << 6)
    }

    #[inline(always)]
    pub fn rt(&self) -> &JSRuntime {
        self.header.rt()
    }

    #[inline(always)]
    pub fn rt_mut(&mut self) -> &mut JSRuntime {
        self.header.rt_mut()
    }

    /// get raw pointer to value
    fn value_ptr(&self) -> NonNull<JSValue> {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => *pvalue,
            VarRefState::Detached { value } => NonNull::from(value),
        }
    }

    /// get value of the var.
    #[inline]
    pub fn value(&self) -> JSValue {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => unsafe { pvalue.as_ref().clone() },
            VarRefState::Detached { value } => value.clone(),
        }
    }

    /// set value of var ref, free old value.
    /// Note: `value` is consumed
    #[inline]
    pub fn set_value(&mut self, new_value: JSValue) {
        match &mut self.state {
            VarRefState::Bind { pvalue, .. } => unsafe {
                std::ptr::replace(pvalue.as_ptr(), new_value);
            },
            VarRefState::Detached { value } => {
                *value = new_value;
            }
        }
    }

    pub fn is_detached(&self) -> bool {
        if let VarRefState::Bind { .. } = &self.state {
            true
        } else {
            false
        }
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
    pub fn detach(&mut self, rt: &mut JSRuntime) {
        if let VarRefState::Bind { pvalue, .. } = &mut self.state {
            let value = unsafe { pvalue.as_mut().clone() };
            // change state
            self.state = VarRefState::Detached { value };
        }
    }

    /// returns id of target bind var, if state is bind.
    pub fn var_idx(&self) -> Option<u16> {
        if let VarRefState::Bind { var_idx, .. } = &self.state {
            Some(*var_idx)
        } else {
            None
        }
    }

    /// get async function state pointer
    pub fn async_func(&self) -> Option<ObjAsyncState> {
        if let VarRefState::Bind { async_func, .. } = &self.state {
            async_func.clone()
        } else {
            None
        }
    }

    fn gc_dump(&self, rt: &JSRuntime) {
        println!("{:14p} [var_ref]", self);
    }

    fn finalize(&mut self) {
        let rt = self.rt_mut();

        match &mut self.state {
            VarRefState::Detached { value } => {
                // JS_FreeValueRT(rt, &value);
            }
            VarRefState::Bind { async_func, .. } => {
                // if !async_func.is_null() {
                //     ObjAsyncState::free(rt, async_func);
                // }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct JSVarRef(pub(crate) Rc<RefCell<JSVarRefInner>>);

impl JSVarRef {
    /// create a detached var ref
    pub fn new(rt: &mut JSRuntime, value: JSValue) -> Self {
        let inner = JSVarRefInner::new(rt, value);
        let s = Rc::new(RefCell::new(inner));
        Self(s)
    }

    /// create a bind var ref
    pub fn new_bind(
        rt: &mut JSRuntime,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue>,
        async_func: Option<ObjAsyncState>,
    ) -> JSVarRef {
        let inner = JSVarRefInner::new_bind(rt, var_idx, is_arg, pvalue, async_func);
        let s = Rc::new(RefCell::new(inner));
        Self(s)
    }

    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn downgrade(&self) -> JSVarRefWeakref {
        JSVarRefWeakref(Rc::downgrade(&self.0))
    }

    /// get pointer to value of the var.
    #[inline(always)]
    pub(crate) fn value_ptr(&self) -> NonNull<JSValue> {
        self.0.borrow().value_ptr()
    }

    /// get value of the var.
    #[inline(always)]
    pub fn value(&self) -> JSValue {
        self.0.borrow().value()
    }

    /// set value of var ref, free old value.
    /// Note: `value` is consumed
    #[inline(always)]
    pub fn set_value(&self, value: JSValue) {
        self.0.borrow_mut().set_value(value);
    }

    #[inline(always)]
    pub fn is_detached(&self) -> bool {
        self.0.borrow().is_detached()
    }

    /// detach bind var ref as detached var ref, duplicate var value as detached value.
    /// this makes var ref no longer refer to a local variable.
    /// if has async funtion, free it too.
    /// if `self` is already detached, do nothing.
    #[inline(always)]
    pub fn detach(&self, rt: &mut JSRuntime) {
        self.0.borrow_mut().detach(rt);
    }

    /// returns id of target bind var, if state is bind.
    #[inline(always)]
    pub fn is_arg(&self) -> bool {
        self.0.borrow().is_arg()
    }

    /// returns id of target bind var, if state is bind.
    #[inline(always)]
    pub fn var_idx(&self) -> Option<u16> {
        self.0.borrow().var_idx()
    }

    /// get async function state pointer
    #[inline(always)]
    pub fn async_func(&self) -> Option<ObjAsyncState> {
        self.0.borrow().async_func()
    }

    pub fn gc_has_mark(&self) -> bool {
        self.0.borrow().header.gc_has_mark()
    }

    pub fn gc_set_mark(&self, mark: bool) {
        self.0.borrow_mut().header.gc_set_mark(mark);
    }
}

#[derive(Debug, Clone)]
pub struct JSVarRefWeakref(Weak<RefCell<JSVarRefInner>>);

impl From<JSVarRefWeakref> for GCObjectWeakref {
    fn from(value: JSVarRefWeakref) -> Self {
        Self::VarRef(value)
    }
}

impl JSVarRefWeakref {
    pub fn upgrade(&self) -> Option<JSVarRef> {
        self.0.upgrade().map(JSVarRef)
    }
}

#[deprecated(note = "use JSVarRef::dispose() instead")]
pub unsafe fn free_var_ref(rt: *mut JSRuntime, var_ref: *mut JSVarRef) {
    unreachable!("unused");
}

#[deprecated(note = "use JSStackFrame::close_lexical_var() instead")]
pub unsafe fn close_lexical_var(ctx: &JSContext, sf: *mut StackFrame, var_idx: u16, is_arg: bool) {
    (*sf).close_lexical_var(ctx, var_idx, is_arg);
}

#[deprecated(note = "use JSStackFrame::close_var_refs() instead")]
pub unsafe fn close_var_refs(ctx: &JSContext, sf: *mut StackFrame) {
    (*sf).close_var_refs(ctx);
}

impl StackFrame {
    // fn make_var_ref(
    //     &mut self,
    //     ctx: &JSContext,
    //     var_idx: u16,
    //     is_arg: bool,
    //     pvalue: *mut JSValue,
    // ) -> Result<Pin<Box<JSVarRef>>, i32> {
    //     let vr = JSVarRef::new_bind(var_idx, is_arg, pvalue, std::ptr::null_mut());
    //     Ok(vr)
    // }

    /// find VarRef by id and is_arg. if not found, create new.
    pub fn get_var_ref(
        &mut self,
        ctx: &JSContext,
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
            ctx.rt_mut(),
            var_idx,
            is_arg,
            unsafe {
                NonNull::new_unchecked(if is_arg {
                    self.arg_base_ptr.offset(var_idx as isize)
                } else {
                    self.var_base_ptr.offset(var_idx as isize)
                })
            },
            if self.js_mode & JS_MODE_ASYNC != 0 {
                // The stack frame is detached and may be destroyed at any
                // time so its reference count must be increased. Calling
                // close_var_refs() when destroying the stack frame is not
                // possible because it would change the graph between the GC
                // objects. Another solution could be to temporarily detach
                // the JSVarRef of async functions during the GC. It would
                // have the advantage of allowing the release of unused stack
                // frames in a cycle.

                let af = self.async_func.upgrade();
                debug_assert!(af.is_some());

                af
            } else {
                None
            },
        );

        ctx.rt_mut()
            .gc_add_object(vref.id(), vref.downgrade().into());
        self.var_ref_list.push(vref.clone());

        Ok(vref)
    }

    pub fn close_var_refs(&self, ctx: &JSContext) {
        let rt = ctx.rt_mut();
        for var_ref in self.var_ref_list.iter() {
            var_ref.detach(rt);
        }
    }

    pub fn close_lexical_var(&mut self, ctx: &JSContext, var_idx: u16, is_arg: bool) {
        if let Some(i) = self
            .var_ref_list
            .iter()
            .position(|v| v.var_idx() == Some(var_idx) && v.is_arg() == is_arg)
        {
            // remove the VarRef and make it detached
            self.var_ref_list.swap_remove(i).detach(ctx.rt_mut());
        }
    }
}
