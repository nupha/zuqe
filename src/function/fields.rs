// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    ClassID, JS_PROP_C_W_E,
    atom::Atom,
    context::ScopedContext,
    function::{
        Callback, CallbackFn, GetterFn, IteratorFn, JSCFunctionFF, JSCFunctionFFF, SetterFn,
    },
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, LazyInit, ObjectImpl},
    value::JSValue,
};

#[repr(u8)]
#[derive(Debug, Clone)]
pub(crate) enum FieldDetail {
    GetSet {
        get: Option<Callback>,
        set: Option<Callback>,
        magic: i16,
    },

    /// build Object with defined fields
    LazyObject(&'static [FieldDef]),
    /// build String from &str
    LazyStr(&'static str),
    /// build Value from a builder func
    LazyValueFn(fn(&mut ScopedContext) -> Result<JSValue, i32>),
    /// build Function object with callback
    LazyFunc {
        callback: Callback,
        length: u8,
        magic: i16,
    },
    /// build class constructor from class id
    LazyClass(fn() -> ClassID),

    Bool(bool),
    Int32(i32),
    Float64(f64),
    Undefined,

    Alias {
        name: &'static str,
        base: i32,
    },
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub(crate) name: &'static str,
    pub(crate) detail: FieldDetail,
    pub(crate) prop_flags: u8,
}

impl FieldDef {
    /// Define a getter, setter field
    pub const fn GETSET(
        name: &'static str,
        get: Option<GetterFn>,
        set: Option<SetterFn>,
        magic: i16,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::GetSet {
                get: if let Some(f) = get {
                    Some(Callback::Getter(f))
                } else {
                    None
                },
                set: if let Some(f) = set {
                    Some(Callback::Setter(f))
                } else {
                    None
                },
                magic,
            },
            prop_flags: JS_PROP_CONFIGURABLE as u8,
        }
    }

    /// Define a lazy initialized class constructor field
    pub const fn CLASS(name: &'static str, f: fn() -> ClassID, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyClass(f),
            prop_flags: prop_flags as u8,
        }
    }

    /// Define a lazy initialized function backed by a native handler.
    pub const fn FUNC(name: &'static str, length: u8, callback: CallbackFn, magic: i16) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyFunc {
                callback: Callback::Func(callback),
                length,
                magic,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
        }
    }

    /// Like [`FieldDef::FUNC`] but also enumerable (used for e.g.
    /// `Headers.prototype.{keys,values,entries}` which the Fetch spec
    /// requires to be enumerable).
    pub const fn FUNC_ENUM(
        name: &'static str,
        length: u8,
        callback: CallbackFn,
        magic: i16,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyFunc {
                callback: Callback::Func(callback),
                length,
                magic,
            },
            prop_flags: JS_PROP_C_W_E as u8,
        }
    }

    /// Define a lazy initialized iterator next function field backed by a native callback.
    pub const fn ITERATOR_NEXT(
        name: &'static str,
        length: u8,
        callback: IteratorFn,
        magic: i16,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyFunc {
                callback: Callback::IteratorNext(callback),
                length,
                magic,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
        }
    }

    /// Define a lazy initialized base object with field defination entries.
    pub const fn OBJECT(name: &'static str, entries: &'static [Self], prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyObject(entries),
            prop_flags: prop_flags as _,
        }
    }

    /// Define a lazy initialized string field
    pub const fn STR(name: &'static str, val: &'static str, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyStr(val),
            prop_flags: prop_flags as u8,
        }
    }

    /// Define a lazy initialized value field with build function
    pub const fn LAZY_VALUE(
        name: &'static str,
        f: fn(&mut ScopedContext) -> Result<JSValue, i32>,
        prop_flags: u32,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyValueFn(f),
            prop_flags: prop_flags as u8,
        }
    }

    /// Define a bool field
    pub const fn BOOL(name: &'static str, val: bool, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Bool(val),
            prop_flags: prop_flags as u8,
        }
    }
    /// Define an i32 field
    pub const fn INT32(name: &'static str, val: i32, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Int32(val),
            prop_flags: prop_flags as u8,
        }
    }
    /// Define a f64 field
    pub const fn FLOAT64(name: &'static str, val: f64, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Float64(val),
            prop_flags: prop_flags as u8,
        }
    }
    /// Define a undefined value field
    pub const fn UNDEFINED(name: &'static str, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Undefined,
            prop_flags: prop_flags as u8,
        }
    }

    /// Define an alias field
    pub const fn ALIAS_BASE(name: &'static str, from_name: &'static str, base: i32) -> Self {
        Self {
            name,
            detail: FieldDetail::Alias {
                name: from_name,
                base,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
        }
    }

    /// Define an alias field
    #[inline(always)]
    pub const fn ALIAS(name: &'static str, from_name: &'static str) -> Self {
        Self::ALIAS_BASE(name, from_name, -1)
    }

    /// Define a (f64)->f64 callback function backed by a native handler.
    pub(crate) const fn CALL_FF(
        name: &'static str,
        length: u8,
        callback: JSCFunctionFF,
        magic: i16,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyFunc {
                callback: Callback::FF(callback),
                length,
                magic,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
        }
    }

    /// Define a (f64, f64)->f64 callback function backed by a native handler.
    pub(crate) const fn CALL_FFF(
        name: &'static str,
        length: u8,
        callback: JSCFunctionFFF,
        magic: i16,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::LazyFunc {
                callback: Callback::FFF(callback),
                length,
                magic,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
        }
    }
}

