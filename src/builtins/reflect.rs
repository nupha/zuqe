use crate::{
    builtins::{
        iterator::JSIteratorKindEnum,
        object::{
            js_object_defineProperty, js_object_getOwnPropertyDescriptor, js_object_getPrototypeOf,
            js_object_isExtensible, js_object_preventExtensions,
        },
    },
    error::JSResult,
    function::{CallContext, FieldDef, JS_CallConstructor2, build_arg_list, js_function_apply},
    object::{JS_GPN_INDEX_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
    value::JSValue,
};

fn js_reflect_apply(cc: CallContext) -> JSResult {
    let cc2 = CallContext::new(
        cc.ctx_ptr(),
        *cc.arg(0),
        if cc.length() == 0 {
            &[]
        } else {
            &cc.arguments()[1..]
        },
        2,
        None,
    );
    js_function_apply(cc2)
}

fn js_reflect_construct(cc: CallContext) -> JSResult {
    let func = *cc.arg(0);
    let array_arg = *cc.arg(1);

    let new_target = if cc.length() > 2 {
        let target = cc.arg(2);
        if !target.is_constructor() {
            cc.ctx().throw_type_error("not a constructor");
            return Err(-1);
        }
        *target
    } else {
        func
    };

    let args = build_arg_list(cc.ctx(), array_arg)?;
    JS_CallConstructor2(cc.ctx(), func, new_target, &args).err()
}

fn js_reflect_deleteProperty(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if let JSValue::Object(mut obj) = *cc.arg(0) {
        let atom = cc.arg(1).to_atom(ctx)?;
        let ret = obj.delete_property(ctx, atom, 0);
        ret.map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_reflect_get(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let obj = *cc.arg(0);

    if obj.is_object() {
        let prop = cc.arg(1);
        let receiver = if cc.length() > 2 { *cc.arg(2) } else { obj };
        let atom = prop.to_atom(ctx)?;
        obj.get_property_internal(ctx, atom, receiver, false)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_reflect_has(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    if let JSValue::Object(obj) = cc.arg(0) {
        let prop = cc.arg(1);
        let atom = prop.to_atom(ctx)?;
        obj.has_property(ctx, atom).map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_reflect_set(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
    let target = cc.arg(0);

    if let Some(o) = target.as_object() {
        let prop = *cc.arg(1);
        let val = *cc.arg(2);
        let receiver = if cc.length() > 3 { cc.arg(3) } else { target };
        let atom = prop.to_atom(ctx)?;

        o.set_property_internal(
            ctx,
            atom,
            val,
            match receiver {
                JSValue::Object(o) => Some(*o),
                JSValue::NULL | JSValue::UNDEFINED => None,
                _ => None,
            },
            0,
        )
        .map(JSValue::Bool)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_reflect_setPrototypeOf(cc: CallContext) -> JSResult {
    let mut val = *cc.arg(0);
    val.set_prototype_internal(cc.ctx(), *cc.arg(1), false)
        .map(JSValue::Bool)
}

fn js_reflect_ownKeys(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();
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

const js_reflect_funcs: &[FieldDef] = &[
    FieldDef::CALL("apply", 3, js_reflect_apply, 0),
    FieldDef::CALL("construct", 2, js_reflect_construct, 0),
    FieldDef::CALL("defineProperty", 3, js_object_defineProperty, 1),
    FieldDef::CALL("deleteProperty", 2, js_reflect_deleteProperty, 0),
    FieldDef::CALL("get", 2, js_reflect_get, 0),
    FieldDef::CALL(
        "getOwnPropertyDescriptor",
        2,
        js_object_getOwnPropertyDescriptor,
        1,
    ),
    FieldDef::CALL("getPrototypeOf", 1, js_object_getPrototypeOf, 1),
    FieldDef::CALL("has", 2, js_reflect_has, 0),
    FieldDef::CALL("isExtensible", 1, js_object_isExtensible, 1),
    FieldDef::CALL("ownKeys", 1, js_reflect_ownKeys, 0),
    FieldDef::CALL("preventExtensions", 1, js_object_preventExtensions, 1),
    FieldDef::CALL("set", 3, js_reflect_set, 0),
    FieldDef::CALL("setPrototypeOf", 2, js_reflect_setPrototypeOf, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "Reflect", JS_PROP_CONFIGURABLE),
];

pub const js_reflect_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "Reflect",
    js_reflect_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
