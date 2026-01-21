pub mod array;
pub mod array_buffer;
pub mod boolean;
pub mod error;
pub mod function;
pub mod generator;
pub mod global;
pub mod iterator;
pub mod number;
pub mod object;
pub mod promise;
pub mod string;
pub mod typed_array;

#[cfg(feature = "builtin_date")]
pub mod date;
#[cfg(feature = "builtin_json")]
pub mod json;
#[cfg(feature = "builtin_mapset")]
pub mod mapset;
#[cfg(feature = "builtin_math")]
pub mod math;
#[cfg(feature = "builtin_proxy")]
pub mod proxy;
#[cfg(feature = "builtin_reflect")]
pub mod reflect;
#[cfg(feature = "builtin_regexp")]
pub mod regexp;
#[cfg(feature = "builtin_symbol")]
pub mod symbol;

use crate::{error::JSResult, function::CallContext};

pub fn js_get_this(cc: CallContext) -> JSResult {
    Ok(*cc.this())
}
