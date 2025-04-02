use crate::{
    context::JSContext,
    gc::JS_MarkFunc,
    runtime::JSRuntime,
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

pub unsafe fn js_promise_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_promise_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    unimplemented!();
}

pub unsafe fn js_promise_resolve_function_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_promise_resolve_function_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    unimplemented!();
}
