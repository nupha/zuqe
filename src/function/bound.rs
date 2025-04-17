use crate::{
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_free_rt,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst, JS_FreeValueRT, JS_VALUE_GET_OBJ},
};

pub struct JSBoundFunction {
    pub func_obj: JSValue,
    pub this_val: JSValue,
    pub argc: i32,

    // TODO: use a vector instead
    pub argv: [JSValue; 0],
}

pub unsafe fn js_bound_function_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = JS_VALUE_GET_OBJ(val);
    let bf: *mut JSBoundFunction = (*p).u.bound_function;

    JS_FreeValueRT(rt, (*bf).func_obj);
    JS_FreeValueRT(rt, (*bf).this_val);
    for i in 0..(*bf).argc {
        JS_FreeValueRT(rt, *(*bf).argv.as_ptr().offset(i as isize));
    }
    js_free_rt(rt, bf.cast());
}

pub unsafe fn js_bound_function_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    let p = JS_VALUE_GET_OBJ(val);
    let bf: *mut JSBoundFunction = (*p).u.bound_function;

    JS_MarkValue(rt, (*bf).func_obj, mark_func);
    JS_MarkValue(rt, (*bf).this_val, mark_func);
    for i in 0..(*bf).argc {
        JS_MarkValue(rt, *(*bf).argv.as_ptr().offset(i as isize), mark_func);
    }
}
