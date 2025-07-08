use crate::{
    context::JSContext,
    error::JSResult,
    function::ObjAsyncState,
    object::JSObjectIsa,
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
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

#[derive(Debug)]
pub struct ObjGenerator {
    pub state: JSGeneratorStateEnum,
    pub func_state: ObjAsyncState,
}

// impl ObjGenerator {
//     pub fn free_generator_stack_rt(&mut self, rt: &mut JSRuntime) {
//         if self.state != JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED {
//             if !self.func_state.is_null() {
//                 ObjAsyncState::free(rt, self.func_state);
//                 self.func_state = std::ptr::null_mut();
//             }
//             self.state = JSGeneratorStateEnum::JS_GENERATOR_STATE_COMPLETED;
//         }
//     }
// }

#[deprecated(note = "use ObjGenerator::free_generator_stack_rt() instead")]
pub unsafe fn free_generator_stack_rt(rt: *mut JSRuntime, s: *mut ObjGenerator) {
    unreachable!("unused");
    // (*s).free_generator_stack_rt(rt.as_mut().unwrap());
}

pub fn js_generator_function_call(
    ctx: &JSContext,
    func_obj: &JSValueConst,
    this_obj: &JSValueConst,
    args: &[JSValue],
    flags: i32,
) -> JSResult {
    let s = ObjGenerator {
        state: JSGeneratorStateEnum::JS_GENERATOR_STATE_SUSPENDED_START,
        func_state: ObjAsyncState::new(ctx, func_obj, this_obj.clone(), args)?,
    };

    // execute the function up to 'OP_initial_yield'
    s.func_state.resume(ctx)?;

    // create js object of generator class
    ctx.create_instance_from_ctor(func_obj, JSObjectIsa::GENERATOR(s))
        .map_err(|_| -1)
}

pub fn js_generator_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {
    unimplemented!();
}

pub fn js_generator_mark(val: JSValue) {
    unimplemented!();
}
