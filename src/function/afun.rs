// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcRef;

use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    builtins::promise::{Promise, js_promise_resolve},
    class::ClassID,
    context::{Context, ContextImpl, ContextPtr},
    error::JSResult,
    function::{AsyncState, BCWrapper, BytecodeFunction, Callback, FunctionKind, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

use super::{CallContext, FieldDef};

/// Async Function object
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct AsyncFunction(pub(crate) GcRef<ObjectImpl>);

object_cast!(AsyncFunction, JS_CLASS_ASYNC_FUNCTION);

impl AsyncFunction {
    pub fn class_init(mut ctx: Context, _: ClassID) -> Result<Option<Object>, i32> {
        let mut proto = {
            let p = ctx.function_proto();
            ctx.create_base_object_with_proto(Some(p))?
        };
        proto.install_fields(&mut ctx, js_async_function_proto_funcs)?;

        let func_ctor = ctx.function_ctor.as_object().copied();
        let ctor: JSValue = NativeFunction::create(
            &mut ctx,
            Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
            "AsyncFunction",
            1,
            FunctionKind::Async as i16,
            func_ctor,
            None,
        )?
        .into();

        ctx.bind_ctor_proto_ex(ctor, proto.into(), 0, JS_PROP_CONFIGURABLE)?;
        Ok(Some(proto))
    }

    /// Wrapper to run async function, returns a Promise.
    pub fn class_call(
        ctx: Context,
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
            .call_async(ctx.into(), this_obj, args, flags)
            .map(|p| p.into())
    }
}

impl BCWrapper {
    /// run bytecode in async manner
    fn execute_async(
        &self,
        mut ctx: ContextPtr,
        this_obj: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> Result<Promise, i32> {
        let (promise, resolving_funcs) = Promise::create(&mut ctx)?;
        let mut a_state = AsyncState::create(ctx, resolving_funcs, *self, this_obj, args)?;

        // protect promise and a_state before calling .execute()
        let rt = ctx.rt_ptr();
        let _ng = rt
            .gc_heap()
            .protect_nodes(&[promise.gc_node_ptr(), a_state.gc_node_ptr()]);

        a_state.execute(ctx);
        Ok(promise)
    }
}

impl JSValue {
    /// if value is function object, call it in async manner
    pub(crate) fn call_async(
        &self,
        mut ctx: ContextPtr,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> Result<Promise, i32> {
        if let Some(bc) = self.as_object_BCWrapper() {
            bc.execute_async(ctx, this_obj, args, flags)
        } else {
            ctx.throw_type_error("not bytecode function");
            Err(-1)
        }
    }
}

impl AsyncState {
    pub(crate) fn class_call(
        ctx: Context,
        func_obj: JSValue,
        _this: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut o = unsafe { *func_obj.as_object_unchecked() };
        let is_reject = o.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;

        let a_state = o.payload_mut().async_function_resolving_mut().unwrap();
        a_state.set_throw_flag(is_reject);

        if is_reject {
            ctx.throw(if args.is_empty() {
                JSValue::UNDEFINED
            } else {
                args[0]
            });
        } else {
            // return value of await
            debug_assert!(a_state.stack_frame.cur_sp_offset > 0);
            a_state.set_return_value(if args.is_empty() {
                JSValue::UNDEFINED
            } else {
                args[0]
            });
        }

        a_state.execute(ctx.into());

        Ok(JSValue::UNDEFINED)
    }

    fn execute(&mut self, mut ctx: ContextPtr) {
        let ret = self.resume(ctx);

        if self.is_completed {
            match ret {
                Ok(val) => {
                    // Normal function return
                    let func = self.resolving_funcs[0]; // resolve 
                    let _ = func.call(&mut ctx, JSValue::UNDEFINED, std::slice::from_ref(&val));
                }
                Err(_) => {
                    let func = self.resolving_funcs[1]; // reject 
                    let exc = ctx.rt_mut().take_exception();
                    let _ = func.call(&mut ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc));
                }
            }
        } else {
            // Retrieve value at stack top
            let value = unsafe {
                std::ptr::replace(
                    self.stack_frame.stack_cur_ptr().offset(-1).as_ptr(), // SP[-1]
                    JSValue::UNDEFINED,
                )
            };

            // Handle await expression
            let mut promise = if let Ok(v) = js_promise_resolve(CallContext::new_simple(
                ctx.gc_ref().into(),
                ctx.promise_ctor,
                std::slice::from_ref(&value),
            )) {
                Promise::try_from(v).unwrap()
            } else {
                let exc = ctx.rt_mut().take_exception();
                let rej = self.resolving_funcs[1];
                let _ = rej.call(&mut ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc)); // TODO what to do if exception?
                return;
            };

            let resolving_funcs = if let Ok(rr) = self.async_function_resolve_create(&mut ctx) {
                rr
            } else {
                let exc = ctx.rt_mut().take_exception();
                let func = self.resolving_funcs[1];
                let _ = func.call(&mut ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc));
                [const { JSValue::UNDEFINED }; 2]
            };

            let res = promise.reaction(
                &mut ctx,
                resolving_funcs,
                // no need to create 'thrownawayCapability' as in the spec
                [const { JSValue::UNDEFINED }; 2],
            );

            if res.is_err() {
                let exc = ctx.rt_mut().take_exception();
                let _ = self.resolving_funcs[1].call(
                    &mut ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&exc),
                );
            }
        }
    }

    fn async_function_resolve_create(&self, ctx: &mut ContextImpl) -> Result<[JSValue; 2], i32> {
        let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];
        let state = AsyncState::from(self.gc_ref());

        for i in 0..2 {
            let func_obj = ctx
                .create_object_with_proto(Some(ctx.function_proto()), {
                    if i == 0 {
                        ObjectPayload::ASYNC_FUNCTION_RESOLVE(state)
                    } else {
                        ObjectPayload::ASYNC_FUNCTION_REJECT(state)
                    }
                })
                .map_err(|_| -1)?;

            resolving_funcs[i] = func_obj.into();
        }

        Ok(resolving_funcs)
    }
}

pub const js_async_function_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "AsyncFunction",
        JS_PROP_CONFIGURABLE,
    ),
];
