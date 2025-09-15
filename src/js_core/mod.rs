pub mod boolean;
pub mod error;
pub mod function;
pub mod global;
pub mod mapset;
pub mod math;
pub mod object;
pub mod string;

use crate::{context::JSContext, error::JSResult, function::CallContext};

pub fn js_get_this(_: &JSContext, cc: CallContext) -> JSResult {
    Ok(cc.this().clone())
}
