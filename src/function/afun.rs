// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Atom, JS_CLASS_ASYNC_FUNCTION_REJECT, JS_CLASS_ASYNC_FUNCTION_RESOLVE,
    builtins::promise::{Promise, js_promise_resolve},
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{AsyncState, BCWrapper, BytecodeFunction, Callback, FunctionKind, NativeFunction},
    object::{AsyncFunction, JS_PROP_CONFIGURABLE, PayloadPtr},
    value::JSValue,
};

use super::{CallContext, FieldDef};

impl AsyncFunction {
    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        let proto = ctx.create_base_object_with_proto(Some(ctx.function_proto()))?;
        ctx.init_class_proto_internal(class_id, proto, js_async_function_proto_funcs)?;

        let ctor = NativeFunction::create(
            ctx,
            Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
            Atom::CONST_AsyncFunction,
            1,
            FunctionKind::Async as i16,
            ctx.function_ctor(),
            None,
        )?;
        ctx.bind_ctor_proto_ex(ctor.into(), proto, 0, JS_PROP_CONFIGURABLE)?;
        ctx.class_slots[class_id as usize].ctor = Some(ctor.into());

        Ok(())
    }

    /// Wrapper to run async function, returns a Promise.
    pub fn class_call(
        ctx: &mut ScopedContext,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        debug_assert!(
            func_obj.as_object_AsyncFunction().is_some(),
            "must be an async function"
        );

        func_obj
            .call_async(ctx, this_obj, args, flags)
            .map(|p| p.into())
    }
}

impl BCWrapper {
    /// run bytecode in async manner
    fn execute_async(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> Result<Promise, i32> {
        let (promise, resolving_funcs) = Promise::create(ctx)?;
        debug_assert!(promise.gc_node().is_local());

        let mut async_state = AsyncState::new(ctx, resolving_funcs, *self, this_obj, args)?;
        debug_assert!(async_state.gc_node().is_local());

        async_state.execute(ctx);
        Ok(promise)
    }
}

impl JSValue {
    /// if value is function object, call it in async manner
    pub fn call_async(
        &self,
        ctx: &mut ScopedContext,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> Result<Promise, i32> {
        if let Some(bc) = self.as_object_BCWrapper() {
            bc.execute_async(ctx, this_obj, args, flags)
        } else {
            Err(ctx.throw_type_error("not bytecode function"))
        }
    }
}

impl AsyncState {
    pub fn class_call(
        ctx: &mut ScopedContext,
        func_obj: JSValue,
        _this: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut o = unsafe { func_obj.as_object_unchecked() };
        let is_reject = o.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;
        let a_state = o.payload_async_function_resolving_mut().unwrap();

        if is_reject {
            let _ = ctx.throw_value(if args.is_empty() {
                JSValue::UNDEFINED
            } else {
                args[0]
            });
            a_state.set_throw_flag(true);
        } else {
            // return value of await
            {
                let arena = ctx.rt().stack_frame_arena();
                debug_assert!(a_state.cur_sp_offset(arena) > 0);
            }

            a_state.set_throw_flag(false);
            {
                let arena = ctx.rt_mut().stack_frame_arena_mut();
                a_state.set_return_value(
                    arena,
                    if args.is_empty() {
                        JSValue::UNDEFINED
                    } else {
                        args[0]
                    },
                );
            }
        }

        a_state.execute(ctx);
        Ok(JSValue::UNDEFINED)
    }

    fn execute(&mut self, ctx: &mut ScopedContext) {
        let ret = self.resume(ctx);

        if self.is_completed {
            // Settle the outer promise FIRST — resolving may queue reaction
            // jobs whose execution allocates and can trigger GC. The state
            // must stay rooted until all of that JS has finished running;
            // removing the root before these calls left the executing state
            // unprotected (UAF under allocation-heavy workloads).
            match ret {
                Ok(val) => {
                    // Normal function return - resolve
                    let _ = self.resolving_funcs[0].call(
                        ctx,
                        JSValue::UNDEFINED,
                        std::slice::from_ref(&val),
                    );
                }
                Err(_) => {
                    let err_val = ctx.take_exception().to_value(ctx);
                    let _ = self.resolving_funcs[1].call(
                        ctx,
                        JSValue::UNDEFINED,
                        std::slice::from_ref(&err_val),
                    );
                }
            }

            // Completed — drop the runtime root added in AsyncState::new so
            // finished states don't accumulate (identity = node pointer).
            let node = self.gc_node_ptr();
            let rt = ctx.rt_mut();
            if let Some(idx) = rt
                .gc_pending_async_states
                .iter()
                .position(|s| s.gc_node_ptr() == node)
            {
                rt.gc_pending_async_states.swap_remove(idx);
            }
        } else {
            // Retrieve value at stack top with GC protection.
            // Arena access is via ctx.rt() (immutable), compatible with ctx borrow.
            let value = {
                let arena = ctx.rt().stack_frame_arena();
                self.take_top_value(arena, ctx)
            };

            // Handle await expression
            let mut promise = if let Ok(v) = js_promise_resolve(
                ctx,
                CallContext::new_simple(ctx.promise_ctor().into(), std::slice::from_ref(&value)),
            ) {
                Promise::try_from(v).unwrap()
            } else {
                let err_val = ctx.take_exception().to_value(ctx);
                let _ = self.resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                ); // TODO what to do if exception?
                return;
            };

            let resolving_funcs = if let Ok(rr) = self.async_function_resolve_create(ctx) {
                rr
            } else {
                let err_val = ctx.take_exception().to_value(ctx);
                let _ = self.resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                );
                [const { JSValue::UNDEFINED }; 2]
            };

            let res = promise.reaction(
                ctx,
                resolving_funcs,
                // no need to create 'thrownawayCapability' as in the spec
                [const { JSValue::UNDEFINED }; 2],
            );

            if res.is_err() {
                let err_val = ctx.take_exception().to_value(ctx);
                let _ = self.resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                );
            }
        }
    }

    fn async_function_resolve_create(&self, ctx: &mut ScopedContext) -> Result<[JSValue; 2], i32> {
        let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];
        let state = AsyncState::from(self.gc_ref());

        for i in 0..2 {
            let func_obj = ctx.create_instance_with_proto(
                Some(ctx.function_proto()),
                PayloadPtr::new(
                    if i == 0 {
                        JS_CLASS_ASYNC_FUNCTION_RESOLVE
                    } else {
                        JS_CLASS_ASYNC_FUNCTION_REJECT
                    },
                    state,
                ),
            )?;

            resolving_funcs[i] = func_obj.into();
        }

        Ok(resolving_funcs)
    }
}

const js_async_function_proto_funcs: &[FieldDef] = &[FieldDef::STR(
    "[Symbol.toStringTag]",
    "AsyncFunction",
    JS_PROP_CONFIGURABLE,
)];
