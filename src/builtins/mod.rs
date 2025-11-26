pub mod array;
pub mod array_buffer;
pub mod boolean;
pub mod date;
pub mod error;
pub mod function;
pub mod global;
pub mod json;
pub mod mapset;
pub mod math;
pub mod object;
pub mod promise;
pub mod proxy;
pub mod reflect;
pub mod regexp;
pub mod string;
pub mod typed_array;

use crate::{context::ContextRef, error::JSResult, function::CallContext};

pub fn js_get_this(_: ContextRef, cc: CallContext) -> JSResult {
    Ok(*cc.this())
}
