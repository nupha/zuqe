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
            Atom::from_string_as_is(self.rt_ptr(), s, atom_kind)?
        } else {
            debug_assert_eq!(atom_kind, AtomKind::Symbol);
            let s = StringRef::create(self.rt_ptr(), StringData::Ident).map_err(|_| {
                self.throw_out_of_memory();
                -1
            })?;
            Atom::from_string_as_is(self.rt_ptr(), s, atom_kind)?
        };

        let sym = JSValue::Symbol(atom.descriptor(self.rt()));
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

pub fn js_thisSymbolValue(ctx: &mut ScopedContext, val: JSValue) -> JSResult {
    val.as_symbol()
        .copied()
        .map(JSValue::Symbol)
        .ok_or_else(|| {
            ctx.throw_type_error_not_a_symbol();
            -1
        })
}

fn js_symbol_toString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = js_thisSymbolValue(ctx, cc.this())?;
    // formatted as 'Symbol(XXX)'
    let repr = val.inspect(ctx.rt_mut())?.into_bytes();
    ctx.create_string_from_utf8(repr).map(JSValue::from)
}

fn js_symbol_valueOf(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    js_thisSymbolValue(ctx, cc.this())
}

fn js_symbol_get_description(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(s) = js_thisSymbolValue(ctx, cc.this())? {
        if s.content().is_ident() {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(s.into())
        }
    } else {
        unreachable!()
    }
}

pub const js_symbol_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("toString", 0, js_symbol_toString, 0),
    FieldDef::CALL("valueOf", 0, js_symbol_valueOf, 0),
    // XXX: should have writable: false
    FieldDef::CALL("[Symbol.toPrimitive]", 1, js_symbol_valueOf, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Symbol", JS_PROP_CONFIGURABLE),
    FieldDef::GETSET("description", Some(js_symbol_get_description), None, 0),
];

fn js_symbol_for(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;
    ctx.create_symbol(Some(s), AtomKind::GlobalSymbol)
}

fn js_symbol_keyFor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let JSValue::Symbol(s) = cc.arg(0) {
        if s.atom_info().unwrap().kind() != AtomKind::GlobalSymbol {
            Ok(JSValue::UNDEFINED)
        } else {
            Ok(JSValue::from(s))
        }
    } else {
        ctx.throw_type_error_not_a_symbol();
        Err(-1)
    }
}

pub const js_symbol_funcs: &[FieldDef] = &[
    FieldDef::CALL("for", 1, js_symbol_for, 0),
    FieldDef::CALL("keyFor", 1, js_symbol_keyFor, 0),
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
