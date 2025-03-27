use crate::{
    context::JSContext,
    types::list_head,
    value::{JSValue, JSValueConst},
};

pub type JSJobFunc = unsafe fn(ctx: *mut JSContext, argc: i32, argv: *mut JSValueConst) -> JSValue;

#[repr(C)]
pub struct JSJobEntry {
    pub link: list_head,
    pub ctx: *mut JSContext,
    pub job_func: JSJobFunc,
    pub argc: i32,
    pub argv: [JSValue; 0],
}
