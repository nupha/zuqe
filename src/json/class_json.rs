use crate::{
    JS_CFUNC_NATIVE_DEF, JS_OBJECT_DEF, JS_PROP_STRING_DEF,
    atom::{JSAtom, JSAtomRc, consts::JS_ATOM_empty_string},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE,
        PropertyKey,
    },
    parse::JS_ParseJSON,
    runtime::js_check_stack_overflow,
    value::JSValue,
};

use super::stringify::JS_JSONStringify;

#[inline(always)]
fn js_json_stringify(ctx: &JSContext, cc: CallContext) -> JSResult {
    JS_JSONStringify(ctx, cc.arg(0).clone(), cc.arg(1).clone(), cc.arg(2).clone())
}

fn internalize_json_property(
    ctx: &JSContext,
    holder: &JSValue,
    name: JSAtom,
    reviver: &JSValue,
) -> JSResult {
    if js_check_stack_overflow(ctx.rt(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    let val = holder.get_property(ctx, name)?;

    if let Some(o) = val.as_object() {
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
            let prop: JSAtomRc = if is_array {
                JSAtom::from_u32(ctx.rt_mut(), i)?
            } else {
                unsafe { pks.get_unchecked(i as usize).atom.dup(ctx.rt()) }
            }
            .into_rc(ctx.rt());

            let new_el = internalize_json_property(ctx, &val, *prop, reviver)?;
            if new_el.is_undefined() {
                val.delete_property(ctx, *prop, 0)
            } else {
                val.define_property_value(ctx, *prop, new_el, JS_PROP_C_W_E)
            }?;
        }
    }

    reviver.call(ctx, holder, &[name.to_string_or_symbol(ctx)?, val])
}

fn js_json_parse(ctx: &JSContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;
    let mut obj = JS_ParseJSON(ctx, s.content().to_utf8().as_bytes(), "<input>")?;

    if cc.length() > 1 && cc.arg(1).is_function() {
        let reviver = cc.arg(1);
        let root: JSValue = ctx.create_base_object()?.into();
        root.define_property_value(ctx, JS_ATOM_empty_string, obj.clone(), JS_PROP_C_W_E)?;
        obj = internalize_json_property(ctx, &root, JS_ATOM_empty_string, reviver)?;
    }

    Ok(obj)
}

static js_json_funcs: [FieldDef; 3] = [
    JS_CFUNC_NATIVE_DEF!("parse", 2, js_json_parse, 0),
    JS_CFUNC_NATIVE_DEF!("stringify", 3, js_json_stringify, 0),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "JSON", JS_PROP_CONFIGURABLE),
];

pub static js_json_obj: [FieldDef; 1] = [JS_OBJECT_DEF!(
    "JSON",
    &js_json_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE
)];
