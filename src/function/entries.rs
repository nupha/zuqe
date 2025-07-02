use std::{mem::ManuallyDrop, ptr::NonNull};

use crate::{
    JS_CLASS_ARRAY,
    atom::{JS_FreeAtom, JSAtom, consts::*},
    context::JSContext,
    function::{JS_NewCFunction2, JSCFunctionEnum, JSCFunctionType},
    value::{
        JSValue, JSValueConst,
        object::{JS_PROP_CONFIGURABLE, LazyInit},
    },
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

pub struct JSCFunctionListEntry {
    pub name: &'static str,
    pub prop_flags: u8,
    pub def_type: u8,
    pub magic: i16,
    pub u: JSCFunctionListEntryUnion,
}

pub union JSCFunctionListEntryUnion {
    pub func: JSCFunctionListEntryFunc,
    pub getset: JSCFunctionListEntryGetSet,
    pub alias: JSCFunctionListEntryAlias,
    pub prop_list: ManuallyDrop<Vec<JSCFunctionListEntry>>, //JSCFunctionListEntryPropList,
    pub str: &'static str,
    pub i32: i32,
    pub i64: i64,
    pub f64: f64,
}

#[derive(Clone, Copy)]
pub struct JSCFunctionListEntryFunc {
    pub length: u8,
    pub cproto: JSCFunctionEnum,
    pub cfunc: JSCFunctionType,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct JSCFunctionListEntryGetSet {
    pub get: Option<JSCFunctionType>,
    pub set: Option<JSCFunctionType>,
}

#[derive(Copy, Clone)]
pub struct JSCFunctionListEntryAlias {
    pub name: &'static str,
    pub base: i32,
}

#[deprecated]
pub struct JSCFunctionListEntryPropList {
    pub tab: *const JSCFunctionListEntry,
    pub len: i32,
}

impl JSValue {
    pub fn install_property_entries(
        &self,
        ctx: &JSContext,
        tab: &[JSCFunctionListEntry],
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
        e: &JSCFunctionListEntry,
    ) -> Result<(), i32> {
        let val: JSValue;
        let mut prop_flags = e.prop_flags;

        match e.def_type {
            JS_DEF_CFUNC => {
                if atom == JS_ATOM_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == JS_ATOM_Symbol_hasInstance {
                    // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                    prop_flags = 0;
                }

                unsafe {
                    self.object_unchecked().define_auto_init_property(
                        ctx,
                        atom,
                        LazyInit::Prop(NonNull::from(e)),
                        prop_flags as _,
                    )?;
                }

                return Ok(());
            }
            JS_DEF_CGETSET | JS_DEF_CGETSET_MAGIC => unsafe {
                // XXX: use autoinit again ?
                //let mut buf: [c_char; 64] = [0; 64];

                let getter = if let Some(f) = e.u.getset.get {
                    JS_NewCFunction2(
                        ctx,
                        f.generic,
                        &format!("get {}", e.name),
                        0,
                        if e.def_type == JS_DEF_CGETSET_MAGIC {
                            JSCFunctionEnum::GetterMagic
                        } else {
                            JSCFunctionEnum::Getter
                        },
                        e.magic,
                    )
                } else {
                    JSValue::UNDEFINED
                };

                let setter = if let Some(t) = e.u.getset.set {
                    JS_NewCFunction2(
                        ctx,
                        t.generic,
                        &format!("set {}", e.name),
                        1,
                        if e.def_type == JS_DEF_CGETSET_MAGIC {
                            JSCFunctionEnum::SetterMagic
                        } else {
                            JSCFunctionEnum::Setter
                        },
                        (*e).magic,
                    )
                } else {
                    JSValue::UNDEFINED
                };

                self.define_property_getset(ctx, atom, getter, setter, prop_flags as _)?;
                return Ok(());
            },
            JS_DEF_ALIAS => unsafe {
                // using autoinit for aliases is not safe
                let atom1 = ctx.create_atom(e.u.alias.name)?;

                val = match e.u.alias.base {
                    -1 => self.get_property(ctx, atom1),
                    0 => ctx.global_obj().get_property(ctx, atom1),
                    1 => {
                        ctx.0.borrow().class_proto[JS_CLASS_ARRAY as usize].get_property(ctx, atom1)
                    }
                    _ => {
                        unreachable!();
                    }
                };
                JS_FreeAtom(ctx, atom1);

                if atom == JS_ATOM_Symbol_toPrimitive {
                    // Symbol.toPrimitive functions are not writable
                    prop_flags = JS_PROP_CONFIGURABLE as u8;
                } else if atom == JS_ATOM_Symbol_hasInstance {
                    // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                    prop_flags = 0;
                }
            },
            JS_DEF_PROP_INT32 => unsafe {
                val = JSValue::Int(e.u.i32);
            },
            JS_DEF_PROP_INT64 => unsafe {
                val = JSValue::from(e.u.i64);
            },
            JS_DEF_PROP_DOUBLE => unsafe {
                val = JSValue::Float64(e.u.f64);
            },
            JS_DEF_PROP_UNDEFINED => {
                val = JSValue::UNDEFINED;
            }
            JS_DEF_PROP_STRING | JS_DEF_OBJECT => unsafe {
                self.object_unchecked()
                    .define_auto_init_property(
                        ctx,
                        atom,
                        LazyInit::Prop(NonNull::new_unchecked(
                            e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry,
                        )),
                        // JSAutoInitIDEnum::JS_AUTOINIT_ID_PROP,
                        // (e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry).cast(),
                        prop_flags as _,
                    )
                    .unwrap();
                return Ok(());
            },
            _ => {
                unreachable!();
            }
        }

        let _ = self.define_property_value(ctx, atom, val, prop_flags as _);
        Ok(())
    }
}

#[deprecated(note = "use JSValue::install_property_list() instead")]
pub unsafe fn JS_SetPropertyFunctionList(
    ctx: &JSContext,
    obj: &JSValueConst,
    tab: *const JSCFunctionListEntry,
    len: usize,
) {
    obj.install_property_entries(ctx, std::slice::from_raw_parts(tab, len))
        .unwrap();
}

#[deprecated(note = "use JSValue::install_property_entries() instead")]
pub unsafe fn JS_InstantiateFunctionListItem(
    ctx: &JSContext,
    obj: &JSValueConst,
    atom: JSAtom,
    e: *const JSCFunctionListEntry,
) -> i32 {
    let val: JSValue;
    let mut prop_flags = (*e).prop_flags;

    match (*e).def_type {
        JS_DEF_ALIAS => {
            // using autoinit for aliases is not safe
            let atom1 = ctx.create_atom((*e).u.alias.name).expect("TODO");

            val = match (*e).u.alias.base {
                -1 => obj.get_property(ctx, atom1),
                0 => ctx.global_obj().get_property(ctx, atom1),
                1 => ctx.0.borrow().class_proto[JS_CLASS_ARRAY as usize].get_property(ctx, atom1),
                _ => {
                    unreachable!();
                }
            };
            JS_FreeAtom(ctx, atom1);

            if atom == JS_ATOM_Symbol_toPrimitive {
                // Symbol.toPrimitive functions are not writable
                prop_flags = JS_PROP_CONFIGURABLE as u8;
            } else if atom == JS_ATOM_Symbol_hasInstance {
                // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                prop_flags = 0;
            }
        }
        JS_DEF_CFUNC => {
            if atom == JS_ATOM_Symbol_toPrimitive {
                // Symbol.toPrimitive functions are not writable
                prop_flags = JS_PROP_CONFIGURABLE as u8;
            } else if atom == JS_ATOM_Symbol_hasInstance {
                // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                prop_flags = 0;
            }

            obj.object_unchecked()
                .define_auto_init_property(
                    ctx,
                    atom,
                    LazyInit::Prop(NonNull::new_unchecked(
                        e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry,
                    )),
                    // JSAutoInitIDEnum::JS_AUTOINIT_ID_PROP,
                    // (e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry).cast(),
                    prop_flags as _,
                )
                .unwrap();

            return 0;
        }
        JS_DEF_CGETSET | JS_DEF_CGETSET_MAGIC => {
            // XXX: use autoinit again ?
            let getter = if let Some(f) = (*e).u.getset.get {
                JS_NewCFunction2(
                    ctx,
                    f.generic,
                    &format!("get {}", (*e).name),
                    0,
                    if (*e).def_type == JS_DEF_CGETSET_MAGIC {
                        JSCFunctionEnum::GetterMagic
                    } else {
                        JSCFunctionEnum::Getter
                    },
                    (*e).magic,
                )
            } else {
                JSValue::UNDEFINED
            };

            let setter = if let Some(t) = (*e).u.getset.set {
                JS_NewCFunction2(
                    ctx,
                    t.generic,
                    &format!("set {}", (*e).name),
                    1,
                    if (*e).def_type == JS_DEF_CGETSET_MAGIC {
                        JSCFunctionEnum::SetterMagic
                    } else {
                        JSCFunctionEnum::Setter
                    },
                    (*e).magic,
                )
            } else {
                JSValue::UNDEFINED
            };

            obj.define_property_getset(ctx, atom, getter, setter, prop_flags as _)
                .unwrap();

            return 0;
        }
        JS_DEF_PROP_INT32 => {
            val = JSValue::from_i32((*e).u.i32);
        }
        JS_DEF_PROP_INT64 => {
            val = JSValue::from_i64((*e).u.i64);
        }
        JS_DEF_PROP_DOUBLE => {
            val = JSValue::from_f64_as_is((*e).u.f64);
        }
        JS_DEF_PROP_UNDEFINED => {
            val = JSValue::UNDEFINED;
        }
        JS_DEF_PROP_STRING | JS_DEF_OBJECT => {
            obj.object_unchecked()
                .define_auto_init_property(
                    ctx,
                    atom,
                    LazyInit::Prop(NonNull::new_unchecked(
                        e as *const JSCFunctionListEntry as *mut JSCFunctionListEntry,
                    )),
                    prop_flags as _,
                )
                .unwrap();
            return 0;
        }
        _ => {
            unreachable!();
        }
    }

    let _ = obj.define_property_value(ctx, atom, val, prop_flags as _);

    0
}
