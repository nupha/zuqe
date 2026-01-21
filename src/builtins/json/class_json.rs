use crate::{
    atom::Atom,
    context::ContextImpl,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE,
        PropertyKey,
    },
    runtime::js_check_stack_overflow,
    value::JSValue,
};

fn js_json_stringify(cc: CallContext) -> JSResult {
    cc.ctx().json_stringify(*cc.arg(0), *cc.arg(1), *cc.arg(2))
}

fn internalize_json_property(
    ctx: &ContextImpl,
    holder: JSValue,
    name: Atom,
    reviver: JSValue,
) -> JSResult {
    if js_check_stack_overflow(ctx.rt_ref(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    let mut val = holder.get_property(ctx, name)?;

    if let Some(o) = val.as_object().copied() {
        let is_array = o.is_array(ctx)?;
        let len: u32;
        let pks: Box<[PropertyKey]>;

        if is_array {
            len = o.get_length_prop(ctx)? as u32;
            pks = [].into();
        } else {
            pks = o.get_own_property_names(ctx, JS_GPN_ENUM_ONLY | JS_GPN_INDEX_MASK)?;
            len = pks.len() as u32;
        }

        for i in 0..len {
            let prop = if is_array {
                Atom::from_i64(ctx.rt_ptr(), i as i64)?
            } else {
                unsafe { pks.get_unchecked(i as usize).atom }
            };

            let new_el = internalize_json_property(ctx, val, prop, reviver)?;
            if new_el.is_undefined() {
                val.delete_property(ctx, prop, 0)
            } else {
                val.define_property_value(ctx, prop, new_el, JS_PROP_C_W_E)
            }?;
        }
    }

    reviver.call(ctx, holder, &[name.string_or_symbol(ctx)?, val])
}

fn js_json_parse(cc: CallContext) -> JSResult {
    let ctx = cc.ctx();

    let s = cc.arg(0).to_string(ctx)?;
    let mut obj = ctx.json_parse(&s.content().encode_utf8_bytes(true), "<input>", 0)?;

    if cc.length() > 1 && cc.arg(1).is_function() {
        let reviver = *cc.arg(1);
        let mut root: JSValue = ctx.create_base_object()?.into();
        root.define_property_value(ctx, Atom::CONST_empty_string, obj, JS_PROP_C_W_E)?;
        obj = internalize_json_property(ctx, root, Atom::CONST_empty_string, reviver)?;
    }

    Ok(obj)
}

const js_json_funcs: &[FieldDef] = &[
    FieldDef::CALL("parse", 2, js_json_parse, 0),
    FieldDef::CALL("stringify", 3, js_json_stringify, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "JSON", JS_PROP_CONFIGURABLE),
];

pub const js_json_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "JSON",
    js_json_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
