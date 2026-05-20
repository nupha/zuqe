// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    Callback,
    atom::Atom,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{FieldDef, prop_str_to_atom},
    module::ModuleDef,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_WRITABLE, Object, ObjectImpl,
    },
    value::JSValue,
};

use super::JSProperty;

/// Lazy initialize a value
#[repr(u8)]
#[derive(Debug)]
pub enum LazyInit {
    /// Build prototype and set as the `constructor` field of target object
    Prototype,
    /// module namespace object
    ModuleNS(ModuleDef),
    /// base object with defined field entries
    Object(&'static [FieldDef]),
    /// value from calling a function
    ValueFn(fn(&mut ScopedContext) -> Result<JSValue, i32>),
    /// string value
    String(&'static str),
    /// function value
    Func {
        name: &'static str,
        callback: Callback,
        length: u8,
        magic: i16,
    },
}

impl GcTrace for LazyInit {
    fn trace(&self, tr: &mut GcTraceCtx) {
        if let Self::ModuleNS(ns) = self {
            ns.trace(tr);
        }
    }
}

impl LazyInit {
    /// apply lazy initialization. returns instantiated value.
    fn instantiate(&mut self, ctx: &mut ScopedContext, this_obj: Object) -> JSResult {
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
            Self::ValueFn(f) => f(ctx),
            Self::String(s) => {
                ctx.create_atom_string(s).map(JSValue::from)
                // why not create a value?
                // ctx.create_string_from_str(s).map(JSValue::from)
            }
            Self::Func {
                name,
                callback,
                length,
                magic,
            } => ctx
                .create_native_function(*callback, name, *length, *magic)
                .map(JSValue::from),
            Self::Object(entries) => {
                let mut obj = ctx.create_base_object()?;
                obj.install_fields(ctx, entries)?;
                Ok(obj.into())
            }
        }
    }
}

impl JSProperty {
    /// Instantiate lazy value.
    ///
    /// `this_obj` is the `this` bind used to call instantiate function.
    ///
    /// Note: will panic if property is not lazy type.
    pub(super) fn instantiate(self, this_obj: Object) -> JSResult {
        if let JSProperty::LazyInit {
            mut realm,
            mut init,
        } = self
        {
            realm
                .with_current_scope(|mut scope| {
                    let ret_val = init.instantiate(&mut scope, this_obj)?;
                    scope.set_return_value(ret_val);
                    Ok(ret_val)
                })
                .unwrap()
        } else {
            unreachable!("expect lazy property, but got {:?}", self);
        }
    }
}

impl ObjectImpl {
    /// define an lazy init property on object
    pub(crate) fn define_lazy_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        lazy_val: LazyInit,
        flags: u32,
    ) -> Result<&mut Self, i32> {
        debug_assert!(
            !self.has_shape_property(prop),
            "auto init property already exists: {} {:?}",
            prop.id(),
            prop.to_utf8(ctx.rt())
        );

        let realm = ContextPtr::from(ctx.gc_ref());

        self.add_own_property(
            ctx,
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_LAZYINIT,
            JSProperty::LazyInit {
                realm,
                init: lazy_val,
            },
        )?;
        Ok(self)
    }

    /// define a lazy init property with value builder function
    pub fn define_lazy_property_with(
        &mut self,
        ctx: &mut ScopedContext,
        prop: &str,
        builder: fn(&mut ScopedContext) -> Result<JSValue, i32>,
        flags: u32,
    ) -> Result<&mut Self, i32> {
        let atom = prop_str_to_atom(ctx, prop)?;
        self.define_lazy_property(ctx, atom, LazyInit::ValueFn(builder), flags)
    }
}
