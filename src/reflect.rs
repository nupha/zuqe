use crate::{
    JS_CFUNC_NATIVE_DEF, JS_OBJECT_DEF, JS_PROP_STRING_DEF,
    atom::JS_FreeAtom,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_CallConstructor2, NativeField, build_arg_list, js_function_apply},
    iterator::JSIteratorKindEnum,
    js_core::object::{
        js_object_defineProperty, js_object_getOwnPropertyDescriptor, js_object_getPrototypeOf,
        js_object_isExtensible, js_object_preventExtensions,
    },
    object::{
        JS_GPN_STRING_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE,
        JS_SetPrototypeInternal,
    },
    value::JSValue,
};

pub fn js_reflect_apply(ctx: &JSContext, cc: CallContext) -> JSResult {
    let c = CallContext::new(
        ctx,
        cc.arg(0).clone(),
        if cc.length() == 0 {
            &[]
        } else {
            &cc.arguments()[1..]
        },
        2,
        None,
    );

    js_function_apply(ctx, c)
}

pub fn js_reflect_construct(ctx: &JSContext, cc: CallContext) -> JSResult {
    let func = cc.arg(0);
    let array_arg = cc.arg(1);
    let mut new_target = func;

    if cc.length() > 2 {
        new_target = cc.arg(2);
        if !new_target.is_constructor() {
            ctx.throw_type_error("not a constructor");
            return Err(-1);
        }
    }

    let args = build_arg_list(ctx, array_arg)?;
    JS_CallConstructor2(ctx, func, new_target.clone(), &args).check_exception()
}

pub fn js_reflect_deleteProperty(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(obj) = cc.arg(0) {
        let atom = cc.arg(1).to_atom(ctx)?;
        let ret = obj.delete_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        ret.map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_reflect_get(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    if obj.is_object() {
        let prop = cc.arg(1);
        let receiver = if cc.length() > 2 { cc.arg(2) } else { obj };
        let atom = prop.to_atom(ctx)?;
        let ret = obj.get_property_internal(ctx, atom, receiver, false)?;

        JS_FreeAtom(ctx, atom);
        Ok(ret)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_reflect_has(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(obj) = cc.arg(0) {
        let prop = cc.arg(1);
        let atom = prop.to_atom(ctx)?;
        let ret = obj.has_property(ctx, atom);
        JS_FreeAtom(ctx, atom);
        ret.map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_reflect_set(ctx: &JSContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    if obj.is_object() {
        let prop = cc.arg(1);
        let val = cc.arg(2);
        let receiver = if cc.length() > 3 { cc.arg(3) } else { obj };
        let atom = prop.to_atom(ctx)?;
        let ret = JSValue::set_property_internal(ctx, obj, atom, val.clone(), receiver, 0);
        JS_FreeAtom(ctx, atom);
        ret.map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_reflect_setPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    JS_SetPrototypeInternal(ctx, cc.arg(0), cc.arg(1).clone(), false).map(JSValue::Bool)
}

pub fn js_reflect_ownKeys(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(obj) = cc.arg(0) {
        obj.get_own_property_names_ex(
            ctx,
            JS_GPN_STRING_MASK | JS_GPN_SYMBOL_MASK,
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
        )
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub const js_reflect_funcs: [NativeField; 14] = [
    JS_CFUNC_NATIVE_DEF!("apply", 3, js_reflect_apply, 0),
    JS_CFUNC_NATIVE_DEF!("construct", 2, js_reflect_construct, 0),
    JS_CFUNC_NATIVE_DEF!("defineProperty", 3, js_object_defineProperty, 1),
    JS_CFUNC_NATIVE_DEF!("deleteProperty", 2, js_reflect_deleteProperty, 0),
    JS_CFUNC_NATIVE_DEF!("get", 2, js_reflect_get, 0),
    JS_CFUNC_NATIVE_DEF!(
        "getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        1
    ),
    JS_CFUNC_NATIVE_DEF!("getPrototypeOf", 1, js_object_getPrototypeOf, 1),
    JS_CFUNC_NATIVE_DEF!("has", 2, js_reflect_has, 0),
    JS_CFUNC_NATIVE_DEF!("isExtensible", 1, js_object_isExtensible, 1),
    JS_CFUNC_NATIVE_DEF!("ownKeys", 1, js_reflect_ownKeys, 0),
    JS_CFUNC_NATIVE_DEF!("preventExtensions", 1, js_object_preventExtensions, 1),
    JS_CFUNC_NATIVE_DEF!("set", 3, js_reflect_set, 0),
    JS_CFUNC_NATIVE_DEF!("setPrototypeOf", 2, js_reflect_setPrototypeOf, 0),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Reflect", JS_PROP_CONFIGURABLE),
];

pub const js_reflect_obj: [NativeField; 1] = [JS_OBJECT_DEF!(
    "Reflect",
    &js_reflect_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE
)];
