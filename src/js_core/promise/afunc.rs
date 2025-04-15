use crate::{
    function::JSCFunctionListEntry, value::object::JS_PROP_CONFIGURABLE, JS_PROP_STRING_DEF,
};

pub const js_async_function_proto_funcs: [JSCFunctionListEntry; 1] = [JS_PROP_STRING_DEF!(
    c"[Symbol.toStringTag]",
    c"AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
