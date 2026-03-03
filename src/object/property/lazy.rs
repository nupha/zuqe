// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ptr::NonNull;

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    atom::Atom,
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    function::{FieldDef, FieldDetail},
    module::ModuleDef,
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_WRITABLE, Object},
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

impl GcTrace for LazyInit {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Self::ModuleNS(ns) = self {
            ns.trace(tr);
        }
    }
}

impl LazyInit {
    /// apply init. returns instantiated value.
    pub fn instantiate(&mut self, ctx: &mut ContextImpl, this_obj: Object) -> JSResult {
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
                    FieldDetail::Func { callback, length } => ctx
                        .create_native_function(*callback, field.name, *length, field.magic)?
                        .into(),
                    FieldDetail::Str(s) => ctx.create_atom_string(s)?.into(),
                    FieldDetail::Object(entries) => {
                        let mut obj = ctx.create_base_object()?;
                        obj.install_fields(ctx, entries)?;
                        obj.into()
                    }
                    _ => {
                        unreachable!("");
                    }
                };
                Ok(val)
            }
        }
    }
}

impl JSProperty {
    /// Instantiate lazy object, and returns instantiated value.
    ///
    /// `this_obj` is the `this` bind used to call instantiate function.
    ///
    /// Note: will panic if property is not lazy type.
    pub(super) fn instantiate(&mut self, _ctx: &ContextImpl, this_obj: Object) -> JSResult {
        if let JSProperty::LazyInit { realm, init_type } = self {
            init_type.instantiate(realm, this_obj)
        } else {
            unreachable!("expect lazy property, but got {:?}", self);
        }
    }
}

impl Object {
    /// define an AutoInit property on object
    pub fn define_lazy_property(
        &mut self,
        ctx: &mut ContextImpl,
        prop: Atom,
        lazy_init: LazyInit,
        flags: u32,
    ) -> Result<(), i32> {
        debug_assert!(
            !self.has_shape_property(prop),
            "auto init property already exists: {} {:?}",
            prop.id(),
            prop.to_utf8(ctx.rt_ref())
        );

        self.add_own_property(
            ctx,
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_LAZYINIT,
            JSProperty::LazyInit {
                realm: ContextPtr::from(ctx.gc_ref()),
                init_type: lazy_init,
            },
        )
    }
}
