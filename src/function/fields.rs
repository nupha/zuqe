use std::{borrow::Cow, ptr::NonNull};

use crate::atom::{AtomTypeHint, JS_ATOM_NULL};
use crate::object::JSObject;
use crate::{
    JS_CLASS_ARRAY,
    atom::{JS_FreeAtom, JSAtom, consts::*},
    context::JSContext,
    function::NativeFunctionType,
    object::{JS_PROP_CONFIGURABLE, LazyInit},
    value::JSValue,
};

// Property definition types
pub const JS_DEF_CFUNC: u8 = 0;
pub const JS_DEF_CGETSET: u8 = 1;
pub const JS_DEF_CGETSET_MAGIC: u8 = 2;
pub const JS_DEF_PROP_STRING: u8 = 3;
pub const JS_DEF_PROP_INT32: u8 = 4;
pub const JS_DEF_PROP_INT64: u8 = 5;
pub const JS_DEF_PROP_DOUBLE: u8 = 6;
pub const JS_DEF_PROP_UNDEFINED: u8 = 7;
pub const JS_DEF_OBJECT: u8 = 8;
pub const JS_DEF_ALIAS: u8 = 9;

#[derive(Clone)]
pub struct FieldDef {
    pub name: &'static str,
    pub detail: NativeFieldDetail,
    pub prop_flags: u8,
    pub magic: i16,
}

