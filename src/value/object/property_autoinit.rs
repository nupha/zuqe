use std::ptr::NonNull;

use crate::{
    atom::{JS_NewAtomString, JSAtom, consts::JS_ATOM_constructor},
    context::JSContext,
    error::JSResult,
    function::{
        JS_DEF_CFUNC, JS_DEF_OBJECT, JS_DEF_PROP_STRING, JS_NewCFunction2, JSCFunctionListEntry,
    },
    module::JSModuleDef,
    runtime::JSRuntime,
    value::{
        JSValue,
        object::{
            JS_PROP_AUTOINIT, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_TMASK, JS_PROP_WRITABLE,
            JSShapeProperty,
        },
    },
};

use super::{JSObject, JSProperty};

#[derive(Debug, Clone)]
#[repr(u8)]
pub enum AutoInitType {
    /// Build prototype and set as construstor for target object
    Prototype,
    /// Build module namespace object
    ModuleNS(JSModuleDef),
    /// Build base object installed with given entries.
    Prop(NonNull<JSCFunctionListEntry>),
}

impl AutoInitType {
    /// apply init. returns instantiated value.
    pub fn instantiate(&self, ctx: &JSContext, this_obj: &JSObject) -> JSResult {
        match self {
            Self::Prototype => {
                let obj = ctx.create_base_object()?;
                // set_cycle_flag(ctx, &obj);
                // set_cycle_flag(ctx, this_val);
                obj.define_property_value(
                    ctx,
                    JS_ATOM_constructor,
                    JSValue::Object(this_obj.clone()),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
                Ok(obj)
            }

            Self::ModuleNS(md) => md.ns(ctx),

            Self::Prop(entry) => {
                let entry = unsafe { entry.as_ref() };
                let val = match entry.def_type {
                    JS_DEF_CFUNC => JS_NewCFunction2(
                        ctx,
                        unsafe { entry.u.func.cfunc.generic },
                        entry.name,
                        unsafe { entry.u.func.length },
                        unsafe { entry.u.func.cproto },
                        entry.magic,
                    ),
                    JS_DEF_PROP_STRING => unsafe { JS_NewAtomString(ctx, entry.u.str) },
                    JS_DEF_OBJECT => {
                        let obj = ctx.create_base_object().unwrap();
                        obj.install_property_entries(ctx, unsafe { entry.u.prop_list.as_slice() })?;
                        obj
                    }
                    t => {
                        unreachable!("invalid def_type {t}");
                    }
                };

                Ok(val)
            }
        }
    }
}

#[deprecated]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JSAutoInitIDEnum {
    JS_AUTOINIT_ID_PROTOTYPE,
    JS_AUTOINIT_ID_MODULE_NS,
    JS_AUTOINIT_ID_PROP,
}

impl JSProperty {
    /// instantiate an AutoInit property.
    /// panic if property is not AutoInit type any more.
    pub(super) fn instantiate(&mut self, this_obj: &JSObject, prop: JSAtom) -> JSResult {
        if let JSProperty::AutoInit {
            realm,
            init_type,
            opaque,
        } = self
        {
            let realm = realm.upgrade().unwrap();

            this_obj.shape_prepare_update(&realm)?;
            let val = init_type.instantiate(&realm, this_obj);

            // reset AUTO INIT flag to NORMAL
            this_obj.shape().unwrap().with_mut(|s| {
                if let Some(prs) = s.prop_defs.get_mut(&prop.0) {
                    prs.set_flags(prs.flags() & (!JS_PROP_TMASK));
                } else {
                    println!(
                        "Fail to lazy instantiate property '{}': shape was sweeped",
                        prop.to_str(realm.rt())
                    );
                }
            });

            // set property normal value
            if let Ok(val) = val {
                *self = JSProperty::Normal(val.clone());
                Ok(val)
            } else {
                *self = JSProperty::Normal(JSValue::UNDEFINED);
                Err(-1)
            }
        } else {
            unreachable!("{:p}", self);
        }
    }
}

