// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::{Exception, JSResult},
    object::{JS_PROP_NORMAL, JS_PROP_TMASK, JSProperty, Object, ObjectImpl, ObjectPayload},
    utils::unlikely,
    value::JSValue,
};

use super::build_arg_list;

pub struct CallContext<'cc> {
    this_value: JSValue,
    args: &'cc [JSValue],
    data: Option<&'cc mut [JSValue]>,
    magic: i32,
}

impl<'cc> CallContext<'cc> {
    pub(crate) const fn new(
        this_value: JSValue,
        args: &'cc [JSValue],
        magic: i32,
        data: Option<&'cc mut [JSValue]>,
    ) -> Self {
        Self {
            this_value,
            args,
            magic,
            data,
        }
    }

    #[inline(always)]
    pub(crate) const fn new_simple(this: JSValue, args: &'cc [JSValue]) -> Self {
        Self::new(this, args, 0, None)
    }

    #[inline(always)]
    pub const fn magic(&self) -> i32 {
        self.magic
    }

    /// number of actual arguments
    #[inline(always)]
    pub const fn length(&self) -> usize {
        self.args.len()
    }

    /// get `this` value
    #[inline(always)]
    pub const fn this(&self) -> JSValue {
        self.this_value
    }

    /// set new `this` value, drop old `this`.
    pub(crate) const fn set_this(&mut self, this_val: JSValue) {
        self.this_value = this_val;
    }

    /// argument values slice
    #[inline(always)]
    pub const fn arguments(&self) -> &[JSValue] {
        self.args
    }

    /// get `nth` argument, fail-safe to JSValue::UNDEFINED.
    #[inline]
    pub fn arg(&self, nth: usize) -> JSValue {
        self.args.get(nth).copied().unwrap_or(JSValue::UNDEFINED)
    }

