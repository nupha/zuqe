use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_SYMBOL, JS_PROP_STRING_DEF,
    atom::{AtomTypeHint, JSAtom},
    context::JSContext,
    error::JSResult,
    function::{CallContext, NativeField},
    object::JS_PROP_CONFIGURABLE,
    string::JSString,
};

use super::value::JSValue;

impl JSContext {
    pub fn create_symbol(&self, str: Option<JSString>, atom_type: AtomTypeHint) -> JSResult {
        let a = JSAtom::new_internal(self.rt_mut(), str, atom_type)?;
        let ss = self.rt().get_atom_desc(a).unwrap();
        Ok(JSValue::Symbol(ss))
    }

    /// `atom` must be a non-numeric string atom
    pub fn create_symbol_from_atom(&self, atom: JSAtom, atom_type: AtomTypeHint) -> JSResult {
        debug_assert!(!atom.is_int_type());
        let s = self.rt().get_atom_desc(atom).unwrap().clone();
        self.create_symbol(Some(s), atom_type)
    }
}

/// returned atom is not cloned. caller should clone to keep its reference.
pub(crate) fn symbol_to_atom(val: &JSValue) -> JSAtom {
    if let JSValue::Symbol(s) = val {
        s.atom_info().unwrap().atom()
    } else {
        unreachable!()
    }
}

pub fn js_symbol_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if !new_target.is_undefined() {
        ctx.throw_type_error("not a constructor");
        return Err(-1);
    }

    let ss: Option<JSString> = if cc.arg(0).is_undefined() {
        None
    } else {
        Some(cc.arg(0).to_string(ctx)?)
    };
    ctx.create_symbol(ss, AtomTypeHint::Symbol)

    // // let mut str: JSValue = mem::zeroed();
    // // let mut p: *mut JSString = mem::zeroed();

    // if argc == 0 || JS_IsUndefined(*argv.offset(0)) {
    //     p = std::ptr::null_mut();
    // } else {
    //     str = JS_ToString(ctx, *argv.offset(0));
    //     if JS_IsException(str) {
    //         return JS_EXCEPTION;
    //     }
    //     p = JS_VALUE_GET_STRING(str);
    // }
    // return crate::obsoletes::JS_NewSymbol(ctx, p, JS_ATOM_TYPE_SYMBOL);
}

//
// prototype funcs
//

pub fn js_thisSymbolValue(ctx: &JSContext, this_val: &JSValue) -> JSResult {
    if this_val.is_symbol() {
        return Ok(this_val.clone());
    } else if let JSValue::Object(p) = this_val {
        if p.class_id() == JS_CLASS_SYMBOL {
            if let Some(v) = p.0.payload.borrow().value() {
                return Ok(v);
            }
        }
    }

    ctx.throw_type_error("not a symbol");
    Err(-1)
}

pub fn js_symbol_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = js_thisSymbolValue(ctx, cc.this())?;
    ctx.create_string_from_str(val.repr().as_ref())
        .map(JSValue::String)
}

pub fn js_symbol_valueOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    return js_thisSymbolValue(ctx, cc.this());
}

pub fn js_symbol_get_description(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(p) = js_thisSymbolValue(ctx, cc.this())? {
        if p.is_empty() && !p.is_wide() {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::String(p))
        }
    } else {
        unreachable!()
    }
}

pub const js_symbol_proto_funcs: [NativeField; 5] = [
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_symbol_toString, 0),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_symbol_valueOf, 0),
    // XXX: should have writable: false
    JS_CFUNC_NATIVE_DEF!("[Symbol.toPrimitive]", 1, js_symbol_valueOf, 0),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Symbol", JS_PROP_CONFIGURABLE),
    JS_CGETSET_NATIVE_DEF!("description", js_symbol_get_description, NONE, 0),
];

fn js_symbol_for(ctx: &JSContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;
    Ok(JSValue::Symbol(s))
}

fn js_symbol_keyFor(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(p) = cc.arg(0) {
        if p.atom_info()
            .map_or(false, |a| a.type_hint() != AtomTypeHint::GlobalSymbol)
        {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::String(p.clone()))
        }
    } else {
        ctx.throw_type_error("not a symbol");
        Err(-1)
    }
}

pub const js_symbol_funcs: [NativeField; 2] = [
    JS_CFUNC_NATIVE_DEF!("for", 1, js_symbol_for, 0),
    JS_CFUNC_NATIVE_DEF!("keyFor", 1, js_symbol_keyFor, 0),
];
