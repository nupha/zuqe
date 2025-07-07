use std::ptr::NonNull;

use crate::{
    JS_CLASS_ARRAY,
    atom::{JS_FreeAtom, JSAtom, consts::*},
    context::JSContext,
    function::NativeFunctionType,
    object::{JS_PROP_CONFIGURABLE, LazyInit},
    value::{JSValue, JSValueConst},
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

pub struct NativeFieldDef {
    pub name: &'static str,
    pub detail: NativeFieldDetail,
    pub prop_flags: u8,
    pub magic: i16,
}

#[repr(u8)]
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
    Object(Vec<NativeFieldDef>),
    Str(&'static str),
    Int32(i32),
    Int64(i64),
    Float64(f64),
    Undefined,
}

impl JSValue {
    pub fn install_property_entries(
        &self,
        ctx: &JSContext,
        tab: &[NativeFieldDef],
    ) -> Result<(), i32> {
        for item in tab {
            let atom = ctx.create_atom(item.name)?;
            self.install_property_entry(ctx, atom, item)?;
            JS_FreeAtom(ctx, atom);
        }
        Ok(())
    }

    fn install_property_entry(
        &self,
        ctx: &JSContext,
        atom: JSAtom,
        e: &NativeFieldDef,
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

                unsafe {
                    self.object_unchecked().define_lazy_property(
                        ctx,
                        atom,
                        LazyInit::Prop(NonNull::from(e)),
                        prop_flags as _,
                    )?;
                }

                return Ok(());
            }
            NativeFieldDetail::GetSet { get, set } => unsafe {
                // XXX: use autoinit again ?
                let getter = if let Some(f) = get {
                    ctx.create_native_function(f, &format!("get {}", e.name), 0, e.magic)?
                } else {
                    JSValue::UNDEFINED
                };
                let setter = if let Some(f) = set {
                    ctx.create_native_function(f, &format!("set {}", e.name), 1, e.magic)?
                } else {
                    JSValue::UNDEFINED
                };

                self.define_property_getset(ctx, atom, getter, setter, prop_flags as _)?;
                return Ok(());
            },
            NativeFieldDetail::Str(_) | NativeFieldDetail::Object(_) => unsafe {
                self.object_unchecked()
                    .define_lazy_property(
                        ctx,
                        atom,
                        LazyInit::Prop(NonNull::new_unchecked(
                            e as *const NativeFieldDef as *mut NativeFieldDef,
                        )),
                        prop_flags as _,
                    )
                    .unwrap();
                return Ok(());
            },

            NativeFieldDetail::Int32(i) => {
                val = JSValue::Int(i);
            }
            NativeFieldDetail::Int64(i) => {
                val = JSValue::from(i);
            }
            NativeFieldDetail::Float64(d) => {
                val = JSValue::Float64(d);
            }
            NativeFieldDetail::Undefined => {
                val = JSValue::UNDEFINED;
            }

            NativeFieldDetail::Alias { name, base } => unsafe {
                // using autoinit for aliases is not safe
                let name_atom = ctx.create_atom(name)?;
                val = match base {
                    -1 => self.get_property(ctx, name_atom),
                    0 => ctx.global_obj().get_property(ctx, name_atom),
                    1 => ctx.0.borrow().class_proto[JS_CLASS_ARRAY as usize]
                        .get_property(ctx, name_atom),
                    _ => {
                        unreachable!();
                    }
                };
                JS_FreeAtom(ctx, name_atom);

                if atom == JS_ATOM_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == JS_ATOM_Symbol_hasInstance {
                    // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                    prop_flags = 0;
                }
            },
        }

        let _ = self.define_property_value(ctx, atom, val, prop_flags as _);
        Ok(())
    }
}

#[deprecated(note = "use JSValue::install_property_list() instead")]
pub unsafe fn JS_SetPropertyFunctionList(
    ctx: &JSContext,
    obj: &JSValueConst,
    tab: *const NativeFieldDef,
    len: usize,
) {
    unreachable!("unused");
    // obj.install_property_entries(ctx, std::slice::from_raw_parts(tab, len))
    //     .unwrap();
}

