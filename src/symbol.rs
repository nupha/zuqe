use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_SYMBOL, JS_PROP_STRING_DEF,
    atom::{Atom, AtomTypeHint},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_CONFIGURABLE,
    string::{JSString, StringContent},
};

use super::value::JSValue;

impl JSContext {
    /// `desc` should be None for anonymous symbol.
    pub fn create_symbol(&self, str: Option<JSString>, atom_type: AtomTypeHint) -> JSResult {
        debug_assert!(
            matches!(
                atom_type,
                AtomTypeHint::GlobalSymbol | AtomTypeHint::Symbol | AtomTypeHint::Private
            ),
            "{atom_type:?}"
        );

        let a = Atom::from_string_as_is(
            str.unwrap_or_else(|| {
                debug_assert_eq!(atom_type, AtomTypeHint::Symbol);
                JSString::new(self.rt_mut(), StringContent::ident())
            }),
            atom_type,
        )?;

        let ss = a.description();
        Ok(JSValue::Symbol(ss))
    }

    /// `atom` must be a non-numeric string atom
    pub fn create_symbol_from_atom(&self, atom: &Atom, atom_type: AtomTypeHint) -> JSResult {
        debug_assert!(!atom.is_small_int());
        let s = atom.description();
        self.create_symbol(Some(s), atom_type)
    }
}

/// returned atom is not cloned. caller should clone to keep its reference.
pub(crate) fn symbol_to_atom(val: &JSValue) -> Atom {
    if let JSValue::Symbol(s) = val {
        s.atom_info().unwrap().atom_ref().deref().clone()
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
}

//
// prototype funcs
//

pub fn js_thisSymbolValue(ctx: &JSContext, val: &JSValue) -> JSResult {
    if val.is_symbol() {
        Ok(val.clone())
    } else {
        // check symbol object
        val.as_object()
            .and_then(|o| {
                if o.class_id() == JS_CLASS_SYMBOL {
                    let s = o.payload().value();
                    debug_assert!(s.is_some_and(|v| v.is_symbol()));
                    s
                } else {
                    None
                }
            })
            // .filter(|v| v.is_symbol())
            .cloned()
            .ok_or_else(|| {
                ctx.throw_type_error_not_a_symbol();
                -1
            })
    }
}

pub fn js_symbol_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    let val = js_thisSymbolValue(ctx, cc.this())?;
    // formatted as 'Symbol(XXX)'
    ctx.create_string_from_utf8(val.inspect(ctx)?.into_bytes())
        .map(JSValue::String)
}

pub fn js_symbol_valueOf(ctx: &JSContext, cc: CallContext) -> JSResult {
    js_thisSymbolValue(ctx, cc.this())
}

pub fn js_symbol_get_description(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(p) = js_thisSymbolValue(ctx, cc.this())? {
        if p.content().is_ident() {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(p.into())
        }
    } else {
        unreachable!()
    }
}

pub static js_symbol_proto_funcs: [FieldDef; 5] = [
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_symbol_toString, 0),
    JS_CFUNC_NATIVE_DEF!("valueOf", 0, js_symbol_valueOf, 0),
    // XXX: should have writable: false
    JS_CFUNC_NATIVE_DEF!("[Symbol.toPrimitive]", 1, js_symbol_valueOf, 0),
    JS_PROP_STRING_DEF!("[Symbol.toStringTag]", "Symbol", JS_PROP_CONFIGURABLE),
    JS_CGETSET_NATIVE_DEF!("description", js_symbol_get_description, NONE, 0),
];

fn js_symbol_for(ctx: &JSContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;
    ctx.create_symbol(Some(s), AtomTypeHint::GlobalSymbol)
}

fn js_symbol_keyFor(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(p) = cc.arg(0) {
        if p.atom_info().unwrap().type_hint() != AtomTypeHint::GlobalSymbol {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::String(p.clone()))
        }
    } else {
        ctx.throw_type_error_not_a_symbol();
        Err(-1)
    }
}

pub static js_symbol_funcs: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("for", 1, js_symbol_for, 0),
    JS_CFUNC_NATIVE_DEF!("keyFor", 1, js_symbol_keyFor, 0),
];
