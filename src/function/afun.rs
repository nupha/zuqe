use gc_lite::GcRef;

use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT,
    builtins::promise::{Promise, js_promise_resolve},
    class::ClassID,
    context::{Context, ContextImpl, ContextPtr},
    error::JSResult,
    function::{AsyncFuncState, BCFunction, BytecodeFunction, Callback, JSFunctionKindEnum},
    object::{JS_PROP_CONFIGURABLE, ObjectImpl, ObjectPayload, ObjectRef},
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
    pub fn class_initialize(ctx: Context, _: ClassID) -> Result<Option<ObjectRef>, i32> {
        let mut proto = ctx.create_base_object_with_proto(Some(ctx.function_proto()))?;
        proto.install_field_defs(&ctx, &js_async_function_proto_funcs)?;

        let ctor: JSValue = ctx
            .create_native_function_with_proto(
                Callback::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "AsyncFunction",
                1,
                JSFunctionKindEnum::JS_FUNC_ASYNC as i16,
                ctx.function_ctor.as_object().cloned(),
            )?
            .into();

        ctx.bind_ctor_proto_ex(ctor, proto.into(), 0, JS_PROP_CONFIGURABLE)?;
        Ok(Some(proto))
    }

    /// hook to run function in async manner, returns a Promise.
    pub fn class_call(
        mut ctx: Context,
        func_obj: JSValue,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        debug_assert_eq!(
            func_obj.as_object().unwrap().class_id(),
            JS_CLASS_ASYNC_FUNCTION
        );

        // create promise and resolving handlers
        func_obj
            .call_async(&mut ctx, this_obj, args, flags)
            .map(|p| p.into())
    }
}

impl BCFunction {
    /// call bytecode function in async manner
    pub fn call_async(
        &self,
        ctx: &mut ContextImpl,
        this_obj: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> Result<Promise, i32> {
        let (promise, resolving_funcs) = Promise::create(ctx)?;
        let mut s =
            AsyncFuncState::new_with_resolvings(ctx, resolving_funcs, *self, this_obj, args)?;
        s.run_async_function(ctx);

        Ok(promise)
    }
}

impl ObjectRef {
    /// if object is a bytecode function, call it in async manner
    pub fn call_async(
        &self,
        ctx: &mut ContextImpl,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> Result<Promise, i32> {
        if let Some(bcf) = self.as_BCFunction() {
            bcf.call_async(ctx, this_obj, args, flags)
        } else {
            ctx.throw_type_error("not a bytecode function");
            Err(-1)
        }
    }
}

impl JSValue {
    /// if value is a bytecode function, call it in async manner
    pub fn call_async(
        &self,
        ctx: &mut ContextImpl,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> Result<Promise, i32> {
        if let Some(f) = self.as_object_BCFunction() {
            f.call_async(ctx, this_obj, args, flags)
        } else {
            ctx.throw_type_error("not a bytecode function");
            Err(-1)
        }
    }
}

impl AsyncFuncState {
    pub fn class_call(
        mut ctx: Context,
        func_obj: JSValue,
        _this_obj: JSValue,
        args: &[JSValue],
        _flags: i32,
    ) -> JSResult {
        let mut o = unsafe { *func_obj.as_object_unchecked() };
        let is_reject = o.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;

        let async_state = o.payload_mut().async_function_resolving_mut().unwrap();
        async_state.set_throw_flag(is_reject);

        if is_reject {
            ctx.throw(if args.is_empty() {
                JSValue::UNDEFINED
            } else {
                args[0]
            });
        } else {
            // return value of await
            debug_assert!(async_state.frame.cur_sp_offset > 0);
            async_state.set_return_value(if args.is_empty() {
                JSValue::UNDEFINED
            } else {
                args[0]
            });
        }

        async_state.run_async_function(&mut ctx);
        Ok(JSValue::UNDEFINED)
    }

    /// run async function associated with `self` state
    fn run_async_function(&mut self, ctx: &mut ContextImpl) {
        let func_ret = self.resume(ctx);

        if self.is_completed {
            match func_ret {
                Ok(val) => {
                    // normal return
                    let func = self.resolving_funcs[0]; // resolve 
                    let _ = func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&val));
                }
                Err(_) => {
                    let func = self.resolving_funcs[1]; // reject 
                    let error = ctx.rt().take_exception();
                    let _ = func.call(ctx, JSValue::UNDEFINED, &[error]);
                }
            }
        } else {
            let value = unsafe {
                let cur_sp = self.frame.stack_cur_ptr();
                std::ptr::replace(cur_sp.offset(-1).as_ptr(), JSValue::UNDEFINED)
            };

            // await
            let mut promise = if let Ok(p) = {
                let cc = CallContext::new_simple(
                    unsafe { ContextPtr::from_ref_unchecked(ctx) },
                    ctx.promise_ctor,
                    std::slice::from_ref(&value),
                );
                js_promise_resolve(cc)
            } {
                Promise::try_from(p).unwrap()
            } else {
                let exc = ctx.rt().take_exception();
                let func = self.resolving_funcs[1];
                let _ = func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc)); // TODO what to do if exception?
                return;
            };

            let resolving_funcs = if let Ok(rr) = self.async_function_resolve_create(ctx) {
                rr
            } else {
                let error = ctx.rt().take_exception();
                let func = self.resolving_funcs[1];
                let _ = func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&error));
                [const { JSValue::UNDEFINED }; 2]
            };

            let res = promise.reaction(
                ctx,
                resolving_funcs,
                // no need to create 'thrownawayCapability' as in the spec
                [const { JSValue::UNDEFINED }; 2],
            );

            if res.is_err() {
                let exc = ctx.rt().take_exception();
                let _ = self.resolving_funcs[1].call(ctx, JSValue::UNDEFINED, &[exc]);
            }
        }
    }

    fn async_function_resolve_create(&self, ctx: &ContextImpl) -> Result<[JSValue; 2], i32> {
        let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];
        let state = *self;

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

pub static js_async_function_proto_funcs: [FieldDef; 1] = [FieldDef::STR(
    "[Symbol.toStringTag]",
    "AsyncFunction",
    JS_PROP_CONFIGURABLE,
)];
