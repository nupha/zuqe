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
            let s = StringRef::create(self, StringData::Ident).map_err(|_| {
                self.throw_out_of_memory();
                -1
            })?;
            Atom::from_string_as_is(self, s, atom_kind)?
        };

        let sym = JSValue::make_symbol(atom.descriptor(self.rt()));
        Ok(sym)
    }

    pub fn create_symbol_from_atom(&mut self, atom: Atom, atom_kind: AtomKind) -> JSResult {
        if atom.is_small_int() || atom.is_null() {
            self.throw_type_error("invalid atom");
            Err(-1)
        } else {
            let s = atom.descriptor(self.rt());
            self.create_symbol(Some(s), atom_kind)
        }
    }
}

// /// returned atom is not cloned. caller should clone to keep its reference.
// fn symbol_to_atom(val: JSValue) -> Atom {
//     if let JSValue::Symbol(a) = val {
//         a
//     } else {
//         unreachable!()
//     }
// }

pub fn js_symbol_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if !new_target.is_undefined() {
        ctx.throw_type_error("not a constructor");
        return Err(-1);
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
        ctx.throw_type_error_not_a_symbol();
        return Err(-1);
    };

    // formatted as 'Symbol(XXX)'
    let repr = JSValue::make_symbol(sym).inspect(ctx)?.into_bytes();
    ctx.create_string_from_utf8(repr).map(JSValue::make_string)
}

fn js_symbol_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    cc.this()
        .as_symbol(true)
        .ok_or_else(|| {
            ctx.throw_type_error_not_a_symbol();
            -1
        })
        .map(JSValue::make_symbol)
}

fn js_symbol_get_description(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let Some(sym) = cc.this().as_symbol(true) else {
        ctx.throw_type_error_not_a_symbol();
        return Err(-1);
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
        ctx.throw_type_error_not_a_symbol();
        return Err(-1);
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

pub(crate) static CLASS_DEF_SYMBOL: ClassDef = ClassDef::builtin(
    crate::Atom::CONST_Symbol,
    js_symbol_proto_funcs,
    Some(
        ClassCtorDef::new(js_symbol_constructor, 0, true)
            .entries(js_symbol_funcs)
            .expose_to_global(),
    ),
);
