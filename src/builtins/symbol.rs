use crate::{
    JS_CLASS_SYMBOL,
    atom::{Atom, AtomKind},
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_CONFIGURABLE,
    string::{StringData, StringRef},
    value::JSValue,
};

impl ContextImpl {
    /// `str` could be None for anonymous symbol.
    pub fn create_symbol(&self, str: Option<StringRef>, atom_kind: AtomKind) -> JSResult {
        debug_assert!(
            matches!(
                atom_kind,
                AtomKind::GlobalSymbol | AtomKind::Symbol | AtomKind::Private
            ),
            "{atom_kind:?}"
        );

        let atom = Atom::from_string_as_is(
            self.rt(),
            if let Some(s) = str {
                s
            } else {
                debug_assert_eq!(atom_kind, AtomKind::Symbol);
                StringRef::new(&self.rt(), StringData::Ident).map_err(|_| {
                    self.throw_out_of_memory();
                    -1
                })?
            },
            atom_kind,
        )?;

        let sym = JSValue::Symbol(atom);
        Ok(sym)
    }

    pub fn create_symbol_from_atom(&self, atom: Atom, atom_kind: AtomKind) -> JSResult {
        if atom.is_small_int() || atom.is_null() {
            self.throw_type_error("invalid atom");
            Err(-1)
        } else {
            let s = atom.string();
            self.create_symbol(Some(s), atom_kind)
        }
    }
}

/// returned atom is not cloned. caller should clone to keep its reference.
pub(crate) fn symbol_to_atom(val: JSValue) -> Atom {
    if let JSValue::Symbol(a) = val {
        a
    } else {
        unreachable!()
    }
}

pub fn js_symbol_constructor(ctx: ContextRef, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if !new_target.is_undefined() {
        ctx.throw_type_error("not a constructor");
        return Err(-1);
    }

    let str: Option<StringRef> = if cc.arg(0).is_undefined() {
        None
    } else {
        Some(cc.arg(0).to_string(&ctx)?)
    };
    ctx.create_symbol(str, AtomKind::Symbol)
}

//
// prototype funcs
//

pub fn js_thisSymbolValue(ctx: &ContextImpl, val: JSValue) -> JSResult {
    if val.is_symbol() {
        Ok(val)
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

pub fn js_symbol_toString(ctx: ContextRef, cc: CallContext) -> JSResult {
    let val = js_thisSymbolValue(&ctx, *cc.this())?;
    // formatted as 'Symbol(XXX)'
    ctx.create_string_from_utf8(val.inspect(&ctx)?.into_bytes())
        .map(JSValue::String)
}

pub fn js_symbol_valueOf(ctx: ContextRef, cc: CallContext) -> JSResult {
    js_thisSymbolValue(&ctx, *cc.this())
}

pub fn js_symbol_get_description(ctx: ContextRef, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(a) = js_thisSymbolValue(&ctx, *cc.this())? {
        let s = a.string();
        if s.content().is_ident() {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(s.into())
        }
    } else {
        unreachable!()
    }
}

pub static js_symbol_proto_funcs: [FieldDef; 5] = [
    FieldDef::FUNCTION_V1("toString", 0, js_symbol_toString, 0),
    FieldDef::FUNCTION_V1("valueOf", 0, js_symbol_valueOf, 0),
    // XXX: should have writable: false
    FieldDef::FUNCTION_V1("[Symbol.toPrimitive]", 1, js_symbol_valueOf, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Symbol", JS_PROP_CONFIGURABLE),
    FieldDef::GETSET("description", Some(js_symbol_get_description), None, 0),
];

fn js_symbol_for(ctx: ContextRef, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(&ctx)?;
    ctx.create_symbol(Some(s), AtomKind::GlobalSymbol)
}

fn js_symbol_keyFor(ctx: ContextRef, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(a) = cc.arg(0) {
        let s = a.string();
        if s.atom_info().unwrap().kind() != AtomKind::GlobalSymbol {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::String(s))
        }
    } else {
        ctx.throw_type_error_not_a_symbol();
        Err(-1)
    }
}

pub static js_symbol_funcs: [FieldDef; 2] = [
    FieldDef::FUNCTION_V1("for", 1, js_symbol_for, 0),
    FieldDef::FUNCTION_V1("keyFor", 1, js_symbol_keyFor, 0),
];
