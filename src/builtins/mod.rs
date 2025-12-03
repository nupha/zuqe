pub mod array;
pub mod array_buffer;
pub mod boolean;
pub mod date;
pub mod error;
pub mod function;
pub mod generator;
pub mod global;
pub mod iterator;
pub mod json;
pub mod mapset;
pub mod math;
pub mod number;
pub mod object;
pub mod promise;
pub mod proxy;
pub mod reflect;
pub mod regexp;
pub mod string;
pub mod symbol;
pub mod typed_array;

use crate::{error::JSResult, function::CallContext};

pub fn js_get_this(cc: CallContext) -> JSResult {
    Ok(*cc.this())
}
