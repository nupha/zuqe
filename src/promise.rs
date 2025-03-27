use crate::{
    context::JSContext,
    types::list_head,
    value::{JSValue, JSValueConst, JS_UNDEFINED},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSPromiseStateEnum {
    JS_PROMISE_PENDING,
    JS_PROMISE_FULFILLED,
    JS_PROMISE_REJECTED,
}

pub struct JSPromiseData {
    promise_state: JSPromiseStateEnum,
    /* 0=fulfill, 1=reject, list of JSPromiseReactionData.link */
    promise_reactions: [list_head; 2],
    is_handled: bool, /* Note: only useful to debug */
    promise_result: JSValue,
}

pub struct JSPromiseFunctionDataResolved {
    ref_count: i32,
    already_resolved: bool,
}

pub struct JSPromiseFunctionData {
    promise: JSValue,
    presolved: *mut JSPromiseFunctionDataResolved,
}

pub unsafe fn JS_NewPromiseCapability(
    ctx: *mut JSContext,
    resolving_funcs: *mut JSValue,
) -> JSValue {
    js_new_promise_capability(ctx, resolving_funcs, JS_UNDEFINED)
}

pub unsafe fn js_new_promise_capability(
    ctx: *mut JSContext,
    resolving_funcs: *mut JSValue,
    ctor: JSValueConst,
) -> JSValue {
    unimplemented!()
}