impl JSObject {
    /// define an AutoInit property on object
    pub fn define_auto_init_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        init_type: AutoInitType,
        flags: u32,
    ) -> Result<(), i32> {
        debug_assert!(
            !self.has_own_property(prop),
            "auto init property already exists: {} {:?}",
            prop.0,
            prop.to_str(ctx.rt())
        );

        self.add_own_property_then(
            ctx,
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_AUTOINIT,
            |pr| {
                *pr = JSProperty::AutoInit {
                    init_type,
                    realm: ctx.downgrade(),
                    opaque: std::ptr::null_mut(),
                };
            },
        )
    }
}

#[deprecated]
pub unsafe fn js_autoinit_get_id(pr: *const JSProperty) -> i32 {
    unreachable!("unuese");
}

#[deprecated(note = "use JSProperty::realm() instead")]
pub unsafe fn js_autoinit_get_realm(pr: &JSProperty) -> JSContext {
    if let JSProperty::AutoInit { realm, .. } = pr {
        realm.upgrade().unwrap()
    } else {
        unreachable!()
    }
}

#[deprecated]
pub unsafe fn js_autoinit_free(rt: &JSRuntime, pr: *const JSProperty) {}

/* warning: 'prs' is reallocated after it */
#[deprecated(note = "use JSProperty::auto_init() instead")]
pub unsafe fn JS_AutoInitProperty(
    ctx: &JSContext,
    p: &JSValue,
    prop: JSAtom,
    pr: &JSProperty,
    mut prs: *mut JSShapeProperty,
) -> i32 {
    unreachable!("unused");
}

// /// return the value associated to the autoinit property or an exception
// pub type JSAutoInitFunc =
//     fn(ctx: &JSContext, this_obj: &JSObject, atom: JSAtom, opaque: *mut c_void) -> JSResult;

// fn js_instantiate_prototype(
//     ctx: &JSContext,
//     this_val: &JSObject,
//     _: JSAtom,
//     _: *mut c_void,
// ) -> JSResult {
//     let obj = ctx.create_base_object()?;

//     // set_cycle_flag(ctx, &obj);
//     // set_cycle_flag(ctx, this_val);

//     obj.define_property_value(
//         ctx,
//         JS_ATOM_constructor,
//         JSValue::Object(this_val.clone()),
//         JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
//     )?;

//     Ok(obj)
// }

// fn js_module_ns_autoinit(
//     ctx: &JSContext,
//     _: &JSObject,
//     _: JSAtom,
//     opaque: *mut c_void,
// ) -> JSResult {
//     unsafe { (*opaque.cast::<JSModuleDef>()).ns(ctx) }
// }

// fn js_instantiate_entry(ctx: &JSContext, _: &JSObject, _: JSAtom, opaque: *mut c_void) -> JSResult {
//     let entry = unsafe { opaque.cast::<JSCFunctionListEntry>().as_ref().unwrap() };

//     let ret = match entry.def_type {
//         JS_DEF_CFUNC => JS_NewCFunction2(
//             ctx,
//             unsafe { entry.u.func.cfunc.generic },
//             entry.name,
//             unsafe { entry.u.func.length },
//             unsafe { entry.u.func.cproto },
//             entry.magic,
//         ),
//         JS_DEF_PROP_STRING => unsafe { JS_NewAtomString(ctx, entry.u.str) },
//         JS_DEF_OBJECT => {
//             let val = ctx.create_base_object().unwrap();
//             val.install_property_entries(ctx, unsafe {
//                 std::slice::from_raw_parts(entry.u.prop_list.tab, entry.u.prop_list.len as _)
//             })?;
//             val
//         }
//         t => {
//             unreachable!("invalid def_type {t}");
//         }
//     };

//     Ok(ret)
// }

// pub static js_autoinit_func_table: &[JSAutoInitFunc] = &[
//     js_instantiate_prototype, // JS_AUTOINIT_ID_PROTOTYPE
//     js_module_ns_autoinit,    // JS_AUTOINIT_ID_MODULE_NS
//     js_instantiate_entry,     // JS_AUTOINIT_ID_PROP
// ];
