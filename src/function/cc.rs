use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use crate::{
    Context,
    context::{ContextImpl, ContextPtr},
    value::JSValue,
};

pub struct CallContext<'cc> {
    ctx: ContextPtr,
    this_value: JSValue,
    args: &'cc [JSValue],
    magic: i32,

    /// additional binding values
    data: Option<Rc<RefCell<Box<[JSValue]>>>>,
}

impl<'cc> CallContext<'cc> {
    pub(crate) const fn new(
        ctx: ContextPtr,
        this_value: JSValue,
        args: &'cc [JSValue],
        magic: i32,
        data: Option<Rc<RefCell<Box<[JSValue]>>>>,
    ) -> Self {
        Self {
            ctx,
            this_value,
            args,
            magic,
            data,
        }
    }

    #[inline(always)]
    pub(crate) const fn new_simple(ctx: ContextPtr, this: JSValue, args: &'cc [JSValue]) -> Self {
        Self::new(ctx, this, args, 0, None)
    }

    /// Context handle
    #[inline(always)]
    pub fn context(&self) -> Context<'cc> {
        Context::from(self.ctx)
    }

    #[inline(always)]
    pub(crate) const fn ctx_ptr(&self) -> ContextPtr {
        self.ctx
    }

    #[inline(always)]
    pub fn ctx(&self) -> &ContextImpl {
        self.ctx.deref()
    }

    #[inline(always)]
    pub fn ctx_mut(&mut self) -> &mut ContextImpl {
        self.ctx.deref_mut()
    }

    pub const fn magic(&self) -> i32 {
        self.magic
    }

    /// number of actual arguments
    #[inline(always)]
    pub const fn length(&self) -> usize {
        self.args.len()
    }

    /// get `this` value ref
    #[inline(always)]
    pub const fn this(&self) -> &JSValue {
        &self.this_value
    }

    /// get `this` value ref
    #[inline(always)]
    pub const fn this_mut(&mut self) -> &mut JSValue {
        &mut self.this_value
    }

    /// set new `this` value, drop old `this`.
    /// Note: `this_val` is consumed.
    pub(crate) const fn set_this(&mut self, this_val: JSValue) {
        self.this_value = this_val;
    }

    /// get `nth` argument, fail-safe to JSValue::UNDEFINED.
    #[inline(always)]
    pub fn arg(&self, nth: usize) -> &JSValue {
        self.args.get(nth).unwrap_or(&JSValue::UNDEFINED)
    }

    /// get argument values slice
    #[inline(always)]
    pub const fn arguments(&self) -> &[JSValue] {
        self.args
    }

    /// get `nth` bound data.
    #[inline]
    pub fn data(&self, nth: usize) -> JSValue {
        self.data
            .as_ref()
            .and_then(|d| d.borrow().get(nth).cloned())
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// number of bound data values
    #[inline]
    pub fn data_len(&self) -> usize {
        self.data.as_ref().map(|d| d.borrow().len()).unwrap_or(0)
    }

    /// Replace nth data with new `val`, and returns old value.
    /// returns `None` if `nth` is out of range.
    pub fn replace_data(&mut self, nth: usize, val: JSValue) -> Option<JSValue> {
        if let Some(data) = self.data.as_ref() {
            let mut lst = data.borrow_mut();
            if nth < lst.len() {
                return Some(std::mem::replace(&mut lst[nth], val));
            }
        }

        None
    }
}
