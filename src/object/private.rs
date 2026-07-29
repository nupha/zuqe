// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::{JS_ThrowTypeErrorPrivateNotFound, JSResult},
    object::{JSProperty, ObjectImpl},
    utils::unlikely,
    value::JSValue,
};

use super::JS_PROP_C_W_E;

impl ObjectImpl {
    /// Check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &mut ScopedContext, func: JSValue) -> Result<bool, i32> {
        let home_obj = func
            .as_object_BCWrapper()
            .and_then(|o| o.payload().home_object());

        if let Some(home_obj) = home_obj {
            if let Some((idx, _)) = home_obj.find_shape_property(Atom::CONST_Private_brand) {
                let atom = if let JSProperty::Normal(v) = home_obj.prop_slot(idx)
                    && let Some(s) = v.as_symbol(true)
                {
                    Atom::from_id(s.atom_info().unwrap().id())
                } else {
                    return Err(ctx.throw_type_error_not_an_object());
                };
                Ok(self.has_shape_property(atom))
            } else {
                Err(ctx.throw_type_error("expecting <brand> private field"))
            }
        } else {
            // Not a function object or can't find function's home object
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    fn set_private_field(
        &mut self,
        ctx: &mut ScopedContext,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        let Some(s) = name.as_symbol(true) else {
            return Err(ctx.throw_type_error_not_a_symbol());
        };

        let atom = Atom::from_id(s.atom_info().unwrap().id());
        if let Some((idx, _)) = self.find_shape_property(atom) {
            self.set_prop_slot(idx, JSProperty::Normal(val));
            Ok(())
        } else {
            Err(JS_ThrowTypeErrorPrivateNotFound(ctx, atom))
        }
    }

    fn get_private_field(&self, ctx: &mut ScopedContext, name: JSValue) -> JSResult {
        let Some(s) = name.as_symbol(true) else {
            return Err(ctx.throw_type_error_not_a_symbol());
        };

        let atom = Atom::from_id(s.atom_info().unwrap().id());
        if let Some((idx, _)) = self.find_shape_property(atom) {
            if let JSProperty::Normal(val) = self.prop_slot(idx) {
                Ok(*val)
            } else {
                unreachable!()
            }
        } else {
            Err(JS_ThrowTypeErrorPrivateNotFound(ctx, atom))
        }
    }

    /// Define a private field on this object.
    pub fn define_private_field(
        &mut self,
        ctx: &mut ScopedContext,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        let Some(s) = name.as_symbol(true) else {
            return Err(ctx.throw_type_error_not_a_symbol());
        };

        let a = Atom::from_id(s.atom_info().unwrap().id());

        // Per ECMAScript PrivateFieldAdd (O, P, value):
        //   1. If O.[[Extensible]] is false, throw a TypeError exception.
        if !self.is_extensible_obj(ctx)? {
            return Err(ctx.throw_type_error("private field on non-extensible object"));
        }

        if self.has_shape_property(a) {
            Err(ctx.throw_type_error(&format!(
                "private class field '{}' already exists",
                s.content().encode_utf8(false)
            )))
        } else {
            self.add_own_property(ctx, a, JS_PROP_C_W_E, JSProperty::Normal(val))
        }
    }
}

impl JSValue {
    /// Check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &mut ScopedContext, func: JSValue) -> Result<bool, i32> {
        if let Some(ob) = self.as_object() {
            ob.check_brand(ctx, func)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    /// Assign `home_val` a symbol as its identifier and store it in `home_val.<brand>`.
    /// If `self` is an object, define a property with the home object's symbol as key.
    pub fn add_brand(&mut self, ctx: &mut ScopedContext, mut home_val: JSValue) -> Result<(), i32> {
        if let Some(mut home) = home_val.as_object() {
            let brand_sym: JSValue = if let Some((idx, _)) =
                home.find_shape_property(Atom::CONST_Private_brand)
            {
                if let JSProperty::Normal(val) = home.prop_slot(idx) {
                    *val
                } else {
                    unreachable!()
                }
            } else {
                // Private <brand> property not present, adding it
                let br = ctx
                    .create_symbol_from_atom(Atom::CONST__brand_, crate::atom::AtomKind::Private)?;
                home.add_own_property(
                    ctx,
                    Atom::CONST_Private_brand,
                    JS_PROP_C_W_E,
                    JSProperty::Normal(br),
                )?;
                br
            };

            if let Some(mut ob) = self.as_object() {
                let brand_atom = brand_sym.to_atom(ctx).unwrap();
                if ob.has_shape_property(brand_atom) {
                    Err(ctx.throw_type_error("private method is already present"))
                } else if !ob.is_extensible_obj(ctx)? {
                    Err(ctx.throw_type_error("cannot add private fields to non-extensible object"))
                } else {
                    ob.add_own_property(
                        ctx,
                        brand_atom,
                        JS_PROP_C_W_E,
                        JSProperty::Normal(JSValue::UNDEFINED),
                    )
                }
            } else {
                Ok(())
            }
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    pub fn set_private_field(
        &mut self,
        ctx: &mut ScopedContext,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let Some(mut o) = self.as_object() {
            if unlikely(!o.flags().extensible()) {
                Err(ctx.throw_type_error("object is nonextensible for private field"))
            } else {
                o.set_private_field(ctx, name, val)
            }
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    pub fn get_private_field(&self, ctx: &mut ScopedContext, name: JSValue) -> JSResult {
        if let Some(ob) = self.as_object() {
            ob.get_private_field(ctx, name)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }

    /// Private fields can be added even on non-extensible objects or Proxies
    pub fn define_private_field(
        &mut self,
        ctx: &mut ScopedContext,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let Some(mut ob) = self.as_object() {
            ob.define_private_field(ctx, name, val)
        } else {
            Err(ctx.throw_type_error_not_an_object())
        }
    }
}
