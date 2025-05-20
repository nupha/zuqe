use std::ffi::c_void;

use crate::{
    atom::JSAtom,
    class::js_instantiate_prototype,
    context::{JSContext, JS_FreeContext},
    function::JS_InstantiateFunctionListItem2,
    module::js_module_ns_autoinit,
    runtime::JSRuntime,
    value::JSValue,
};

use super::{JSObject, JSProperty};

/* return the value associated to the autoinit property or an exception */
pub type JSAutoInitFunc =
    unsafe fn(ctx: *mut JSContext, p: *mut JSObject, atom: JSAtom, opaque: *mut c_void) -> JSValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSAutoInitIDEnum {
    JS_AUTOINIT_ID_PROTOTYPE,
    JS_AUTOINIT_ID_MODULE_NS,
    JS_AUTOINIT_ID_PROP,
}

#[inline(always)]
pub const unsafe fn js_autoinit_get_id(pr: *const JSProperty) -> i32 {
    ((*pr).u.init.realm_and_id & 0b11) as _
}

#[inline(always)]
pub const unsafe fn js_autoinit_get_realm(pr: *const JSProperty) -> *mut JSContext {
    ((*pr).u.init.realm_and_id & !0b11) as *mut JSContext
}

#[inline(always)]
pub unsafe fn js_autoinit_free(rt: *const JSRuntime, pr: *const JSProperty) {
    JS_FreeContext(js_autoinit_get_realm(pr));
}

pub static js_autoinit_func_table: &[JSAutoInitFunc] = &[
    js_instantiate_prototype,        /* JS_AUTOINIT_ID_PROTOTYPE */
    js_module_ns_autoinit,           /* JS_AUTOINIT_ID_MODULE_NS */
    JS_InstantiateFunctionListItem2, /* JS_AUTOINIT_ID_PROP */
];
