use crate::{
    JS_CFUNC_NATIVE_DEF, JS_OBJECT_DEF, JS_PROP_STRING_DEF,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, JS_CallConstructor2, build_arg_list, js_function_apply},
    iterator::JSIteratorKindEnum,
    js_core::object::{
        js_object_defineProperty, js_object_getOwnPropertyDescriptor, js_object_getPrototypeOf,
        js_object_isExtensible, js_object_preventExtensions,
    },
    object::{JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
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
    let new_target = if cc.length() > 2 {
        let target = cc.arg(2);
        if !target.is_constructor() {
            ctx.throw_type_error("not a constructor");
            return Err(-1);
        }
        target
    } else {
        func
    };

    let args = build_arg_list(ctx, array_arg)?;
    JS_CallConstructor2(ctx, func, new_target.clone(), &args).err()
}

pub fn js_reflect_deleteProperty(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(obj) = cc.arg(0) {
        let atom = cc.arg(1).to_atom(ctx)?;
        let ret = obj.delete_property(ctx, atom.as_ref(), 0);
        atom.free(ctx.rt_mut());
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
        let ret = obj.get_property_internal(ctx, atom.as_ref(), receiver, false)?;
        atom.free(ctx.rt_mut());
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
        let ret = obj.has_property(ctx, atom.as_ref());
        atom.free(ctx.rt_mut());
        ret.map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_reflect_set(ctx: &JSContext, cc: CallContext) -> JSResult {
    let target = cc.arg(0);

    if let Some(o) = target.as_object() {
        let prop = cc.arg(1);
        let val = cc.arg(2);
        let receiver = if cc.length() > 3 { cc.arg(3) } else { target };
        let atom = prop.to_atom(ctx)?;

        let ret = o.set_property_internal(
            ctx,
            atom.as_ref(),
            val.clone(),
            match receiver {
                JSValue::NULL | JSValue::UNDEFINED => None,
                JSValue::Object(o) => Some(o),
                _ => {
                    // returns false instead of throwing an error
                    return Ok(JSValue::Bool(false));
                }
            },
            0,
        );
        atom.free(ctx.rt_mut());
        ret.map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_reflect_setPrototypeOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    cc.arg(0)
        .set_prototype_internal(ctx, cc.arg(1).clone(), false)
        .map(JSValue::Bool)
}

pub fn js_reflect_ownKeys(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    if val.is_object() {
        val.build_own_property_kvs_array(
            ctx,
            JS_GPN_INDEX_MASK | JS_GPN_SYMBOL_MASK,
            JSIteratorKindEnum::JS_ITERATOR_KIND_KEY,
        )
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub static js_reflect_funcs: [FieldDef; 14] = [
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

pub static js_reflect_obj: [FieldDef; 1] = [JS_OBJECT_DEF!(
    "Reflect",
    &js_reflect_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE
)];
