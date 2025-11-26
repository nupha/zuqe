pub mod boolean;
pub mod error;
pub mod function;
pub mod global;
pub mod math;
pub mod object;
pub mod string;

use crate::{context::ContextRef, error::JSResult, function::CallContext};

pub fn js_get_this(_: ContextRef, cc: CallContext) -> JSResult {
    Ok(*cc.this())
}
