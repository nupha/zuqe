use std::ptr::NonNull;

use crate::{
    atom::{JSAtom, consts::JS_ATOM_constructor},
    context::JSContext,
    error::JSResult,
    function::{FieldDef, NativeFieldDetail},
    module::ModuleDef,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_TMASK, JS_PROP_WRITABLE,
        JSObject, JSShapeProperty,
    },
    runtime::JSRuntime,
    value::JSValue,
};

use super::JSProperty;

#[repr(u8)]
#[derive(Debug, Clone)]
pub enum LazyInit {
    /// Build prototype and set as constructor for target object
    Prototype,
    /// Build value from field def.
    Prop(NonNull<FieldDef>),
    /// Build module namespace object
    ModuleNS(ModuleDef),
}

impl LazyInit {
    /// apply init. returns instantiated value.
    pub fn instantiate(&self, ctx: &JSContext, this_obj: &JSObject) -> JSResult {
        match self {
            Self::Prototype => {
                let obj = ctx.create_base_object()?;
                // TODO: mark cyclic reference
                // set_cycle_flag(ctx, &obj);
                // set_cycle_flag(ctx, this_val);
                obj.define_property_value(
                    ctx,
                    JS_ATOM_constructor,
                    JSValue::Object(this_obj.clone()),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
                Ok(obj.into())
            }

            Self::ModuleNS(md) => md.ns(ctx).map(JSValue::Object),

            Self::Prop(field) => {
                let field = unsafe { field.as_ref() };
                let val = match &field.detail {
                    NativeFieldDetail::Func { handler, length } => {
                        ctx.create_native_function(*handler, field.name, *length, field.magic)?
                    }
                    NativeFieldDetail::Str(s) => ctx.create_atom_string(s)?.into(),
                    NativeFieldDetail::Object(list) => {
                        let obj = ctx.create_base_object()?;
                        obj.install_field_defs(ctx, list.as_ref())?;
                        obj.into()
                    }
                    _ => {
                        unreachable!();
                    }
                };

                Ok(val)
            }
        }
    }
}

impl JSProperty {
    /// instantiate lazy property.
    /// panic if property is not AutoInit type any more.
    pub(super) fn instantiate(
        &mut self,
        master_obj: &JSObject,
        prop: JSAtom,
        this_obj: &JSObject,
    ) -> JSResult {
        if let JSProperty::LazyInit {
            realm,
            init_type,
            opaque,
        } = self
        {
            let realm = realm.upgrade().unwrap();

            master_obj.shape_prepare_update(&realm)?;
            let val = init_type.instantiate(&realm, this_obj);

            // update property type from LAZY_INIT to NORMAL
            master_obj.shape().unwrap().with_mut(|s| {
                if let Some(prs) = s.prop_defs.get_mut(&prop.0) {
                    prs.set_flags(prs.flags() & !JS_PROP_TMASK);
                } else {
                    unreachable!();
                    // println!(
                    //     "Fail to instantiate lazy property '{}'",
                    //     prop.to_str(realm.rt())
                    // );
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
            unreachable!("expect lazy property, but got {:?}", self);
        }
    }
}

impl JSObject {
    /// define an AutoInit property on object
    pub fn define_lazy_property(
        &self,
        ctx: &JSContext,
        prop: JSAtom,
        lazy_init: LazyInit,
        flags: u32,
    ) -> Result<(), i32> {
        debug_assert!(
            !self.has_shape_property(prop),
            "auto init property already exists: {} {:?}",
            prop.0,
            prop.to_utf8(ctx.rt())
        );

        self.add_own_property(
            ctx,
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_LAZYINIT,
            JSProperty::LazyInit {
                init_type: lazy_init,
                realm: ctx.downgrade(),
                opaque: std::ptr::null_mut(),
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
    if let JSProperty::LazyInit { realm, .. } = pr {
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
//     unsafe { (*opaque.cast::<ModuleDef>()).ns(ctx) }
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