#[repr(u8)]
#[derive(Clone)]
pub enum NativeFieldDetail {
    Func {
        handler: NativeFunctionType,
        length: u8,
    },
    GetSet {
        get: Option<NativeFunctionType>,
        set: Option<NativeFunctionType>,
    },
    Alias {
        name: &'static str,
        base: i32,
    },
    /// Lazy init object
    Object(Cow<'static, [FieldDef]>),
    Str(&'static str),
    Int32(i32),
    Float64(f64),
    Undefined,
}

impl JSObject {
    pub fn install_field_defs(&self, ctx: &JSContext, entries: &[FieldDef]) -> Result<(), i32> {
        for entry in entries {
            let atom = ctx.make_lazy_prop_atom(entry.name)?;
            self.install_native_field_entry(ctx, atom, entry)?;
            JS_FreeAtom(ctx, atom);
        }
        Ok(())
    }

    fn install_native_field_entry(
        &self,
        ctx: &JSContext,
        atom: JSAtom,
        e: &FieldDef,
    ) -> Result<(), i32> {
        let val: JSValue;
        let mut prop_flags = e.prop_flags;

        match e.detail {
            NativeFieldDetail::Func { .. } => {
                if atom == JS_ATOM_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == JS_ATOM_Symbol_hasInstance {
                    // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                    prop_flags = 0;
                }

                self.define_lazy_property(
                    ctx,
                    atom,
                    LazyInit::Prop(NonNull::from(e)),
                    prop_flags as _,
                )?;

                return Ok(());
            }
            NativeFieldDetail::GetSet { get, set } => unsafe {
                // XXX: use autoinit again ?
                return self
                    .define_property_getset(
                        ctx,
                        atom,
                        if let Some(f) = get {
                            ctx.create_native_function(f, &format!("get {}", e.name), 0, e.magic)?
                        } else {
                            JSValue::UNDEFINED
                        },
                        if let Some(f) = set {
                            ctx.create_native_function(f, &format!("set {}", e.name), 1, e.magic)?
                        } else {
                            JSValue::UNDEFINED
                        },
                        prop_flags as _,
                    )
                    .map(|_| ());
            },
            NativeFieldDetail::Str(_) | NativeFieldDetail::Object(_) => {
                return self.define_lazy_property(
                    ctx,
                    atom,
                    LazyInit::Prop(unsafe {
                        NonNull::new_unchecked(e as *const FieldDef as *mut FieldDef)
                    }),
                    prop_flags as _,
                );
            }

            NativeFieldDetail::Int32(i) => {
                val = JSValue::Int(i);
            }
            NativeFieldDetail::Float64(d) => {
                val = JSValue::Float64(d);
            }
            NativeFieldDetail::Undefined => {
                val = JSValue::UNDEFINED;
            }

            NativeFieldDetail::Alias { name, base } => {
                // using autoinit for aliases is not safe
                let name_atom = ctx.make_lazy_prop_atom(name)?.into_rc(ctx.rt());
                val = match base {
                    -1 => self.get_property(ctx, *name_atom)?,
                    0 => ctx.global_obj().get_property(ctx, *name_atom)?,
                    1 => ctx.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                        .get_property(ctx, *name_atom)?,
                    _ => {
                        unreachable!();
                    }
                };

                if atom == JS_ATOM_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == JS_ATOM_Symbol_hasInstance {
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
    pub fn install_field_defs(&self, ctx: &JSContext, entries: &[FieldDef]) -> Result<(), i32> {
        if let Self::Object(o) = self {
            o.install_field_defs(ctx, entries)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    // fn install_native_field_entry(
    //     &self,
    //     ctx: &JSContext,
    //     atom: JSAtom,
    //     e: &FieldDef,
    // ) -> Result<(), i32> {
    //     let val: JSValue;
    //     let mut prop_flags = e.prop_flags;

    //     match e.detail {
    //         NativeFieldDetail::Func { .. } => {
    //             if atom == JS_ATOM_Symbol_toPrimitive {
    //                 // Symbol.toPrimitive functions are not writable
    //                 prop_flags = JS_PROP_CONFIGURABLE as u8;
    //             } else if atom == JS_ATOM_Symbol_hasInstance {
    //                 // Function.prototype[Symbol.hasInstance] is not writable nor configurable
    //                 prop_flags = 0;
    //             }

    //             unsafe {
    //                 self.object_unchecked().define_lazy_property(
    //                     ctx,
    //                     atom,
    //                     LazyInit::Prop(NonNull::from(e)),
    //                     prop_flags as _,
    //                 )?;
    //             }

    //             return Ok(());
    //         }
    //         NativeFieldDetail::GetSet { get, set } => unsafe {
    //             // XXX: use autoinit again ?
    //             let getter = if let Some(f) = get {
    //                 ctx.create_native_function(f, &format!("get {}", e.name), 0, e.magic)?
    //             } else {
    //                 JSValue::UNDEFINED
    //             };
    //             let setter = if let Some(f) = set {
    //                 ctx.create_native_function(f, &format!("set {}", e.name), 1, e.magic)?
    //             } else {
    //                 JSValue::UNDEFINED
    //             };
    //             return self
    //                 .define_property_getset(ctx, atom, getter, setter, prop_flags as _)
    //                 .map(|_| ());
    //         },
    //         NativeFieldDetail::Str(_) | NativeFieldDetail::Object(_) => unsafe {
    //             self.object_unchecked()
    //                 .define_lazy_property(
    //                     ctx,
    //                     atom,
    //                     LazyInit::Prop(NonNull::new_unchecked(
    //                         e as *const FieldDef as *mut FieldDef,
    //                     )),
    //                     prop_flags as _,
    //                 )
    //                 .unwrap();
    //             return Ok(());
    //         },

    //         NativeFieldDetail::Int32(i) => {
    //             val = JSValue::Int(i);
    //         }
    //         NativeFieldDetail::Int64(i) => {
    //             val = JSValue::from(i);
    //         }
    //         NativeFieldDetail::Float64(d) => {
    //             val = JSValue::Float64(d);
    //         }
    //         NativeFieldDetail::Undefined => {
    //             val = JSValue::UNDEFINED;
    //         }

    //         NativeFieldDetail::Alias { name, base } => {
    //             // using autoinit for aliases is not safe
    //             let name_atom = ctx.make_prop_atom(name)?.into_rc(ctx.rt());
    //             val = match base {
    //                 -1 => self.get_property(ctx, *name_atom)?,
    //                 0 => ctx.global_obj().get_property(ctx, *name_atom)?,
    //                 1 => ctx.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
    //                     .get_property(ctx, *name_atom)?,
    //                 _ => {
    //                     unreachable!();
    //                 }
    //             };

    //             if atom == JS_ATOM_Symbol_toPrimitive {
    //                 // Symbol.toPrimitive functions are not writable
    //                 prop_flags = JS_PROP_CONFIGURABLE as u8;
    //             } else if atom == JS_ATOM_Symbol_hasInstance {
    //                 // Function.prototype[Symbol.hasInstance] is not writable nor configurable
    //                 prop_flags = 0;
    //             }
    //         }
    //     }

    //     let _ = self.define_property_value(ctx, atom, val, prop_flags as _);
    //     Ok(())
    // }
}

impl JSContext {
    /// create new or reuse existing atom for lazy init property
    fn make_lazy_prop_atom(&self, str: &str) -> Result<JSAtom, i32> {
        // format [xxxx]
        if str.starts_with('[') && str.ends_with(']') {
            let name2 = &str[1..str.len() - 1]; // chop heading '[', tailing ']'
            let rt = self.rt_mut();

            // We assume 8 bit non null strings, which is the case for these symbols
            for i in JS_ATOM_Symbol_toPrimitive.0..JS_ATOM_INIT_COUNT {
                if let Some(astr) = rt
                    .atom_slots
                    .get(i as usize)
                    .and_then(|p| unsafe { p.as_ref() })
                {
                    if astr.content().eq_str(name2) {
                        return Ok(JSAtom(i).dup(rt));
                    }
                }
            }

            self.throw_internal_error(&format!("unknown atom '{str}'"));
            return Err(-1);
        }

        if str.is_empty() || !str.chars().next().unwrap().is_ascii_digit() {
            // find in registry
            if let Some(atom) = JSAtom::find(self.rt(), str, AtomTypeHint::String) {
                return Ok(atom);
            }
        }

        JSAtom::new(self.rt_mut(), str)
    }
}

#[deprecated(note = "use JSContext::make_prop_atom () instead")]
pub unsafe fn find_atom(ctx: &JSContext, name: &str) -> JSAtom {
    ctx.make_lazy_prop_atom(name).unwrap_or(JS_ATOM_NULL)
}
