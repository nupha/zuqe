use std::ptr::NonNull;

use crate::{
    atom::{JSAtom, consts::JS_ATOM_constructor},
    context::JSContext,
    error::JSResult,
    function::{FieldDef, NativeFieldDetail},
    module::ModuleDef,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_TMASK, JS_PROP_WRITABLE,
        JSObject,
    },
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
    /// Instantiate lazy object, and set instantiated value back to the property value slot as normal value,
    /// change shape property kind to NORMAL.
    ///
    /// - `master_obj` is the object that owns this `self` property.
    /// - `this_obj` is the `this` bind used to call instantiate function.
    /// - `idx` is the index of the property in the shape.
    ///
    /// Note: will panic if property is not lazy type.
    pub(super) fn instantiate(
        &mut self,
        master_obj: &JSObject,
        idx: usize,
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

            // modify shape property kind to NORMAL
            master_obj.shape().unwrap().with_mut(|sh| {
                if let Some((_, prs)) = sh.prop_defs.get_index_mut(idx) {
                    prs.set_flags(prs.flags() & !JS_PROP_TMASK);
                } else {
                    unreachable!();
                }
            });

            // set instantiated value back
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
