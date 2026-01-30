use crate::{
    atom::Atom,
    context::ContextImpl,
    error::{JS_ThrowTypeErrorPrivateNotFound, JSResult},
    object::{JSProperty, Object},
    utils::unlikely,
    value::JSValue,
};

use super::JS_PROP_C_W_E;

impl Object {
    /// Check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &ContextImpl, func: JSValue) -> Result<bool, i32> {
        if let Some(home_obj) = func
            .as_object_BCFunction()
            .and_then(|o| o.payload().home_object())
        {
            if let Some((idx, _)) = home_obj.find_shape_property(Atom::CONST_Private_brand) {
                let atom = if let JSProperty::Normal(JSValue::Symbol(s)) = home_obj.prop_slot(idx) {
                    Atom::from_id(s.atom_info().unwrap().id())
                } else {
                    ctx.throw_type_error_not_an_object();
                    return Err(-1);
                };
                Ok(self.has_shape_property(atom))
            } else {
                ctx.throw_type_error("expecting <brand> private field");
                Err(-1)
            }
        } else {
            // Not a function object or can't find function's home object
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    fn set_private_field(
        &mut self,
        ctx: &ContextImpl,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let JSValue::Symbol(s) = name {
            let atom = Atom::from_id(s.atom_info().unwrap().id());

            if let Some((idx, _)) = self.find_shape_property(atom) {
                self.set_prop_slot(ctx, idx, JSProperty::Normal(val));
                Ok(())
            } else {
                JS_ThrowTypeErrorPrivateNotFound(ctx, atom);
                Err(-1)
            }
        } else {
            ctx.throw_type_error_not_a_symbol();
            Err(-1)
        }
    }

    fn get_private_field(&self, ctx: &ContextImpl, name: JSValue) -> JSResult {
        if let JSValue::Symbol(s) = name {
            let atom = Atom::from_id(s.atom_info().unwrap().id());

            if let Some((idx, _)) = self.find_shape_property(atom) {
                if let JSProperty::Normal(val) = self.prop_slot(idx) {
                    Ok(*val)
                } else {
                    unreachable!()
                }
            } else {
                JS_ThrowTypeErrorPrivateNotFound(ctx, atom);
                Err(-1)
            }
        } else {
            ctx.throw_type_error_not_a_symbol();
            Err(-1)
        }
    }

    /// Private fields can be added even on non-extensible objects or Proxies
    pub fn define_private_field(
        &mut self,
        ctx: &ContextImpl,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let JSValue::Symbol(s) = name {
            let a = Atom::from_id(s.atom_info().unwrap().id());

            if self.has_shape_property(a) {
                ctx.throw_type_error(&format!(
                    "private class field '{}' already exists",
                    s.content().encode_utf8(false)
                ));
                Err(-1)
            } else {
                self.add_own_property(ctx, a, JS_PROP_C_W_E, JSProperty::Normal(val))
            }
        } else {
            ctx.throw_type_error_not_a_symbol();
            Err(-1)
        }
    }
}

impl JSValue {
    /// Check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &ContextImpl, func: JSValue) -> Result<bool, i32> {
        if let JSValue::Object(ob) = self {
            ob.check_brand(ctx, func)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// Assign `home_obj` a symbol as its identifier and store it in `home_obj.<brand>`.
    /// If `self` is an object, define a property with the home object's symbol as key.
    pub fn add_brand(&mut self, ctx: &ContextImpl, home_obj: JSValue) -> Result<(), i32> {
        if let JSValue::Object(mut home) = home_obj {
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

            if let Some(ob) = self.as_object_mut() {
                let brand_atom = brand_sym.to_atom(ctx).unwrap();
                if ob.has_shape_property(brand_atom) {
                    ctx.throw_type_error("private method is already present");
                    Err(-1)
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
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    pub fn set_private_field(
        &mut self,
        ctx: &ContextImpl,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let JSValue::Object(o) = self {
            if unlikely(!o.flags().extensible()) {
                ctx.throw_type_error("object is nonextensible for private field");
                Err(-1)
            } else {
                o.set_private_field(ctx, name, val)
            }
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    pub fn get_private_field(&self, ctx: &ContextImpl, name: JSValue) -> JSResult {
        if let Self::Object(ob) = self {
            ob.get_private_field(ctx, name)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// Private fields can be added even on non-extensible objects or Proxies
    pub fn define_private_field(
        &mut self,
        ctx: &ContextImpl,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let Self::Object(ob) = self {
            ob.define_private_field(ctx, name, val)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}
