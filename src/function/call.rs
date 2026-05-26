// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    object::{Object, ObjectImpl, ObjectPayload},
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
    /// iterator is not done
    NotDone,
    /// iterator is done
    Done,
    /// iterator object is not parsed
    NotParsed,
}

pub(crate) const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub(crate) const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub(crate) const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

impl ObjectImpl {
    /// The unified place to dispatch a function call:
    /// either bytecode or native execution, either normal or constructor call.
    ///
    /// # Note:
    ///
    /// For bytecode function, will create a new sub scope for execution,
    /// return value is guaranteed to be valid at least in current scope.
    ///
    /// # Arguments
    /// * ctx is the caller ctx.
    /// * argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0.
    /// * flags & JS_CALL_FLAG_CONSTRUCTOR !=0 indicates a constructor call
    fn call_internal(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        new_target: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        ctx.poll_interrupts()?;

        let is_ctor = flags & JS_CALL_FLAG_CONSTRUCTOR != 0;
        if unlikely(is_ctor && !self.flags().is_constructor()) {
            ctx.throw_type_error("not a constructor");
            return Err(-1);
        }

        let ret_val = if let Some(mut bfun) = Object::from_ref(self).as_BytecodeFunction().copied()
        {
            // When calling bytecode function, a StackFrame is created for execution,
            // gc nodes self, this_obj, new_target, args are protected by the StackFrame
            let is_legacy_ctor =
                is_ctor && !bfun.payload().bytecode().attrs.is_derived_class_constructor;

            let this: JSValue = if is_legacy_ctor {
                // legacy constructor behavior.
                ctx.create_instance_with_ctor(new_target, ObjectPayload::OBJECT)
                    .map_err(|_| {
                        ctx.throw_type_error("constructor must return an object");
                        -1
                    })?
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
                Object::from_ref(self).into(),
                if is_ctor { new_target } else { this_obj },
                args,
                flags,
            );

            // ES2023 OrdinaryCreateFromConstructor: when a constructor is called
            // via super() from a derived class, newTarget determines the instance
            // prototype. Native constructors should use
            // `create_class_instance_with_opaque(new_target, ...)` to create
            // instances with the correct prototype from the start.
            //
            // If a native constructor used the deprecated `create_class_instance_with_opaque()`
            // (which ignores newTarget), the instance prototype will be wrong.
            //
            // Debug mode: panic immediately to force the constructor author to fix it.
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
                let self_val: JSValue = Object::from_ref(self).into();

                if new_target != self_val   // new_target differs from this constructor → called as super()
                   && let Ok(ref mut val) = result
                        && let Some(obj) = val.as_object_mut()
                        && let Ok(proto_val) = new_target.get_property(ctx, Atom::CONST_prototype)
                        && (proto_val.is_object() || proto_val.is_null())
                {
                    // Per ES OrdinaryCreateFromConstructor:
                    //   1. proto = Get(newTarget, "prototype")
                    //   2. If proto_val is not an Object, proto = GetFunctionRealm(newTarget).intrinsicDefaultProto
                    //
                    // When newTarget.prototype is not an object, the fallback prototype
                    // comes from newTarget's realm, NOT the current realm.
                    let expected_proto = if let Some(p) = proto_val.as_object().copied() {
                        Some(p)
                    } else {
                        // Fallback: GetFunctionRealm(newTarget) → realm.class_proto(class_id)
                        let class_id = obj.class_id();
                        new_target
                            .as_object()
                            .and_then(|o| o.realm(ctx.ctx_ptr).ok())
                            .and_then(|realm| realm.class_proto(class_id))
                    };

                    // Debug mode behavior: panic immediately to force the constructor author to fix it
                    #[cfg(debug_assertions)]
                    {
                        let actual_proto = obj.shape_proto();
                        debug_assert!(
                            actual_proto == expected_proto,
                            "Native constructor returned object with wrong prototype. \
                                     The constructor should use \
                                     `create_class_instance_with_opaque(new_target, class_id, opaque)` \
                                     instead of the deprecated \
                                     `create_class_instance_with_opaque(class_id, opaque)` \
                                     to respect newTarget.prototype per ES OrdinaryCreateFromConstructor."
                        );
                    }

                    // Release mode behavior: fix the prototype silently
                    if let Some(p) = expected_proto {
                        let _ = obj.set_prototype(ctx, JSValue::Object(p));
                    } else {
                        let _ = obj.set_prototype(ctx, JSValue::NULL);
                    }
                }
            }

            result
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        };

        #[cfg(debug_assertions)]
        if let Ok(v) = ret_val.as_ref()
            && let Some(n) = v.gc_node()
        {
            n.debug_assert_node_valid(ctx.rt_ptr().gc_heap());
        }

        ret_val
    }

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
        self.call_ctor_new_target(ctx, Object::from_ref(self).into(), args)
    }

    /// call function `self.method`.
    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline]
    pub fn invoke(&self, ctx: &mut ScopedContext, method: Atom, args: &[JSValue]) -> JSResult {
        self.get_property(ctx, method)
            .and_then(|f| f.call(ctx, Object::from_ref(self).into(), args))
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None
    pub fn invoke_defined(
        &self,
        ctx: &mut ScopedContext,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let f = self.get_property(ctx, method)?;
        if !f.is_undefined_or_null() {
            Ok(Some(f.call(ctx, Object::from_ref(self).into(), args)))
        } else {
            Ok(None)
        }
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    pub fn invoke_callable(
        &self,
        ctx: &mut ScopedContext,
        method: Atom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let f = self.get_property(ctx, method)?;
        if f.is_callable() {
            Ok(Some(f.call(ctx, Object::from_ref(self).into(), args)))
        } else {
            Ok(None)
        }
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
        if let Some(o) = self.as_object() {
            o.call_internal(ctx, this_obj, new_target, args, flags)
        } else {
            ctx.throw_type_error("not a function");
            Err(-1)
        }
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
    /// For bytecode function, new scope will be created to execute bytecodes.
    #[inline]
    pub fn invoke(&self, ctx: &mut ScopedContext, method: Atom, args: &[JSValue]) -> JSResult {
        let f = self.get_property(ctx, method)?;
        f.call(ctx, *self, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None.
    pub fn invoke_defined(
        &self,
        ctx: &mut ScopedContext,
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
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub(crate) fn js_function_apply(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    this_val.ensure_callable(ctx)?;

    let this_arg = cc.arg(0);
    let array_arg = cc.arg(1);

    if array_arg.is_undefined_or_null() && cc.magic() != 2 {
        // magic: 0/1 normal or constructor apply
        this_val.call(ctx, this_arg, &[])
    } else {
        let arg_list = build_arg_list(ctx, array_arg)?;
        if cc.magic() & 1 != 0 {
            // magic: 1 constructor apply
            this_val.call_internal(
                ctx,
                JSValue::UNDEFINED,
                this_arg,
                &arg_list,
                JS_CALL_FLAG_COPY_ARGV | JS_CALL_FLAG_CONSTRUCTOR,
            )
        } else {
            // magic: 0/2 normal or Reflect.apply
            this_val.call(ctx, this_arg, arg_list.as_ref())
        }
    }
}
