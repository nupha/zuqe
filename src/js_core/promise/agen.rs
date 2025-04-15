use crate::{
    context::JSContext,
    function::JSCFunctionListEntry,
    generator::js_async_generator_next,
    iterator::{
        js_async_from_sync_iterator_next, GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW,
    },
    value::{object::JS_PROP_CONFIGURABLE, JSValue, JSValueConst},
    JS_CFUNC_DEF, JS_CFUNC_MAGIC_DEF, JS_PROP_STRING_DEF,
};

pub unsafe fn js_iterator_proto_iterator(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    this_val.dup()
}

pub const js_async_iterator_proto_funcs: [JSCFunctionListEntry; 1] = [JS_CFUNC_DEF!(
    c"[Symbol.asyncIterator]",
    0,
    js_iterator_proto_iterator
)];

pub const js_async_from_sync_iterator_proto_funcs: [JSCFunctionListEntry; 3] = [
    JS_CFUNC_MAGIC_DEF!(
        c"next",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_NEXT as _
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"return",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_RETURN as _
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"throw",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_THROW as _
    ),
];

pub const js_async_generator_proto_funcs: [JSCFunctionListEntry; 4] = [
    JS_CFUNC_MAGIC_DEF!(c"next", 1, js_async_generator_next, GEN_MAGIC_NEXT as _),
    JS_CFUNC_MAGIC_DEF!(c"return", 1, js_async_generator_next, GEN_MAGIC_RETURN as _),
    JS_CFUNC_MAGIC_DEF!(c"throw", 1, js_async_generator_next, GEN_MAGIC_THROW as _),
    JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"AsyncGenerator",
        JS_PROP_CONFIGURABLE
    ),
];

pub const js_async_generator_function_proto_funcs: [JSCFunctionListEntry; 1] =
    [JS_PROP_STRING_DEF!(
        c"[Symbol.toStringTag]",
        c"AsyncGeneratorFunction",
        JS_PROP_CONFIGURABLE
    )];
