use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT, JS_PROP_STRING_DEF,
    class::ClassID,
    context::JSContext,
    error::JSResult,
    function::{
        AsyncFuncState, BCFunction, BytecodeFunction, JSFunctionKindEnum, NativeFunctionType,
    },
    object::{JS_PROP_CONFIGURABLE, JSObject, ObjectPayload},
    object_cast,
    promise::{Promise, js_promise_resolve},
    value::JSValue,
};

use super::{CallContext, FieldDef};

/// Async Function object
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct AsyncFunction<'a>(pub(crate) JSObject<'a>);

object_cast!(AsyncFunction, JS_CLASS_ASYNC_FUNCTION);

impl<'a> AsyncFunction<'a> {
    pub fn class_initialize<'b>(
        ctx: &JSContext<'b>,
        _: ClassID,
    ) -> Result<Option<JSObject<'b>>, i32> {
        let proto = ctx.create_base_object_with_proto(Some(ctx.function_proto()))?;
        proto.install_field_defs(ctx, &js_async_function_proto_funcs)?;

        let ctor: JSValue = ctx
            .create_native_function_with_proto(
                NativeFunctionType::ConstructorOrFunc(BytecodeFunction::class_constructor),
                "AsyncFunction",
                1,
                JSFunctionKindEnum::JS_FUNC_ASYNC as i16,
                ctx.0.borrow().function_ctor.as_object().cloned(),
            )?
            .into();

        ctx.bind_ctor_proto_ex(&ctor, &proto.clone().into(), 0, JS_PROP_CONFIGURABLE)?;
        Ok(Some(proto))
    }

    /// hook to run function in async manner, returns a Promise.
    pub fn class_call<'b>(
        ctx: &JSContext<'b>,
        func_obj: &JSValue<'b>,
        this_obj: &JSValue<'b>,
        args: &[JSValue<'b>],
        flags: i32,
    ) -> JSResult<'b> {
        debug_assert_eq!(
            func_obj.as_object().unwrap().class_id(),
            JS_CLASS_ASYNC_FUNCTION
        );

        // create promise and resolving handlers
        func_obj
            .call_async(ctx, this_obj, args, flags)
            .map(|p| p.into())
    }
}

impl<'a> BCFunction<'a> {
    /// call a bc function in async manner
    pub fn call_async(
        &self,
        ctx: &JSContext<'a>,
        this_obj: &JSValue<'a>,
        args: &[JSValue<'a>],
        _flags: i32,
    ) -> Result<Promise<'a>, i32> {
        let (promise, resolving_funcs) = Promise::create(ctx)?;
        async_function_resume(
            ctx,
            &AsyncFuncState::new_with_resolvings(
                ctx,
                resolving_funcs,
                self.clone(),
                this_obj.clone(),
                args,
            )?,
        );
        Ok(promise)
    }
}

impl<'a> JSObject<'a> {
    /// call a bc function object in async manner
    pub fn call_async(
        &self,
        ctx: &JSContext<'a>,
        this_obj: &JSValue<'a>,
        args: &[JSValue<'a>],
        flags: i32,
    ) -> Result<Promise<'a>, i32> {
        if let Some(bcf) = self.as_BCFunction() {
            bcf.call_async(ctx, this_obj, args, flags)
        } else {
            ctx.throw_type_error("not a function with bytecode");
            Err(-1)
        }
    }
}

impl<'a> JSValue<'a> {
    /// call a bc function object in async manner
    pub fn call_async(
        &self,
        ctx: &JSContext<'a>,
        this_obj: &JSValue<'a>,
        args: &[JSValue<'a>],
        flags: i32,
    ) -> Result<Promise<'a>, i32> {
        if let Some(f) = self.as_object_BCFunction() {
            f.call_async(ctx, this_obj, args, flags)
        } else {
            ctx.throw_type_error("not a function with bytecode");
            Err(-1)
        }
    }
}