#[deprecated(note = "use JSValue::install_property_entries() instead")]
pub unsafe fn JS_InstantiateFunctionListItem(
    ctx: &JSContext,
    obj: &JSValueConst,
    atom: JSAtom,
    e: *const NativeFieldDef,
) -> i32 {
    unreachable!("unused");

    // let val: JSValue;
    // let mut prop_flags = (*e).prop_flags;

    // match (*e).def_type {
    //     JS_DEF_ALIAS => {
    //         // using autoinit for aliases is not safe
    //         let atom1 = ctx.create_atom((*e).u.alias.name).expect("TODO");

    //         val = match (*e).u.alias.base {
    //             -1 => obj.get_property(ctx, atom1),
    //             0 => ctx.global_obj().get_property(ctx, atom1),
    //             1 => ctx.0.borrow().class_proto[JS_CLASS_ARRAY as usize].get_property(ctx, atom1),
    //             _ => {
    //                 unreachable!();
    //             }
    //         };
    //         JS_FreeAtom(ctx, atom1);

    //         if atom == JS_ATOM_Symbol_toPrimitive {
    //             // Symbol.toPrimitive functions are not writable
    //             prop_flags = JS_PROP_CONFIGURABLE as u8;
    //         } else if atom == JS_ATOM_Symbol_hasInstance {
    //             // Function.prototype[Symbol.hasInstance] is not writable nor configurable
    //             prop_flags = 0;
    //         }
    //     }
    //     JS_DEF_CFUNC => {
    //         if atom == JS_ATOM_Symbol_toPrimitive {
    //             // Symbol.toPrimitive functions are not writable
    //             prop_flags = JS_PROP_CONFIGURABLE as u8;
    //         } else if atom == JS_ATOM_Symbol_hasInstance {
    //             // Function.prototype[Symbol.hasInstance] is not writable nor configurable
    //             prop_flags = 0;
    //         }

    //         obj.object_unchecked()
    //             .define_lazy_property(
    //                 ctx,
    //                 atom,
    //                 LazyInit::Prop(NonNull::new_unchecked(
    //                     e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry,
    //                 )),
    //                 // JSAutoInitIDEnum::JS_AUTOINIT_ID_PROP,
    //                 // (e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry).cast(),
    //                 prop_flags as _,
    //             )
    //             .unwrap();

    //         return 0;
    //     }
    //     JS_DEF_CGETSET | JS_DEF_CGETSET_MAGIC => {
    //         // XXX: use autoinit again ?
    //         let getter = if let Some(f) = (*e).u.getset.get {
    //             JS_NewCFunction2(
    //                 ctx,
    //                 f.generic,
    //                 &format!("get {}", (*e).name),
    //                 0,
    //                 if (*e).def_type == JS_DEF_CGETSET_MAGIC {
    //                     JSCFunctionEnum::GetterMagic
    //                 } else {
    //                     JSCFunctionEnum::Getter
    //                 },
    //                 (*e).magic,
    //             )
    //         } else {
    //             JSValue::UNDEFINED
    //         };

    //         let setter = if let Some(t) = (*e).u.getset.set {
    //             JS_NewCFunction2(
    //                 ctx,
    //                 t.generic,
    //                 &format!("set {}", (*e).name),
    //                 1,
    //                 if (*e).def_type == JS_DEF_CGETSET_MAGIC {
    //                     JSCFunctionEnum::SetterMagic
    //                 } else {
    //                     JSCFunctionEnum::Setter
    //                 },
    //                 (*e).magic,
    //             )
    //         } else {
    //             JSValue::UNDEFINED
    //         };

    //         obj.define_property_getset(ctx, atom, getter, setter, prop_flags as _)
    //             .unwrap();

    //         return 0;
    //     }
    //     JS_DEF_PROP_INT32 => {
    //         val = JSValue::from_i32((*e).u.i32);
    //     }
    //     JS_DEF_PROP_INT64 => {
    //         val = JSValue::from_i64((*e).u.i64);
    //     }
    //     JS_DEF_PROP_DOUBLE => {
    //         val = JSValue::from_f64_as_is((*e).u.f64);
    //     }
    //     JS_DEF_PROP_UNDEFINED => {
    //         val = JSValue::UNDEFINED;
    //     }
    //     JS_DEF_PROP_STRING | JS_DEF_OBJECT => {
    //         obj.object_unchecked()
    //             .define_lazy_property(
    //                 ctx,
    //                 atom,
    //                 LazyInit::Prop(NonNull::new_unchecked(
    //                     e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry,
    //                 )),
    //                 prop_flags as _,
    //             )
    //             .unwrap();
    //         return 0;
    //     }
    //     _ => {
    //         unreachable!();
    //     }
    // }

    // let _ = obj.define_property_value(ctx, atom, val, prop_flags as _);

    // 0
}
