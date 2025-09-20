use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT, JS_PROP_STRING_DEF,
    context::JSContext,
    error::JSResult,
    function::AsyncFuncState,
    object::{JS_PROP_CONFIGURABLE, JSObject, ObjectPayload},
    object_cast,
    promise::{Promise, js_promise_resolve},
    value::JSValue,
};

use super::{CallContext, FieldDef};

/// An async function object
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct AsyncFunction(pub(crate) JSObject);

object_cast!(AsyncFunction, JS_CLASS_ASYNC_FUNCTION);

impl AsyncFunction {
    /// hook to call an async function class object
    pub fn class_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        debug_assert!(func_obj.as_object_AsyncFunction().is_some());

        // create promise and resolving handlers
        let (promise, resolving_funcs) = ctx.create_promise(&JSValue::UNDEFINED)?;

        // async state is on stack
        let s = AsyncFuncState::new_with_resolvings(
            ctx,
            resolving_funcs,
            func_obj,
            this_obj.clone(),
            args,
        )?;
        async_function_resume(ctx, &s);

        Ok(promise.into())
    }
}

/// Resume async function execution with an given state
fn async_function_resume(ctx: &JSContext, s: &AsyncFuncState) {
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
            let cc = CallContext::new_simple(ctx, ctor, std::slice::from_ref(&value));
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

fn js_async_function_resolve_create(
    ctx: &JSContext,
    s: &AsyncFuncState,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];

    for i in 0..2 {
        let func_obj = ctx
            .create_object_from_proto(ctx.0.borrow().function_proto.clone(), {
                if i == 0 {
                    ObjectPayload::ASYNC_FUNCTION_RESOLVE(s.clone())
                } else {
                    ObjectPayload::ASYNC_FUNCTION_REJECT(s.clone())
                }
            })
            .map_err(|_| -1)?;

        resolving_funcs[i] = func_obj.into();
    }

    return Ok(resolving_funcs);
}

pub(crate) fn js_async_function_resolve_call(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    let p = unsafe { func_obj.as_object_unchecked() };
    let s = p.0.payload.borrow().async_func_state().cloned().unwrap();
    let is_reject = p.class_id() == JS_CLASS_ASYNC_FUNCTION_REJECT;

    let arg = if args.len() > 0 {
        args[0].clone()
    } else {
        JSValue::UNDEFINED
    };

    unsafe {
        s.0.borrow_mut().as_mut().get_unchecked_mut().throw_flag = is_reject;
    }

    if is_reject {
        ctx.throw(arg);
    } else {
        // return value of await
        debug_assert!(s.0.borrow().frame.cur_sp_offset > 0);

        let mut s = s.0.borrow_mut();
        let s = unsafe { s.as_mut().get_unchecked_mut() };
        unsafe {
            *s.frame
                .stack_base_ptr()
                .as_ptr()
                .add(s.frame.cur_sp_offset as usize - 1) = arg;
        }
    }

    async_function_resume(ctx, &s);
    Ok(JSValue::UNDEFINED)
}

pub static js_async_function_proto_funcs: [FieldDef; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