    /// get `nth` bound data.
    #[inline]
    pub fn data(&self, nth: usize) -> JSValue {
        self.data
            .as_ref()
            .and_then(|d| d.get(nth).cloned())
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// number of bound data values
    #[inline]
    pub fn data_len(&self) -> usize {
        self.data.as_ref().map(|d| d.len()).unwrap_or(0)
    }

    /// Replace nth binding data with new `val`, returns the old value.
    /// returns `None` if `nth` is out of range.
    pub fn replace_data(&mut self, nth: usize, val: JSValue) -> Option<JSValue> {
        if let Some(lst) = self.data.as_deref_mut()
            && nth < lst.len()
        {
            return Some(std::mem::replace(&mut lst[nth], val));
        }

        None
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IteratorState {
    NotDone,
    Done,
    /// iterator object is not parsed
    NotParsed,
}

pub(crate) const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub(crate) const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub(crate) const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

impl ObjectImpl {
    /// Dispatch a function call (bytecode or native, normal or constructor).
    ///
    /// Runs under `with_active_frame_guard` because nested
    /// `execute_trampoline` (async/generator) clears `active_frame_index`
    /// on exit.
    ///
    /// For bytecode functions, creates a new scope for execution.
    /// Return value is valid within the current scope.
    fn call_internal(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        ctx.with_active_frame_guard(|cx| {
            self.do_call_internal(cx, this_obj, new_target, args, flags)
        })
    }

    fn do_call_internal(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        debug_assert!(self.is_callable(), "must be callable object");

        if ctx.poll_interrupts() {
            return Err(-1);
        }

        let is_ctor = flags & JS_CALL_FLAG_CONSTRUCTOR != 0;
        if unlikely(is_ctor && !self.flags().is_constructor()) {
            return Err(ctx.throw_type_error("not a constructor"));
        }

        let self_obj = Object::from_ref(self);

        let ret_val = if let Some(mut bfun) = self_obj.as_BytecodeFunction().copied() {
            // When calling bytecode function, a StackFrame is created for execution,
            // gc nodes self, this_obj, new_target, args are protected by the StackFrame
            let is_legacy_ctor =
                is_ctor && !bfun.payload().bytecode().attrs.is_derived_class_constructor;

            let this: JSValue = if is_legacy_ctor {
                // legacy constructor behavior.
                ctx.create_instance_with_ctor(new_target, ObjectPayload::OBJECT)
                    .map_err(|_| ctx.throw_type_error("constructor must return an object"))?
                    .into()
            } else {
                this_obj
            };

            if is_legacy_ctor {
                // legacy constructor behavior.
                bfun.call(ctx, this, new_target, args, flags)
                    .map(|v| if v.is_object() { v } else { this })
            } else {
                bfun.call(ctx, this, new_target, args, flags)
            }
        } else if let Some(cb) = unsafe {
            ctx.rt()
                .class_registry
                .get_unchecked(self.class_id() as usize)
                .call
        } {
            // calling class defined exotic callback (e.g. bound function, native function, async function etc)
            let mut result = cb(
                ctx,
                self_obj.into(),
                if is_ctor { new_target } else { this_obj },
                args,
                flags,
            );

            // ES2023 OrdinaryCreateFromConstructor: when a constructor is called
            // via super() from a derived class, newTarget determines the instance
            // prototype. Constructors should pass `new_target` to
            // `create_class_instance_with_opaque` to create instances with the
            // correct prototype from the start.
            //
            // Debug mode: panic immediately if the prototype doesn't match.
            // Release mode: silently fix the prototype as a safety net.
            //
            // Per ECMAScript spec, every built-in constructor that creates instances uses
            // OrdinaryCreateFromConstructor (§10.1.13), which derives the instance prototype
            // from newTarget.prototype (with GetFunctionRealm fallback). The prototype
            // check below applies to ALL such constructors by default.
            //
            // The following classes are excluded because their spec algorithms explicitly
            // do NOT use OrdinaryCreateFromConstructor:
            //
            // - Proxy (§10.4.11): [[Construct]] invokes the construct trap and returns
            //   the trap's result directly; no prototype constraint is enforced.
            // - BoundFunction (§10.4.12): [[Construct]] delegates to
            //   target.[[Construct]](args, newTarget); the target handles prototype itself.
            // - AsyncFunction: spec says "not a constructor"; cannot be invoked with `new`.
            // - PromiseResolve/RejectFunction, AsyncFunctionResolve/Reject: internal
            //   callable objects with no [[Construct]] defined in spec.
            //
            // NOTE: When adding a new class with `on_call`, re-evaluate whether its
            // spec algorithm calls OrdinaryCreateFromConstructor. If not, add it here.
            const SKIP_PROTO_CHECK: &[crate::ClassID] = &[
                crate::JS_CLASS_PROXY, // §10.4.11: construct trap returns arbitrary object
                crate::JS_CLASS_BOUND_FUNCTION, // §10.4.12: delegates to target.[[Construct]]
                crate::JS_CLASS_ASYNC_FUNCTION, // spec: not a constructor
                crate::JS_CLASS_ASYNC_FUNCTION_RESOLVE, // internal callable, no [[Construct]]
                crate::JS_CLASS_ASYNC_FUNCTION_REJECT, // internal callable, no [[Construct]]
                crate::JS_CLASS_PROMISE_RESOLVE_FUNCTION, // internal callable, no [[Construct]]
                crate::JS_CLASS_PROMISE_REJECT_FUNCTION, // internal callable, no [[Construct]]
            ];

            if is_ctor && !SKIP_PROTO_CHECK.contains(&self.class_id()) {
                let self_val = JSValue::from(self_obj);

                if new_target != self_val   // new_target differs from this constructor → called as super()
                   && let Ok(ref mut val) = result
                        && let Some(mut obj) = val.as_object ()
                        && let Some(proto_val) = new_target.as_object().and_then(get_prototype_no_side_effect)
                        && proto_val.is_object()
                {
                    // Per ES OrdinaryCreateFromConstructor:
                    //   1. proto = Get(newTarget, "prototype")
                    //   2. If Type(proto) is not Object, proto = GetFunctionRealm(newTarget).intrinsicDefaultProto
                    //
                    // We only reach here when proto is an Object (line 259 guard).
                    // Non-Object prototypes (null, undefined, etc.) trigger the
                    // intrinsic fallback which call.rs cannot verify (e.g., EvalError
                    // vs Error prototype both share JS_CLASS_ERROR class_id).
                    let expected_proto = proto_val.as_object();

                    // Debug mode behavior: panic immediately to force the constructor author to fix it
                    #[cfg(debug_assertions)]
                    {
                        let actual_proto = obj.shape_proto();
                        debug_assert!(
                            actual_proto == expected_proto,
                            "constructor returned object with wrong prototype; \
                             pass new_target to create_class_instance_with_opaque"
                        );
                    }

                    // Release mode behavior: fix the prototype silently
                    if let Some(p) = expected_proto {
                        let _ = obj.set_prototype(ctx, JSValue::from(p));
                    } else {
                        let _ = obj.set_prototype(ctx, JSValue::NULL);
                    }
                }
            }

            result
        } else {
            unreachable!("self is not a callable");
            // ctx.throw_type_error("not a function");
            // Err(-1)
        };

        #[cfg(debug_assertions)]
        if let Ok(v) = ret_val.as_ref()
            && let Some(n) = v.gc_node()
        {
            n.debug_assert_node_valid(ctx.rt_ptr().gc_heap());
        }

        // Exception-hygiene invariant: a call that returns Ok must leave
        // cur_exception empty. If a native/bytecode function swallowed a JS
        // exception (e.g. `let _ = some_func()` that threw) and still returned
        // Ok, cur_exception is left set; it would otherwise surface only later
        // as a confusing `[task bug]` panic at an unrelated job boundary.
        #[cfg(debug_assertions)]
        if ret_val.is_ok() {
            debug_assert!(
                ctx.rt().exception() == Exception::No,
                "call returned Ok but left an exception set; \
                 the callee must take_exception() if it swallows a throw"
            );
        }

        ret_val
    }

    #[inline(always)]
    pub fn call(&self, ctx: &mut ScopedContext, this_obj: JSValue, args: &[JSValue]) -> JSResult {
        self.ensure_callable(ctx)?.call_internal(
            ctx,
            this_obj,
            JSValue::UNDEFINED,
            args,
            JS_CALL_FLAG_COPY_ARGV,
        )
    }

    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline(always)]
    pub fn call_ctor_new_target(
        &self,
        ctx: &mut ScopedContext,
        new_target: JSValue,
        args: &[JSValue],
    ) -> JSResult {
        self.ensure_callable(ctx)?.call_internal(
            ctx,
            JSValue::UNDEFINED,
            new_target,
            args,
            JS_CALL_FLAG_COPY_ARGV | JS_CALL_FLAG_CONSTRUCTOR,
        )
    }

    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline(always)]
    pub fn call_ctor(&self, ctx: &mut ScopedContext, args: &[JSValue]) -> JSResult {
        self.call_ctor_new_target(ctx, Object::from_ref(self).into(), args)
    }

    /// call function `self.method`.
    #[inline(always)]
    pub fn invoke(&self, ctx: &mut ScopedContext, method: Atom, args: &[JSValue]) -> JSResult {
        JSValue::from(Object::from_ref(self)).invoke(ctx, method, args)
    }

    /// call function `self.method`.
    #[inline(always)]
    pub fn invoke_str(&self, ctx: &mut ScopedContext, method: &str, args: &[JSValue]) -> JSResult {
        JSValue::from(Object::from_ref(self)).invoke_str(ctx, method, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// throws TypeError if it exists but is not callable (GetMethod semantics).
    /// otherwise, returns None
    #[inline(always)]
    pub fn invoke_defined(
        &self,
        ctx: &mut ScopedContext,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        JSValue::from(Object::from_ref(self)).invoke_defined(ctx, method, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None
    #[inline(always)]
    pub fn invoke_str_defined(
        &self,
        ctx: &mut ScopedContext,
        method: &str,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        JSValue::from(Object::from_ref(self)).invoke_str_defined(ctx, method, args)
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    #[inline(always)]
    pub fn invoke_callable(
        &self,
        ctx: &mut ScopedContext,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        JSValue::from(Object::from_ref(self)).invoke_callable(ctx, method, args)
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    #[inline(always)]
    pub fn invoke_str_callable(
        &self,
        ctx: &mut ScopedContext,
        method: &str,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        JSValue::from(Object::from_ref(self)).invoke_str_callable(ctx, method, args)
    }
}

/// Read `func_obj.prototype` without observable side effects: only a plain
/// data property on the object's own shape counts. Proxies, getters and other
/// exotic lookups return None — the prototype safety net in `call_internal` is
/// not allowed to re-run user code (the spec performs Get(newTarget,
/// "prototype") exactly once, inside the constructor; a second Get would be
/// observable, e.g. by a proxy `get` trap, and its exception would be lost).
fn get_prototype_no_side_effect(func_obj: Object) -> Option<JSValue> {
    let (idx, prs_flags) = func_obj.find_shape_property(Atom::CONST_prototype)?;
    if prs_flags & JS_PROP_TMASK != JS_PROP_NORMAL {
        return None;
    }
    if let JSProperty::Normal(v) = func_obj.prop_slot(idx) {
        Some(*v)
    } else {
        None
    }
}

impl JSValue {
    /// `ctx` is the caller ctx.
    /// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0.
    pub(crate) fn call_internal(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        self.ensure_callable(ctx)?
            .call_internal(ctx, this_obj, new_target, args, flags)
    }

    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline(always)]
    pub fn call(&self, ctx: &mut ScopedContext, this_obj: JSValue, args: &[JSValue]) -> JSResult {
        self.call_internal(
            ctx,
            this_obj,
            JSValue::UNDEFINED,
            args,
            JS_CALL_FLAG_COPY_ARGV,
        )
    }

    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline(always)]
    pub fn call_ctor_new_target(
        &self,
        ctx: &mut ScopedContext,
        new_target: JSValue,
        args: &[JSValue],
    ) -> JSResult {
        self.call_internal(
            ctx,
            JSValue::UNDEFINED,
            new_target,
            args,
            JS_CALL_FLAG_COPY_ARGV | JS_CALL_FLAG_CONSTRUCTOR,
        )
    }

    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline(always)]
    pub fn call_ctor(&self, ctx: &mut ScopedContext, args: &[JSValue]) -> JSResult {
        self.call_ctor_new_target(ctx, *self, args)
    }

    /// Call `self.method` function.
    pub fn invoke(&self, ctx: &mut ScopedContext, method: Atom, args: &[JSValue]) -> JSResult {
        let f = self.get_property(ctx, method)?;
        f.call(ctx, *self, args)
    }

    /// Call `self.method` function.
    #[inline]
    pub fn invoke_str(&self, ctx: &mut ScopedContext, method: &str, args: &[JSValue]) -> JSResult {
        let a = Atom::from_str(ctx, method)?;
        self.invoke(ctx, a, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// throws TypeError if it exists but is not callable (GetMethod semantics).
    /// otherwise, returns None.
    pub fn invoke_defined(
        &self,
        ctx: &mut ScopedContext,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let Some(m) = self.get_method(ctx, method)? else {
            return Ok(None);
        };
        Ok(Some(m.call(ctx, *self, args)))
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None.
    pub fn invoke_str_defined(
        &self,
        ctx: &mut ScopedContext,
        method: &str,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let a = Atom::from_str(ctx, method)?;
        self.invoke_defined(ctx, a, args)
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    pub fn invoke_callable(
        &self,
        ctx: &mut ScopedContext,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if m.is_callable() {
            let r = m.call(ctx, *self, args);
            Ok(Some(r))
        } else {
            Ok(None)
        }
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    pub fn invoke_str_callable(
        &self,
        ctx: &mut ScopedContext,
        method: &str,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let a = Atom::from_str(ctx, method)?;
        self.invoke_callable(ctx, a, args)
    }
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub(crate) fn js_function_apply(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let func_obj = cc.this().ensure_callable(ctx)?;
    let this_arg = cc.arg(0);
    let array_arg = cc.arg(1);

    if array_arg.is_undefined_or_null() && cc.magic() != 2 {
        // magic: 0/1 normal or constructor apply
        func_obj.call(ctx, this_arg, &[])
    } else {
        let arg_list = build_arg_list(ctx, array_arg)?;
        if cc.magic() & 1 != 0 {
            // magic: 1 constructor apply
            func_obj.call_internal(
                ctx,
                JSValue::UNDEFINED,
                this_arg,
                &arg_list,
                JS_CALL_FLAG_COPY_ARGV | JS_CALL_FLAG_CONSTRUCTOR,
            )
        } else {
            // magic: 0/2 normal or Reflect.apply
            func_obj.call(ctx, this_arg, arg_list.as_ref())
        }
    }
}
