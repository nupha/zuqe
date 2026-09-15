// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ClassCtorDef,
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
            Atom::from_content_as_is(self, s, atom_kind)?
        } else {
            debug_assert_eq!(atom_kind, AtomKind::Symbol);
            let s = StringRef::from_content(self, StringData::Ident)
                .map_err(|_| self.throw_out_of_memory())?;
            Atom::from_content_as_is(self, s, atom_kind)?
        };

        let sym = JSValue::make_symbol(atom.to_string_ref(self)?);
        Ok(sym)
    }

    pub fn create_symbol_from_atom(&mut self, atom: Atom, atom_kind: AtomKind) -> JSResult {
        if atom.is_small_int() || atom.is_null() {
            Err(self.throw_type_error("invalid atom"))
        } else {
            let s = atom.to_string_ref(self)?;
            self.create_symbol(Some(s), atom_kind)
        }
    }

    /// Create (first call) or return the cached per-realm symbol for
    /// `description`.
    ///
    /// Unlike [`Self::create_symbol`] — which mints a fresh identity every
    /// call — the same description always returns the SAME symbol here,
    /// giving builtins a stable property key that user code can neither forge
    /// nor collide with. Identities never cross contexts.
    ///
    /// Mirrors ECMA-402's `%Intl%.[[FallbackSymbol]]`
    /// (`"IntlLegacyConstructedSymbol"`): the key under which legacy
    /// `Intl.DateTimeFormat.call(obj)` stashes the real instance on the
    /// receiver for later unwrap via ordinary `[[Get]]`.
    pub fn create_realm_symbol(&mut self, description: &str) -> JSResult {
        if let Some((_, sym)) = self.realm_symbols.iter().find(|(d, _)| d == description) {
            return Ok(*sym);
        }
        let desc = self.create_string_interned(description)?;
        let sym = self.create_symbol(Some(desc), AtomKind::Symbol)?;
        self.realm_symbols.push((description.to_string(), sym));
        Ok(sym)
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

crate::def_native_object!(
    builtin,
    "Symbol",
    CLASS_DEF_SYMBOL,
    class_id: (crate::JS_CLASS_SYMBOL, crate::Atom::CONST_Symbol),
    crate::string::StringRef,
    js_symbol_proto_funcs,
    Some(
        ClassCtorDef::new(js_symbol_constructor, 0, true)
            .entries(js_symbol_funcs)
            .expose_to_global(),
    )
);

#[cfg(test)]
mod symbol_key_tests {
    //! Locks in the builtin-facing symbol-key property recipe: a runtime
    //! Symbol value converts to its interned key atom and works through the
    //! ordinary define/get paths (the mechanism ECMA-402's FallbackSymbol
    //! and the well-known symbols rely on).

    use crate::{
        JSValue, Runtime, ScopedContext,
        atom::AtomKind,
        object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
    };

    fn make_named_symbol(cx: &mut ScopedContext) -> JSValue {
        let desc = cx
            .create_string_from_utf8(b"IntlLegacyConstructedSymbol".to_vec())
            .ok();
        cx.create_symbol(desc, AtomKind::Symbol).unwrap()
    }

    #[test]
    fn symbol_key_define_get_roundtrip() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
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

    #[test]
    fn realm_symbol_is_stable_per_realm() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        ctx.with_root_scope(|mut cx| {
            // Same description: identical symbol on repeated calls.
            let a1 = cx
                .create_realm_symbol("IntlLegacyConstructedSymbol")
                .unwrap();
            let a2 = cx
                .create_realm_symbol("IntlLegacyConstructedSymbol")
                .unwrap();
            assert_eq!(
                a1.to_atom(&mut cx).unwrap(),
                a2.to_atom(&mut cx).unwrap(),
                "realm_symbol must be stable within a realm"
            );

            // Different description: different symbol.
            let b = cx.create_realm_symbol("other.marker").unwrap();
            assert_ne!(a1.to_atom(&mut cx).unwrap(), b.to_atom(&mut cx).unwrap());

            // Usable as a real property key through the standard recipe.
            let mut obj = cx.create_base_object().unwrap();
            let key = a1.to_atom(&mut cx).unwrap();
            obj.define_property_value(&mut cx, key, JSValue::from(true), JS_PROP_CONFIGURABLE)
                .unwrap();
            let got = obj.get_property_value(&mut cx, a1).unwrap();
            assert!(got.as_bool() == Some(true));
        });
    }
}
