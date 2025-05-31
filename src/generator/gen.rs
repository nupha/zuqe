use crate::{
    context::JSContext,
    function::{async_func_free, async_func_init, async_func_resume, JSAsyncFunctionState},
    gc::{GCMarkFunc, JS_MarkFunc},
    mem::js_malloc_typed,
    runtime::JSRuntime,
    value::{object::JSObjectIsa, JSValue, JSValueConst, JS_FreeValue},
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

impl JSGeneratorData {
    pub fn free_generator_stack_rt(&mut self, rt: &mut JSRuntime) {
        if self.state != JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED {
            if !self.func_state.is_null() {
                unsafe {
                    async_func_free(rt, (*self).func_state);
                }
                self.func_state = std::ptr::null_mut();
            }
            self.state = JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED;
        }
    }
}

#[deprecated(note = "use JSGeneratorData::free_generator_stack_rt() instead")]
pub unsafe fn free_generator_stack_rt(rt: *mut JSRuntime, s: *mut JSGeneratorData) {
    (*s).free_generator_stack_rt(rt.as_mut().unwrap());
}

pub unsafe fn js_generator_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let func_state = async_func_init(ctx, func_obj, this_obj, argc, argv);
    if func_state.is_null() {
        return JSValue::EXCEPTION;
    }

    let mut s = if let Some(p) = js_malloc_typed::<JSGeneratorData>(ctx, 1, true) {
        p.write(JSGeneratorData {
            state: JSGeneratorStateEnum::JS_GENERATOR_STATE_SUSPENDED_START,
            func_state,
        });
        Box::from_raw(p.as_ptr())
    } else {
        return JSValue::EXCEPTION;
    };

    // execute the function up to 'OP_initial_yield'
    let func_ret = async_func_resume(ctx, s.func_state);
    if func_ret.is_exception() {
        s.free_generator_stack_rt(ctx.rt_mut());
        return JSValue::EXCEPTION;
    }
    JS_FreeValue(ctx, func_ret);

    // create js object of generator class
    match ctx.create_instance_from_ctor(func_obj, JSObjectIsa::GENERATOR(Box::into_raw(s))) {
        Ok(obj) => obj,
        Err(isa) => {
            if let JSObjectIsa::GENERATOR(s) = isa {
                let mut s = Box::from_raw(s);
                s.free_generator_stack_rt(ctx.rt_mut());
            } else {
                #[cfg(debug_assertions)]
                unreachable!()
            }
            JSValue::EXCEPTION
        }
    }
}

pub unsafe fn js_generator_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_generator_mark(rt: *mut JSRuntime, val: JSValue, mark_func: GCMarkFunc) {
    unimplemented!();
}
