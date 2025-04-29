pub mod boolean;
pub mod error;
pub mod function;
pub mod global;
pub mod mapset;
pub mod object;
pub mod string;

use crate::{context::JSContext, value::JSValue};

pub unsafe fn js_get_this(ctx: *mut JSContext, this_val: JSValue) -> JSValue {
    this_val.dup()
}
