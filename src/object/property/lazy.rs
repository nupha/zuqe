// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    Callback, ClassID,
    atom::Atom,
    context::{ContextPtr, ScopedContext},
    error::JSResult,
    function::{FieldDef, NativeFunction, prop_str_to_atom},
    module::ModuleDef,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_LAZYINIT, JS_PROP_WRITABLE, Object, ObjectImpl,
    },
    value::JSValue,
};

use super::{JSProperty, PropertyLazy};

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
        callback: Callback,
        length: u8,
        magic: i16,
    },
    /// class constructor by class id
    Class(fn() -> ClassID),
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
    fn instantiate(&mut self, ctx: &mut ScopedContext, prop: Atom, this_obj: Object) -> JSResult {
        match self {
            Self::Prototype => {
                let mut obj = ctx.create_base_object()?;
                obj.define_property_value(
                    ctx,
                    Atom::CONST_constructor,
                    JSValue::from(this_obj),
                    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                )?;
                Ok(obj.into())
            }
            Self::ModuleNS(md) => md.ns_object(ctx).map(JSValue::from),
            Self::ValueFn(f) => f(ctx),
            Self::String(s) => {
                ctx.create_string_interned(s).map(JSValue::make_string)
                // why not create a value?
                // ctx.create_string_from_str(s).map(JSValue::make_string)
            }

            Self::Func {
                callback,
                length,
                magic,
            } => NativeFunction::create(
                ctx,
                *callback,
                prop,
                *length,
                *magic,
                ctx.function_proto(),
                None,
            )
            .map(JSValue::from),

            Self::Class(f) => {
                let class_id = f();
                ctx.class_slots
                    .get(class_id as usize)
                    .and_then(|x| x.ctor)
                    .map(JSValue::from)
                    .ok_or_else(|| ctx.throw_internal_error(&format!(
                            "can't get constructor for class {class_id}: either class is not registered, not used or has no constructor"
                        )))
            }

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
    /// `this_obj` is used for LazyInit::Prototype case only, indicating the value of `constructor` field.
    ///
    /// Note: will panic if property is not lazy type.
    pub(super) fn instantiate(self, prop: Atom, this_obj: Object) -> JSResult {
        if let JSProperty::LazyInit(boxed) = self {
            let PropertyLazy {
                mut realm,
                mut init,
            } = *boxed;
            realm.with_current_scope(|mut cx| init.instantiate(&mut cx, prop, this_obj))
        } else {
            unreachable!();
        }
    }
}

impl ObjectImpl {
    /// Define a lazy-init property on an object.
    ///
    /// Lazy slots are appended, never merged: `add_own_property` grows the
    /// shape and writes at the new end. Defining a name the object already owns
    /// would therefore leave the shape and `prop_values` disagreeing about how
    /// many slots there are, so it is refused here rather than asserted. The
    /// object being freshly built is the only correct caller.
    ///
    /// Redefining an existing property is a different question and belongs to
    /// `define_property`, which consults `check_define_prop_flags`: a
    /// configurable property is replaced, a non-configurable one is refused.
    pub(crate) fn define_lazy_property(
        &mut self,
        ctx: &mut ScopedContext,
        prop: Atom,
        lazy_val: LazyInit,
        flags: u32,
    ) -> Result<&mut Self, i32> {
        if self.has_shape_property(prop) {
            return Err(ctx.throw_type_error(&format!(
                "cannot define lazy property '{}': already exists",
                prop.to_utf8(ctx.rt())
            )));
        }

        let realm = ContextPtr::from(ctx.gc_ref());

        self.add_own_property(
            ctx,
            prop,
            (flags & JS_PROP_C_W_E) | JS_PROP_LAZYINIT,
            JSProperty::LazyInit(Box::new(PropertyLazy {
                realm,
                init: lazy_val,
            })),
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
