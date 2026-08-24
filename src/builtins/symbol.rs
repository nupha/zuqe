// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ClassCtorDef, ClassDef,
    atom::{Atom, AtomKind},
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::JS_PROP_CONFIGURABLE,
    string::{StringData, StringRef},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    /// `str` is None for anonymous symbol.
    pub fn create_symbol(&mut self, str: Option<StringRef>, atom_kind: AtomKind) -> JSResult {
        debug_assert!(
            matches!(
                atom_kind,
                AtomKind::GlobalSymbol | AtomKind::Symbol | AtomKind::Private
            ),
            "{atom_kind:?}"
        );

        let atom = if let Some(s) = str {
            Atom::from_string_as_is(self, s, atom_kind)?
        } else {
            debug_assert_eq!(atom_kind, AtomKind::Symbol);
            let s = StringRef::create(self, StringData::Ident)
                .map_err(|_| self.throw_out_of_memory())?;
            Atom::from_string_as_is(self, s, atom_kind)?
        };

        let sym = JSValue::make_symbol(atom.descriptor(self.rt()));
        Ok(sym)
    }

    pub fn create_symbol_from_atom(&mut self, atom: Atom, atom_kind: AtomKind) -> JSResult {
        if atom.is_small_int() || atom.is_null() {
            Err(self.throw_type_error("invalid atom"))
        } else {
            let s = atom.descriptor(self.rt());
            self.create_symbol(Some(s), atom_kind)
        }
    }
}

pub fn js_symbol_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if !new_target.is_undefined() {
        return Err(ctx.throw_type_error("not a constructor"));
    }

    let str: Option<StringRef> = if cc.arg(0).is_undefined() {
        None
    } else {
        Some(cc.arg(0).to_string(ctx)?)
    };
    ctx.create_symbol(str, AtomKind::Symbol)
}

//
// prototype funcs
//

fn js_symbol_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(sym) = cc.this().as_symbol(true) else {
        return Err(ctx.throw_type_error_not_a_symbol());
    };

    // formatted as 'Symbol(XXX)'
    let repr = if sym.content().is_ident() {
        "Symbol()".to_string()
    } else {
        format!("Symbol({})", sym.content().encode_utf8(false))
    };
    ctx.create_string_from_utf8(repr.into_bytes())
        .map(JSValue::make_string)
}

fn js_symbol_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this()
        .as_symbol(true)
        .ok_or_else(|| ctx.throw_type_error_not_a_symbol())
        .map(JSValue::make_symbol)
}

fn js_symbol_get_description(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(sym) = cc.this().as_symbol(true) else {
        return Err(ctx.throw_type_error_not_a_symbol());
    };
    if sym.content().is_ident() {
        Ok(JSValue::UNDEFINED)
    } else {
        Ok(JSValue::make_string(sym))
    }
}

pub const js_symbol_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_symbol_toString, 0),
    FieldDef::FUNC("valueOf", 0, js_symbol_valueOf, 0),
    // XXX: should have writable: false
    FieldDef::FUNC("[Symbol.toPrimitive]", 1, js_symbol_valueOf, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Symbol", JS_PROP_CONFIGURABLE),
    FieldDef::GETSET("description", Some(js_symbol_get_description), None, 0),
];

fn js_symbol_for(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;
    ctx.create_symbol(Some(s), AtomKind::GlobalSymbol)
}

fn js_symbol_keyFor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let arg = cc.arg(0);
    if !arg.is_symbol() {
        return Err(ctx.throw_type_error_not_a_symbol());
    }
    let s = arg.as_symbol(false).unwrap();
    if s.atom_info().unwrap().kind() != AtomKind::GlobalSymbol {
        Ok(JSValue::UNDEFINED)
    } else {
        Ok(JSValue::make_string(s))
    }
}

pub const js_symbol_funcs: &[FieldDef] = &[
    FieldDef::FUNC("for", 1, js_symbol_for, 0),
    FieldDef::FUNC("keyFor", 1, js_symbol_keyFor, 0),
];

/// Recipe for builtin authors: a Symbol value is used as a property key by
/// converting it to its atom and passing that to the ordinary property ops —
///
/// ```ignore
/// let key = sym.to_atom(ctx)?;                       // symbol -> key
/// obj.define_property_value(ctx, key, val, flags)?;  // [[DefineOwnProperty]]
/// let v = obj.get_property_value(ctx, sym)?;         // [[Get]] (MOP-observable)
/// ```
///
/// `to_atom` returns the *existing* interned atom for the symbol's descriptor,
/// so every use of the same Symbol value maps to the same property.

pub(crate) static CLASS_DEF_SYMBOL: ClassDef = ClassDef::builtin(
    crate::Atom::CONST_Symbol,
    js_symbol_proto_funcs,
    Some(
        ClassCtorDef::new(js_symbol_constructor, 0, true)
            .entries(js_symbol_funcs)
            .expose_to_global(),
    ),
);

#[cfg(test)]
mod symbol_key_tests {
    //! Locks in the builtin-facing symbol-key property recipe: a runtime
    //! Symbol value converts to its interned key atom and works through the
    //! ordinary define/get paths (the mechanism ECMA-402's FallbackSymbol
    //! and the well-known symbols rely on).

    use crate::{
        atom::AtomKind,
        object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
        Context, Runtime, ScopedContext, JSValue,
    };

    fn make_named_symbol(cx: &mut ScopedContext) -> JSValue {
        let desc = cx.create_string_from_utf8(b"IntlLegacyConstructedSymbol".to_vec()).ok();
        cx.create_symbol(desc, AtomKind::Symbol).unwrap()
    }

    #[test]
    fn symbol_key_define_get_roundtrip() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(|cx| ScopedContext::ECMA_CONTEXT_SETUP(cx))
            .unwrap();

        ctx.with_root_scope(|mut cx| {
            let s1 = make_named_symbol(&mut cx);
            let s2 = make_named_symbol(&mut cx);

            // Same description, distinct identities (Symbol() semantics).
            let k1 = s1.to_atom(&mut cx).unwrap();
            let k2 = s2.to_atom(&mut cx).unwrap();
            assert_ne!(k1, k2, "same-description symbols must stay distinct");

            let mut obj = cx.create_base_object().unwrap();

            // Define via atom, read back via the symbol JSValue.
            obj.define_property_value(
                &mut cx,
                k1,
                JSValue::from(true),
                JS_PROP_C_W_E | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

            let got = obj.get_property_value(&mut cx, s1).unwrap();
            assert!(got.as_bool() == Some(true), "read back through same symbol");

            // The other identity must not see the property.
            let other = obj.get_property_value(&mut cx, s2).unwrap();
            assert!(other.is_undefined(), "distinct symbol must miss");
        });
    }
}
