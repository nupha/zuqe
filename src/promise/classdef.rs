use crate::{
    function::JSCFunctionListEntry, js_core::js_get_this, value::object::JS_PROP_CONFIGURABLE,
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_CGETSET_DEF, JS_PROP_STRING_DEF,
};

use super::{
    js_promise_all, js_promise_catch, js_promise_finally, js_promise_race, js_promise_resolve,
    js_promise_then, js_promise_withResolvers, PROMISE_MAGIC_all, PROMISE_MAGIC_allSettled,
    PROMISE_MAGIC_any,
};

//
// Promise
//

pub const js_promise_funcs: [JSCFunctionListEntry; 8] = [
    JS_CFUNC_MAGIC_DEF!(c"resolve", 1, js_promise_resolve, 0),
    JS_CFUNC_MAGIC_DEF!(c"reject", 1, js_promise_resolve, 1),
    JS_CFUNC_MAGIC_DEF!(c"all", 1, js_promise_all, PROMISE_MAGIC_all as _),
    JS_CFUNC_MAGIC_DEF!(
        c"allSettled",
        1,
        js_promise_all,
        PROMISE_MAGIC_allSettled as _
    ),
    JS_CFUNC_MAGIC_DEF!(c"any", 1, js_promise_all, PROMISE_MAGIC_any as _),
    JS_CFUNC_DEF!(c"race", 1, js_promise_race),
    JS_CFUNC_DEF!(c"withResolvers", 0, js_promise_withResolvers),
    JS_CGETSET_DEF!(c"[Symbol.species]", js_get_this, NONE),
];

pub const js_promise_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_DEF!(c"then", 2, js_promise_then),
    JS_CFUNC_DEF!(c"catch", 1, js_promise_catch),
    JS_CFUNC_DEF!(c"finally", 1, js_promise_finally),
    JS_PROP_STRING_DEF!(c"[Symbol.toStringTag]", c"Promise", JS_PROP_CONFIGURABLE),
];
