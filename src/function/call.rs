use std::{
    cell::RefCell,
    ops::{Deref, DerefMut},
    rc::Rc,
};

use crate::{
    atom::Atom,
    context::{Context, ContextImpl, ContextPtr},
    error::JSResult,
    function::build_arg_list,
    object::{ObjectPayload, ObjectRef},
    utils::unlikely,
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

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IteratorState {
    /// iterator is not done
    NotDone,
    /// iterator is done
    Done,
    /// iterator object is not parsed
    NotParsed,
}

pub const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

impl ObjectRef {
    /// call `self` object. if object is not callable, returns not function error.
    #[inline(always)]
    pub fn call(&self, ctx: &ContextImpl, this_obj: JSValue, args: &[JSValue]) -> JSResult {
        JS_CallInternal(
            ctx,
            JSValue::Object(*self),
            this_obj,
            JSValue::UNDEFINED,
            args,
            JS_CALL_FLAG_COPY_ARGV,
        )
        .err()
    }

    /// call function `self.method`
    ///
    /// `method` is not consumed
    #[inline]
    pub fn invoke(&self, ctx: &ContextImpl, method: Atom, args: &[JSValue]) -> JSResult {
        self.get_property(ctx, method)?
            .call(ctx, (*self).into(), args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None
    pub fn invoke_defined(
        &self,
        ctx: &ContextImpl,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if !m.is_undefined_or_null() {
            return Ok(Some(m.call(ctx, (*self).into(), args)));
        }
        Ok(None)
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    pub fn invoke_callable(
        &self,
        ctx: &ContextImpl,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if m.is_function() {
            Ok(Some(m.call(ctx, (*self).into(), args)))
        } else {
            Ok(None)
        }
    }
}

impl JSValue {
    /// call `self` value. if value is not callable, returns 'not function error'.
    #[inline(always)]
    pub fn call(&self, ctx: &ContextImpl, this_obj: JSValue, args: &[JSValue]) -> JSResult {
        JS_CallInternal(
            ctx,
            *self,
            this_obj,
            JSValue::UNDEFINED,
            args,
            JS_CALL_FLAG_COPY_ARGV,
        )
        .err()
    }

    /// call function `self.method`
    #[inline]
    pub fn invoke(&self, ctx: &ContextImpl, method: Atom, args: &[JSValue]) -> JSResult {
        self.get_property(ctx, method)?.call(ctx, *self, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None.
    pub fn invoke_defined(
        &self,
        ctx: &ContextImpl,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if !m.is_undefined_or_null() {
            Ok(Some(m.call(ctx, *self, args)))
        } else {
            Ok(None)
        }
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    ///
    /// `method` is not consumed
    pub fn invoke_callable(
        &self,
        ctx: &ContextImpl,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if m.is_function() {
            let r = m.call(ctx, *self, args);
            Ok(Some(r))
        } else {
            Ok(None)
        }
    }
}

/// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0.
/// both `this_obj` and `argv` values are not consumed.
pub(crate) fn JS_CallInternal(
    caller_ctx: &ContextImpl,
    func_obj: JSValue,
    this_obj: JSValue,
    new_target: JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSValue {
    if caller_ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    match func_obj {
        JSValue::Object(o) => {
            if let Some(bf) = o.as_BytecodeFunction() {
                bf.call(caller_ctx, this_obj, new_target, args, flags)
            } else {
                // non bytecode function: bound function, native function, async function etc.
                // use callback defined on the class.
                if let Some(f) = caller_ctx.rt().class_registry[o.class_id() as usize].call {
                    f(
                        unsafe { ContextPtr::from_ref_unchecked(caller_ctx).into() },
                        func_obj,
                        this_obj,
                        args,
                        flags,
                    )
                    .unwrap_or(JSValue::EXCEPTION)
                } else {
                    caller_ctx.throw_type_error("not callable");
                    JSValue::EXCEPTION
                }
            }
        }
        _ => {
            caller_ctx.throw_type_error("not a function");
            JSValue::EXCEPTION
        }
    }
}

pub(crate) fn JS_CallConstructorInternal(
    ctx: &ContextImpl,
    func_obj: JSValue,
    new_target: JSValue,
    args: &[JSValue],
    mut flags: i32,
) -> JSValue {
    if ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    if let JSValue::Object(o) = func_obj {
        if !o.flags().is_constructor() {
            ctx.throw_type_error("not a constructor");
            return JSValue::EXCEPTION;
        }

        flags |= JS_CALL_FLAG_CONSTRUCTOR;

        if let Some(bcf) = o.as_BytecodeFunction() {
            // BYTECODE_FUNCTION
            if bcf.payload().bytecode().attrs.is_derived_class_constructor {
                JS_CallInternal(ctx, func_obj, JSValue::UNDEFINED, new_target, args, flags)
            } else {
                // legacy constructor behavior
                let this: JSValue =
                    if let Ok(o) = ctx.create_object_from_ctor(new_target, ObjectPayload::OBJECT) {
                        o.into()
                    } else {
                        return JSValue::EXCEPTION;
                    };

                let ret = JS_CallInternal(ctx, func_obj, this, new_target, args, flags);
                if ret.is_object() || ret.is_exception() {
                    ret
                } else {
                    this
                }
            }
        } else {
            // call class exotic handler
            if let Some(cb) = ctx.rt().class_registry[o.class_id() as usize].call {
                cb(
                    unsafe { ContextPtr::from_ref_unchecked(ctx).into() },
                    func_obj,
                    new_target,
                    args,
                    flags,
                )
                .unwrap_or(JSValue::EXCEPTION)
            } else {
                ctx.throw_type_error("not a function");
                JSValue::EXCEPTION
            }
        }
    } else {
        ctx.throw_type_error("not a function");
        JSValue::EXCEPTION
    }
}

#[inline(always)]
pub(crate) fn JS_CallConstructor2(
    ctx: &ContextImpl,
    func_obj: JSValue,
    new_target: JSValue,
    args: &[JSValue],
) -> JSValue {
    JS_CallConstructorInternal(ctx, func_obj, new_target, args, JS_CALL_FLAG_COPY_ARGV)
}

#[inline(always)]
pub fn JS_CallConstructor(ctx: &ContextImpl, func_obj: JSValue, args: &[JSValue]) -> JSValue {
    JS_CallConstructorInternal(ctx, func_obj, func_obj, args, JS_CALL_FLAG_COPY_ARGV)
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub(crate) fn js_function_apply(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let this_val = *cc.this();
    this_val.ensure_function(ctx)?;

    let this_arg = *cc.arg(0);
    let array_arg = *cc.arg(1);

    if unlikely(array_arg.is_undefined_or_null() && cc.magic() != 2) {
        this_val.call(ctx, this_arg, &[])
    } else {
        let arg_list = build_arg_list(ctx, array_arg)?;
        if cc.magic() & 1 != 0 {
            JS_CallConstructor2(ctx, this_val, this_arg, arg_list.as_ref()).err()
        } else {
            this_val.call(ctx, this_arg, arg_list.as_ref())
        }
    }
}
