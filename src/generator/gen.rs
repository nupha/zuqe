use crate::{
    class::js_create_from_ctor,
    context::JSContext,
    function::{async_func_free, async_func_init, async_func_resume, JSAsyncFunctionState},
    gc::JS_MarkFunc,
    mem::{js_free, js_mallocz},
    runtime::JSRuntime,
    value::{object::JS_SetOpaque, JSValue, JSValueConst, JS_FreeValue, JS_IsException},
    JS_CLASS_GENERATOR,
};

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(u8)]
pub enum JSGeneratorStateEnum {
    JS_GENERATOR_STATE_SUSPENDED_START,
    JS_GENERATOR_STATE_SUSPENDED_YIELD,
    JS_GENERATOR_STATE_SUSPENDED_YIELD_STAR,
    JS_GENERATOR_STATE_EXECUTING,
    JS_GENERATOR_STATE_COMPLETED,
}

pub const FUNC_RET_AWAIT: i32 = 0;
pub const FUNC_RET_YIELD: i32 = 1;
pub const FUNC_RET_YIELD_STAR: i32 = 2;
pub const FUNC_RET_INITIAL_YIELD: i32 = 3;

#[repr(C)]
pub struct JSGeneratorData {
    pub state: JSGeneratorStateEnum,
    pub func_state: *mut JSAsyncFunctionState,
}

pub unsafe fn free_generator_stack_rt(rt: *mut JSRuntime, s: *mut JSGeneratorData) {
    if (*s).state == JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED {
        return;
    }
    if (*s).func_state.is_null() == false {
        async_func_free(rt, (*s).func_state);
        (*s).func_state = std::ptr::null_mut();
    }
    (*s).state = JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED;
}

pub unsafe fn js_generator_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    macro_rules! fail {
        ($s:expr) => {
            free_generator_stack_rt(ctx.rt_mut(), $s);
            js_free(ctx, $s.cast());
            return JSValue::EXCEPTION;
        };
    }

    let s = js_mallocz(ctx, std::mem::size_of::<JSGeneratorData>()) as *mut JSGeneratorData;
    if s.is_null() {
        return JSValue::EXCEPTION;
    }
    (*s).state = JSGeneratorStateEnum::JS_GENERATOR_STATE_SUSPENDED_START;
    (*s).func_state = async_func_init(ctx, func_obj, this_obj, argc, argv);

    if (*s).func_state.is_null() {
        (*s).state = JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED;
        fail!(s);
    }

    /* execute the function up to 'OP_initial_yield' */
    let func_ret = async_func_resume(ctx, (*s).func_state);
    if JS_IsException(func_ret) {
        fail!(s);
    }
    JS_FreeValue(ctx, func_ret);

    let obj = js_create_from_ctor(ctx, func_obj, JS_CLASS_GENERATOR);
    if JS_IsException(obj) {
        fail!(s);
    }
    JS_SetOpaque(obj, s.cast());

    obj
}

pub unsafe fn js_generator_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_generator_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    unimplemented!();
}
