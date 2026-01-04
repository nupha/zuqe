use crate::{
    atom::{Atom, AtomKind},
    context::ContextImpl,
    error::{JS_ThrowTypeErrorPrivateNotFound, JSResult},
    object::{JSProperty, Object},
    utils::unlikely,
    value::JSValue,
};

use super::JS_PROP_C_W_E;

impl Object {
    /// check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &ContextImpl, func: JSValue) -> Result<bool, i32> {
        if let Some(home_obj) = func
            .as_object_BCFunction()
            .and_then(|o| o.payload().home_object())
        {
            if let Some((idx, _)) = home_obj.find_shape_property(Atom::CONST_Private_brand) {
                let atom = if let JSProperty::Normal(JSValue::Symbol(a)) =
                    home_obj.prop_values.get(idx).unwrap()
                {
                    *a
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
            // not a function object, or can't find function's home object
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
        if let JSValue::Symbol(atom) = name {
            if let Some((idx, _)) = self.find_shape_property(atom) {
                self.prop_values[idx] = JSProperty::Normal(val);
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
        if let JSValue::Symbol(atom) = name {
            if let Some((idx, _)) = self.find_shape_property(atom) {
                if let JSProperty::Normal(val) = self.prop_values.get(idx).unwrap() {
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

    /// Private fields can be added even on non extensible objects or Proxies
    pub fn define_private_field(
        &mut self,
        ctx: &ContextImpl,
        name: JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let JSValue::Symbol(atom) = name {
            if self.has_shape_property(atom) {
                ctx.throw_type_error(&format!(
                    "private class field '{}' already exists",
                    atom.to_utf8()
                ));
                Err(-1)
            } else {
                self.add_own_property(ctx, atom, JS_PROP_C_W_E, JSProperty::Normal(val))
            }
        } else {
            ctx.throw_type_error_not_a_symbol();
            Err(-1)
        }
    }
}

impl JSValue {
    /// check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &ContextImpl, func: JSValue) -> Result<bool, i32> {
        if let JSValue::Object(ob) = self {
            ob.check_brand(ctx, func)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// assign 'home_obj' a symbol as its ident, store that symbol in home_obj.<brand> field.
    /// if self is an object, then defined a property with the home_obj's symbol as key.
    pub fn add_brand(&mut self, ctx: &ContextImpl, home_obj: JSValue) -> Result<(), i32> {
        if let JSValue::Object(mut home) = home_obj {
            let brand_sym: JSValue =
                if let Some((idx, _)) = home.find_shape_property(Atom::CONST_Private_brand) {
                    if let JSProperty::Normal(val) = home.prop_values.get(idx).unwrap() {
                        *val
                    } else {
                        unreachable!()
                    }
                } else {
                    // the private <brand> prop is not present, add it
                    let br = ctx.create_symbol_from_atom(Atom::CONST__brand_, AtomKind::Private)?;
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

    /// Private fields can be added even on non extensible objects or Proxies
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