impl ObjectImpl {
    pub fn install_fields(
        &mut self,
        ctx: &mut ScopedContext,
        entries: &[FieldDef],
    ) -> Result<(), i32> {
        for entry in entries {
            let atom = prop_str_to_atom(ctx, entry.name)?;
            self.install_field_entry(ctx, atom, entry.clone())?;
        }
        Ok(())
    }

    fn install_field_entry(
        &mut self,
        ctx: &mut ScopedContext,
        atom: Atom,
        entry: FieldDef,
    ) -> Result<(), i32> {
        debug_assert!(!entry.name.is_empty());

        let mut prop_flags = entry.prop_flags;
        let val: JSValue;

        match entry.detail {
            FieldDetail::GetSet { get, set, magic } => {
                // TODO: use lazy init?
                let getter = if let Some(f) = get {
                    ctx.create_native_function(f, &format!("get {}", entry.name), 0, magic)?
                        .into()
                } else {
                    JSValue::UNDEFINED
                };
                let setter = if let Some(f) = set {
                    ctx.create_native_function(f, &format!("set {}", entry.name), 1, magic)?
                        .into()
                } else {
                    JSValue::UNDEFINED
                };
                return self
                    .define_property_getset(ctx, atom, getter, setter, prop_flags as _)
                    .map(|_| ());
            }

            FieldDetail::LazyValueFn(f) => {
                self.define_lazy_property(ctx, atom, LazyInit::ValueFn(f), prop_flags as _)?;
                return Ok(());
            }
            FieldDetail::LazyFunc {
                callback,
                length,
                magic,
            } => {
                if atom == Atom::CONST_Symbol_toPrimitive {
                    prop_flags = JS_PROP_CONFIGURABLE as u8; // Symbol.toPrimitive functions are not writable
                } else if atom == Atom::CONST_Symbol_hasInstance {
                    prop_flags = 0; // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                }

                self.define_lazy_property(
                    ctx,
                    atom,
                    LazyInit::Func {
                        callback,
                        length,
                        magic,
                    },
                    prop_flags as _,
                )?;
                return Ok(());
            }
            FieldDetail::LazyStr(s) => {
                self.define_lazy_property(ctx, atom, LazyInit::String(s), prop_flags as _)?;
                return Ok(());
            }
            FieldDetail::LazyObject(tab) => {
                self.define_lazy_property(ctx, atom, LazyInit::Object(tab), prop_flags as _)?;
                return Ok(());
            }
            FieldDetail::LazyClass(f) => {
                self.define_lazy_property(ctx, atom, LazyInit::Class(f), prop_flags as _)?;
                return Ok(());
            }

            FieldDetail::Bool(b) => {
                val = JSValue::from(b);
            }
            FieldDetail::Int32(i) => {
                val = JSValue::from(i);
            }
            FieldDetail::Float64(d) => {
                val = JSValue::from(d);
            }
            FieldDetail::Undefined => {
                val = JSValue::UNDEFINED;
            }

            FieldDetail::Alias { name, base } => {
                let src_atom = prop_str_to_atom(ctx, name)?;
                val = match base {
                    -1 => self.get_property(ctx, src_atom)?,
                    0 => ctx.global_obj().get_property(ctx, src_atom)?,
                    1 => ctx.array_proto().get_property(ctx, src_atom)?,
                    _ => {
                        unreachable!();
                    }
                };

                if atom == Atom::CONST_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == Atom::CONST_Symbol_hasInstance {
                    // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                    prop_flags = 0;
                }
            }
        }

        self.define_property_value(ctx, atom, val, prop_flags as _)
            .map(|_| ())
    }
}

impl JSValue {
    pub fn install_fields(
        &mut self,
        ctx: &mut ScopedContext,
        entries: &[FieldDef],
    ) -> Result<(), i32> {
        if let Some(mut o) = self.as_object() {
            o.install_fields(ctx, entries)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }
}

pub(crate) fn prop_str_to_atom(ctx: &mut ScopedContext, str: &str) -> Result<Atom, i32> {
    if str.starts_with('[') && str.ends_with(']') {
        // check [Symbol.XXX] const atom
        let name2 = &str[1..str.len() - 1]; // chop heading '[', tailing ']'

        // We assume 8 bit non null strings, which is the case for these symbols
        for id in crate::atom::ATOM_CONST_SYMBOL_RANGE {
            let a = Atom::from_id(id);
            if a.descriptor(ctx.rt()).content().eq_str(name2) {
                return Ok(a);
            }
        }

        return Err(ctx.throw_internal_error(&format!("unknown atom '{str}'")));
    }

    Atom::intern_str(ctx, str)
}
