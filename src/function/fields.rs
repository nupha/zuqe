use std::ptr::NonNull;

use crate::{
    atom::Atom,
    context::{Context, ContextImpl, ContextPtr},
    function::{
        Callback, CallbackFn, GetterFn, IteratorFn, JSCFunctionFF, JSCFunctionFFF, SetterFn,
    },
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, LazyInit, Object},
    value::JSValue,
};

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum FieldDetail {
    Func {
        handler: Callback,
        length: u8,
    },
    GetSet {
        get: Option<Callback>,
        set: Option<Callback>,
    },
    Alias {
        name: &'static str,
        base: i32,
    },
    /// Lazy init object with entries
    Object(&'static [FieldDef]),
    Str(&'static str),
    Int32(i32),
    Float64(f64),
    Undefined,
    /// user defined value build func
    Build(fn(Context) -> Result<JSValue, i32>),
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: &'static str,
    pub detail: FieldDetail,
    pub prop_flags: u8,
    pub magic: i16,
}

impl FieldDef {
    /// Define a callback function backed by a native handler.
    pub const fn CALL(name: &'static str, length: u8, callback: CallbackFn, magic: i16) -> Self {
        Self {
            name,
            detail: FieldDetail::Func {
                handler: Callback::Func(callback),
                length,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
            magic,
        }
    }

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
            },
            prop_flags: JS_PROP_CONFIGURABLE as u8,
            magic,
        }
    }

    /// Define an iterator next function field backed by a native callback handler.
    pub const fn ITERATOR_NEXT(
        name: &'static str,
        length: u8,
        callback: IteratorFn,
        magic: i16,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::Func {
                handler: Callback::IteratorNext(callback),
                length,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
            magic,
        }
    }

    /// Define a lazy init object with entries.
    pub const fn OBJECT(name: &'static str, entries: &'static [Self], prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Object(entries),
            prop_flags: prop_flags as u8,
            magic: 0,
        }
    }

    /// Define an i32 field
    pub const fn INT32(name: &'static str, val: i32, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Int32(val),
            prop_flags: prop_flags as u8,
            magic: 0,
        }
    }

    /// Define an f64 field
    pub const fn FLOAT64(name: &'static str, val: f64, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Float64(val),
            prop_flags: prop_flags as u8,
            magic: 0,
        }
    }

    /// Define a str field
    pub const fn STR(name: &'static str, val: &'static str, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Str(val),
            prop_flags: prop_flags as u8,
            magic: 0,
        }
    }

    /// Define an undefined value field
    pub const fn UNDEFINED(name: &'static str, prop_flags: u32) -> Self {
        Self {
            name,
            detail: FieldDetail::Undefined,
            prop_flags: prop_flags as u8,
            magic: 0,
        }
    }

    pub const fn BUILD(
        name: &'static str,
        f: fn(Context) -> Result<JSValue, i32>,
        prop_flags: u32,
    ) -> Self {
        Self {
            name,
            detail: FieldDetail::Build(f),
            prop_flags: prop_flags as u8,
            magic: 0,
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
            magic: 0,
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
            detail: FieldDetail::Func {
                handler: Callback::FF(callback),
                length,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
            magic,
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
            detail: FieldDetail::Func {
                handler: Callback::FFF(callback),
                length,
            },
            prop_flags: (JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE) as u8,
            magic,
        }
    }
}

impl Object {
    pub fn install_fields(
        &mut self,
        ctx: &ContextImpl,
        entries: &'static [FieldDef],
    ) -> Result<(), i32> {
        for entry in entries {
            let atom = prop_name_to_atom(ctx, entry.name)?;
            self.install_field_entry(ctx, atom, entry)?;
        }
        Ok(())
    }

    fn install_field_entry(
        &mut self,
        ctx: &ContextImpl,
        atom: Atom,
        entry: &'static FieldDef,
    ) -> Result<(), i32> {
        let val: JSValue;
        let mut prop_flags = entry.prop_flags;

        match entry.detail {
            FieldDetail::Func { .. } => {
                #[cfg(feature = "builtin_symbol")]
                if atom == Atom::CONST_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == Atom::CONST_Symbol_hasInstance {
                    // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                    prop_flags = 0;
                }

                self.define_lazy_property(
                    ctx,
                    atom,
                    LazyInit::Prop(NonNull::from(entry)),
                    prop_flags as _,
                )?;

                return Ok(());
            }
            FieldDetail::GetSet { get, set } => {
                // XXX: use autoinit again ?
                return self
                    .define_property_getset(
                        ctx,
                        atom,
                        if let Some(f) = get {
                            ctx.create_native_function(
                                f,
                                &format!("get {}", entry.name),
                                0,
                                entry.magic,
                            )?
                            .into()
                        } else {
                            JSValue::UNDEFINED
                        },
                        if let Some(f) = set {
                            ctx.create_native_function(
                                f,
                                &format!("set {}", entry.name),
                                1,
                                entry.magic,
                            )?
                            .into()
                        } else {
                            JSValue::UNDEFINED
                        },
                        prop_flags as _,
                    )
                    .map(|_| ());
            }
            FieldDetail::Str(_) | FieldDetail::Object(_) => {
                return self.define_lazy_property(
                    ctx,
                    atom,
                    LazyInit::Prop(unsafe {
                        NonNull::new_unchecked(entry as *const FieldDef as *mut FieldDef)
                    }),
                    prop_flags as _,
                );
            }

            FieldDetail::Int32(i) => {
                val = JSValue::Int(i);
            }
            FieldDetail::Float64(d) => {
                val = JSValue::Float64(d);
            }
            FieldDetail::Undefined => {
                val = JSValue::UNDEFINED;
            }
            FieldDetail::Build(f) => {
                let c = unsafe { ContextPtr::from_ref_unchecked(ctx) };
                val = f(c.into())?;
            }

            FieldDetail::Alias { name, base } => {
                // using autoinit for aliases is not safe
                let name_atom = prop_name_to_atom(ctx, name)?;
                val = match base {
                    -1 => self.get_property(ctx, name_atom)?,
                    0 => ctx.global_obj().get_property(ctx, name_atom)?,
                    1 => ctx.array_proto().get_property(ctx, name_atom)?,
                    _ => {
                        unreachable!();
                    }
                };

                #[cfg(feature = "builtin_symbol")]
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
        ctx: &ContextImpl,
        entries: &'static [FieldDef],
    ) -> Result<(), i32> {
        if let Self::Object(o) = self {
            o.install_fields(ctx, entries)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}

fn prop_name_to_atom(ctx: &ContextImpl, str: &str) -> Result<Atom, i32> {
    #[cfg(not(feature = "builtin_symbol"))]
    debug_assert!(
        !(str.starts_with('[') && str.ends_with(']')),
        "can't use prop name '{str}': Symbol is not supported"
    );

    if str.starts_with('[') && str.ends_with(']') {
        // check [Symbol.XXX] const atom

        #[cfg(feature = "builtin_symbol")]
        {
            let name2 = &str[1..str.len() - 1]; // chop heading '[', tailing ']'

            // We assume 8 bit non null strings, which is the case for these symbols
            for id in crate::atom::ATOM_CONST_SYMBOL_RANGE {
                let a = Atom::from_id(id);
                if a.desc_string(ctx.rt_ref()).content().eq_str(name2) {
                    return Ok(a);
                }
            }
        }

        ctx.throw_internal_error(&format!("unknown atom '{str}'"));
        return Err(-1);
    }

    Atom::new(ctx.rt_ptr(), str)
}