/// Resume async function execution with an given state
fn async_function_resume<'a>(ctx: &JSContext<'a>, s: &AsyncFuncState<'a>) {
    let func_ret = s.resume(ctx);

    if s.is_completed() {
        match func_ret {
            Ok(val) => {
                // normal return
                let func = s.0.borrow().resolving_funcs[0].clone(); // resolve: short borrow
                let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&val));
            }
            Err(_) => {
                let func = s.0.borrow().resolving_funcs[1].clone(); // reject: short borrow
                let error = ctx.take_exception();
                let _ = func.call(ctx, &JSValue::UNDEFINED, &[error]);
            }
        }
    } else {
        let value = unsafe {
            let cur_sp = s.0.borrow().frame.stack_cur_ptr();
            std::ptr::replace(cur_sp.offset(-1).as_ptr(), JSValue::UNDEFINED)
        };

        // await
        let promise = if let Ok(p) = {
            let ctor = ctx.0.borrow().promise_ctor.clone();
            let cc = CallContext::new_simple(ctx, ctor, unsafe {
                std::mem::transmute(std::slice::from_ref(&value))
            });
            js_promise_resolve(ctx, cc)
        } {
            Promise::try_from(p).unwrap()
        } else {
            let exc = ctx.take_exception();
            let func = s.0.borrow().resolving_funcs[1].clone();
            let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&exc)); // TODO what to do if exception?
            return;
        };

        let resolving_funcs = if let Ok(rr) = js_async_function_resolve_create(ctx, s) {
            rr
        } else {
            let error = ctx.take_exception();
            let func = s.0.borrow().resolving_funcs[1].clone();
            let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&error));
            [const { JSValue::UNDEFINED }; 2]
        };

        let res = promise.reaction(
            ctx,
            &resolving_funcs,
            // no need to create 'thrownawayCapability' as in the spec
            &[const { JSValue::UNDEFINED }; 2],
        );

        if res.is_err() {
            let exc = ctx.take_exception();
            let func = &s.0.borrow().resolving_funcs[1].clone();
            let _ = func.call(ctx, &JSValue::UNDEFINED, &[exc]);
        }
    }
}

fn js_async_function_resolve_create<'a>(
    ctx: &JSContext<'a>,
    s: &AsyncFuncState<'a>,
) -> Result<[JSValue<'a>; 2], i32> {
    let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];

    for i in 0..2 {
        let func_obj = ctx
            .create_object_with_proto(Some(ctx.function_proto()), {
                if i == 0 {
                    ObjectPayload::ASYNC_FUNCTION_RESOLVE(s.clone())
                } else {
                    ObjectPayload::ASYNC_FUNCTION_REJECT(s.clone())
                }
            })
            .map_err(|_| -1)?;

        resolving_funcs[i] = func_obj.into();
    }

    Ok(resolving_funcs)
}

pub(crate) fn js_async_function_resolve_call<'a>(
    ctx: &JSContext<'a>,
    func_obj: &JSValue<'a>,
    _this_obj: &JSValue<'a>,
    args: &[JSValue<'a>],
    _flags: i32,
) -> JSResult<'a> {
    let p = unsafe { func_obj.as_object_unchecked() };
    let is_reject = p.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;

    let async_state: AsyncFuncState<'a> = p.payload().async_function_resolving().cloned().unwrap();
    async_state.set_throw_flag(is_reject);

    if is_reject {
        ctx.throw(if args.is_empty() {
            JSValue::UNDEFINED
        } else {
            args[0].clone()
        });
    } else {
        // return value of await
        debug_assert!(async_state.0.borrow().frame.cur_sp_offset > 0);
        async_state.set_return_value(if args.is_empty() {
            JSValue::UNDEFINED
        } else {
            args[0].clone()
        });
    }

    async_function_resume(ctx, &async_state);
    Ok(JSValue::UNDEFINED)
}

pub static js_async_function_proto_funcs: [FieldDef; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
