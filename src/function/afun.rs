use crate::{
    JS_CLASS_ASYNC_FUNCTION, JS_CLASS_ASYNC_FUNCTION_REJECT, JS_PROP_STRING_DEF,
    context::JSContext,
    error::JSResult,
    function::AsyncState,
    object::{JS_PROP_CONFIGURABLE, JSObject, JSObjectIsa},
    promise::{PromiseValue, js_promise_resolve},
    utils::UncheckedInto,
    value::JSValue,
};

use super::{CallContext, JS_Call, NativeFieldDef};

/// An async function object
#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct AsyncFunction(pub(crate) JSObject);

impl TryFrom<JSObject> for AsyncFunction {
    type Error = i32;

    fn try_from(obj: JSObject) -> Result<Self, Self::Error> {
        if obj.class_id() == JS_CLASS_ASYNC_FUNCTION {
            Ok(Self(obj))
        } else {
            Err(-1)
        }
    }
}

impl TryFrom<JSValue> for AsyncFunction {
    type Error = i32;

    fn try_from(value: JSValue) -> Result<Self, Self::Error> {
        if let JSValue::Object(o) = value {
            o.try_into()
        } else {
            Err(-1)
        }
    }
}

impl From<AsyncFunction> for JSObject {
    #[inline(always)]
    fn from(value: AsyncFunction) -> Self {
        value.0
    }
}

impl From<AsyncFunction> for JSValue {
    #[inline(always)]
    fn from(value: AsyncFunction) -> Self {
        Self::Object(value.0)
    }
}

impl AsyncFunction {
    /// call async function
    pub fn call(
        &self,
        ctx: &JSContext,
        this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> Result<PromiseValue, i32> {
        // create promise and resolving handlers
        let (promise, resolving_funcs) = ctx.create_promise(&JSValue::UNDEFINED)?;

        // create state for async function execution
        let state = AsyncState::new_with_resolvings(
            ctx,
            resolving_funcs,
            &JSValue::Object(self.0.clone()),
            this_obj,
            args,
        )?;

        // start to execute async func
        async_function_resume(ctx, &state);

        Ok(promise)
    }

    /// hook to call an async function
    #[inline(always)]
    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        this_obj: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let afunc: AsyncFunction = func_obj.clone().try_into()?;
        afunc
            .call(ctx, this_obj.clone(), args, flags)
            .map(|p| p.into())
    }
}

/// Resume an async function on its execution state
fn async_function_resume(ctx: &JSContext, state: &AsyncState) {
    let func_ret = state.resume(ctx);

    if state.is_completed() {
        match func_ret {
            Ok(val) => {
                // normal return
                let func = state.0.borrow().resolving_funcs[0].clone(); // resolve: short borrow
                JS_Call(ctx, &func, &JSValue::UNDEFINED, std::slice::from_ref(&val));
            }
            Err(_) => {
                let func = state.0.borrow().resolving_funcs[1].clone(); // reject: short borrow
                let error = ctx.take_exception();
                JS_Call(ctx, &func, &JSValue::UNDEFINED, &[error]);
            }
        }
    } else {
        let cur_sp = state.0.borrow().frame.stack_cur_ptr();
        let value = unsafe { std::ptr::replace(cur_sp.offset(-1).as_ptr(), JSValue::UNDEFINED) };
        let promise_ctor = ctx.0.borrow().promise_ctor.clone();

        let promise = {
            let cc = CallContext::new(ctx, promise_ctor, std::slice::from_ref(&value), 0, None);
            js_promise_resolve(ctx, cc).unwrap_or(JSValue::EXCEPTION)
        };
        if promise.is_exception() {
            let error = ctx.take_exception();
            JS_Call(
                ctx,
                &state.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                std::slice::from_ref(&error),
            );
        }

        let resolving_funcs =
            if let Ok(rr) = unsafe { js_async_function_resolve_create(ctx, state) } {
                rr
            } else {
                let error = ctx.take_exception();
                JS_Call(
                    ctx,
                    &state.0.borrow().resolving_funcs[1],
                    &JSValue::UNDEFINED,
                    &[error],
                );
                [const { JSValue::UNDEFINED }; 2]
            };

        let res = unsafe {
            UncheckedInto::<PromiseValue>::unchecked_into(promise).reaction(
                ctx,
                &resolving_funcs,
                // no need to create 'thrownawayCapability' as in the spec
                &[const { JSValue::UNDEFINED }; 2],
            )
        };

        if res.is_err() {
            let error = ctx.take_exception();
            JS_Call(
                ctx,
                &state.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                &[error],
            );
        }
    }
}

pub unsafe fn js_async_function_resolve_create(
    ctx: &JSContext,
    s: &AsyncState,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [const { JSValue::UNDEFINED }; 2];

    for i in 0..2 {
        let func_obj = ctx
            .create_instance_from_proto(&ctx.0.borrow().function_proto, {
                if i == 0 {
                    JSObjectIsa::ASYNC_FUNCTION_RESOLVE(s.clone())
                } else {
                    JSObjectIsa::ASYNC_FUNCTION_REJECT(s.clone())
                }
            })
            .map_err(|_| -1)?;

        resolving_funcs[i] = func_obj;
    }

    return Ok(resolving_funcs);
}

pub fn js_async_function_resolve_call(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    let p = unsafe { func_obj.object_unchecked() };
    let s = p.0.isa.borrow().async_func_state().cloned().unwrap();
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

pub const js_async_function_proto_funcs: [NativeFieldDef; 1] = [JS_PROP_STRING_DEF!(
    "[Symbol.toStringTag]",
    "AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
