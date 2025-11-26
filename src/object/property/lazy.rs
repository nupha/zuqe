use std::ptr::NonNull;

use crate::{
    atom::Atom,
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{FieldDef, FieldDetail},
    module::ModuleDef,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_TMASK, JS_PROP_WRITABLE,
        ObjectRef,
    },
    value::JSValue,
};

use super::JSProperty;

#[repr(u8)]
#[derive(Debug)]
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
    pub fn instantiate(&mut self, ctx: &ContextImpl, this_obj: ObjectRef) -> JSResult {
        match self {
            Self::Prototype => {
                let mut obj = ctx.create_base_object()?;
                obj.define_property_value(
                    ctx,
                    Atom::CONST_constructor,
                    JSValue::Object(this_obj),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
                Ok(obj.into())
            }

            Self::ModuleNS(md) => md.ns(ctx).map(JSValue::Object),

            Self::Prop(field) => {
                let field = unsafe { field.as_ref() };
                let val: JSValue = match &field.detail {
                    FieldDetail::Func { handler, length } => ctx
                        .create_native_function(*handler, field.name, *length, field.magic)?
                        .into(),
                    FieldDetail::Str(s) => ctx.create_atom_string(s)?.into(),
                    FieldDetail::Object(list) => {
                        let mut obj = ctx.create_base_object()?;
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
    /// - `obj` is the object that owns this `self` property.
    /// - `this_obj` is the `this` bind used to call instantiate function.
    /// - `idx` is the index of the property in the shape.
    ///
    /// Note: will panic if property is not lazy type.
    pub(super) fn instantiate(
        &mut self,
        mut obj: ObjectRef,
        idx: usize,
        this_obj: ObjectRef,
    ) -> JSResult {
        if let JSProperty::LazyInit {
            realm, init_type, ..
        } = self
        {
            let realm = ContextRef::from(realm.upgrade(obj.rt().gc_heap()).unwrap());

            // modify shape property kind to NORMAL
            obj.modify_shape_property_index_flags(&realm, idx, |f| f & !JS_PROP_TMASK)?;

            let val = init_type.instantiate(&realm, this_obj);

            // set instantiated value back
            if let Ok(val) = val {
                *self = JSProperty::Normal(val);
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

impl ObjectRef {
    /// define an AutoInit property on object
    pub fn define_lazy_property(
        &mut self,
        ctx: &ContextImpl,
        prop: Atom,
        lazy_init: LazyInit,
        flags: u32,
    ) -> Result<(), i32> {
        debug_assert!(
            !self.has_shape_property(prop),
            "auto init property already exists: {} {:?}",
            prop.id(),
            prop.to_utf8()
        );

        self.add_own_property(
            ctx,
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_LAZYINIT,
            JSProperty::LazyInit {
                init_type: lazy_init,
                realm: unsafe { ContextRef::from_ref_unchecked(ctx).downgrade() },
                opaque: std::ptr::null_mut(),
            },
        )
    }
}
