use std::{
    cell::RefCell,
    ptr::NonNull,
    rc::{Rc, Weak},
};

use crate::{
    context::{JS_MODE_ASYNC, JSContext},
    gc::GCObjectHeader,
    object::GCObjectWeakref,
    runtime::RuntimePtr,
    stack_frame::StackFrame,
    value::JSValue,
};

use super::AsyncFuncState;

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum VarRefState<'a> {
    /// var ref is detached
    Detached { value: JSValue<'a> },

    /// reference to a target var
    Bind {
        var_idx: u16,
        /// raw pointer to variable value
        pvalue: NonNull<JSValue<'a>>,
        async_func: Option<AsyncFuncState<'a>>,
    },
}

#[derive(Debug)]
pub(crate) struct JSVarRefInner<'a> {
    pub header: GCObjectHeader<'a>,

    /// bitfield for is_detached and is_arg
    pub bitfield: u8,

    pub state: VarRefState<'a>,
}

impl<'a> Drop for JSVarRefInner<'a> {
    fn drop(&mut self) {
        self.finalize();
    }
}

impl<'a> JSVarRefInner<'a> {
    /// create a detached var ref
    pub const fn new(rt: RuntimePtr<'a>, value: JSValue<'a>) -> Self {
        Self {
            header: GCObjectHeader::new(rt, 1),
            bitfield: 0,
            state: VarRefState::Detached { value },
        }
    }

    /// create a bind var ref
    pub const fn new_bind(
        rt: RuntimePtr<'a>,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue<'a>>,
        async_func: Option<AsyncFuncState<'a>>,
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

    #[inline(always)]
    pub const fn is_arg(&self) -> bool {
        self.bitfield & 0b0100_0000 != 0
    }

    pub const fn set_arg(&mut self, v: bool) {
        self.bitfield = (self.bitfield & !0b0100_0000) | ((v as u8) << 6)
    }

    /// get raw pointer to value
    fn value_ptr(&self) -> NonNull<JSValue<'a>> {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => *pvalue,
            VarRefState::Detached { value } => NonNull::from(value),
        }
    }

    /// get value of the var.
    #[inline]
    pub fn value(&self) -> JSValue<'a> {
        match &self.state {
            VarRefState::Bind { pvalue, .. } => unsafe { pvalue.as_ref().clone() },
            VarRefState::Detached { value } => value.clone(),
        }
    }

    /// set value of var ref, free old value.
    /// Note: `value` is consumed
    #[inline]
    pub fn set_value(&mut self, new_value: JSValue<'a>) {
        match &mut self.state {
            VarRefState::Bind { pvalue, .. } => unsafe {
                std::ptr::replace(pvalue.as_ptr(), new_value);
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
    pub fn detach(&mut self) {
        if let VarRefState::Bind { pvalue, .. } = &mut self.state {
            let value = unsafe { pvalue.as_mut().clone() };
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
    pub fn async_func(&self) -> Option<AsyncFuncState<'a>> {
        if let VarRefState::Bind { async_func, .. } = &self.state {
            async_func.clone()
        } else {
            None
        }
    }

    // fn gc_dump(&self, _rt: &JSRuntime) {
    //     println!("{:14p} [var_ref]", self);
    // }

    const fn finalize(&mut self) {
        // match &mut self.state {
        //     VarRefState::Detached { value } => {
        //         // JS_FreeValueRT(rt, &value);
        //     }
        //     VarRefState::Bind { async_func, .. } => {
        //         // if !async_func.is_null() {
        //         //     AsyncFuncState::free(rt, async_func);
        //         // }
        //     }
        // }
    }
}

#[derive(Debug, Clone)]
pub struct JSVarRef<'a>(pub(crate) Rc<RefCell<JSVarRefInner<'a>>>);

impl<'a> JSVarRef<'a> {
    /// create a detached var ref
    pub fn new(rt: RuntimePtr<'a>, value: JSValue<'a>) -> Self {
        let inner = JSVarRefInner::new(rt, value);
        let s = Rc::new(RefCell::new(inner));
        Self(s)
    }

    /// create a bind var ref
    pub fn new_bind(
        rt: RuntimePtr<'a>,
        var_idx: u16,
        is_arg: bool,
        pvalue: NonNull<JSValue<'a>>,
        async_func: Option<AsyncFuncState<'a>>,
    ) -> JSVarRef<'a> {
        let inner = JSVarRefInner::new_bind(rt, var_idx, is_arg, pvalue, async_func);
        let s = Rc::new(RefCell::new(inner));
        Self(s)
    }

    pub fn id(&self) -> usize {
        self.0.borrow().id()
    }

    pub fn downgrade(&self) -> JSVarRefWeakref<'a> {
        JSVarRefWeakref(Rc::downgrade(&self.0))
    }

    /// get pointer to value of the var.
    #[inline(always)]
    pub(crate) fn value_ptr(&self) -> NonNull<JSValue<'a>> {
        self.0.borrow().value_ptr()
    }

    /// get value of the var.
    #[inline(always)]
    pub fn value(&self) -> JSValue<'a> {
        self.0.borrow().value()
    }

    /// set new value to the referenced var. frees its old value.
    #[inline(always)]
    pub fn set_value(&self, value: JSValue<'a>) {
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
    pub fn detach(&self) {
        self.0.borrow_mut().detach();
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
    pub fn async_func(&self) -> Option<AsyncFuncState<'a>> {
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
pub struct JSVarRefWeakref<'a>(Weak<RefCell<JSVarRefInner<'a>>>);

impl<'a> From<JSVarRefWeakref<'a>> for GCObjectWeakref<'a> {
    fn from(value: JSVarRefWeakref<'a>) -> Self {
        Self::VarRef(value)
    }
}

impl<'a> JSVarRefWeakref<'a> {
    pub fn upgrade(&self) -> Option<JSVarRef<'a>> {
        self.0.upgrade().map(JSVarRef)
    }
}

impl<'a> StackFrame<'a> {
    /// find VarRef by id and is_arg. if not found, create new.
    pub(crate) fn get_var_ref(
        &mut self,
        ctx: &JSContext<'a>,
        var_idx: u16,
        is_arg: bool,
    ) -> Result<JSVarRef<'a>, i32> {
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
            ctx.rt(),
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

        ctx.rt().gc_add_object(vref.id(), vref.downgrade().into());
        self.var_ref_list.push(vref.clone());

        Ok(vref)
    }

    pub(crate) fn close_var_refs(&self) {
        for var_ref in self.var_ref_list.iter() {
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
