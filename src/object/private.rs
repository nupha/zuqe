use crate::{
    atom::{JS_FreeAtom, JSAtomTypeHint, consts::*, js_symbol_to_atom},
    context::JSContext,
    error::{JS_ThrowTypeErrorPrivateNotFound, JSResult},
    object::{JSObject, JSProperty},
    value::{JS_NewSymbolFromAtom, JSValue},
};

use super::JS_PROP_C_W_E;

impl JSObject {
    /// check if `self` has own property suggested by `func.<home_object>.<brand>`.
    fn check_brand(&self, ctx: &JSContext, func: &JSValue) -> Result<bool, i32> {
        if let Some(home_obj) = func
            .object()
            .and_then(|o| o.0.isa.borrow().func().and_then(|f| f.home_object.clone()))
        {
            if let Some((idx, _)) = home_obj.find_shape_property(JS_ATOM_Private_brand) {
                match home_obj.0.prop_values.borrow().get(idx).cloned().unwrap() {
                    JSProperty::Normal(sym @ JSValue::Symbol(_)) => {
                        Ok(self.has_shape_property(js_symbol_to_atom(&sym)))
                    }
                    _ => {
                        ctx.JS_ThrowTypeErrorNotAnObject();
                        Err(-1)
                    }
                }
            } else {
                ctx.JS_ThrowTypeError("expecting <brand> private field");
                Err(-1)
            }
        } else {
            // func is not an object, or can't find its home object
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    fn set_private_field(&self, ctx: &JSContext, name: &JSValue, val: JSValue) -> Result<(), i32> {
        // name must be a Symbol
        if !name.is_symbol() {
            ctx.JS_ThrowTypeErrorNotASymbol();
            return Err(-1);
        }

        let prop = js_symbol_to_atom(name);

        if let Some((idx, _)) = self.find_shape_property(prop) {
            self.0.prop_values.borrow_mut()[idx] = JSProperty::Normal(val);
            Ok(())
        } else {
            JS_ThrowTypeErrorPrivateNotFound(ctx, prop);
            Err(-1)
        }
    }

    fn get_private_field(&self, ctx: &JSContext, name: &JSValue) -> JSResult {
        if !name.is_symbol() {
            ctx.JS_ThrowTypeErrorNotASymbol();
            Err(-1)
        } else {
            let prop = js_symbol_to_atom(name);
            if let Some((idx, _)) = self.find_shape_property(prop) {
                if let JSProperty::Normal(val) = self.0.prop_values.borrow().get(idx).unwrap() {
                    Ok(val.clone())
                } else {
                    unreachable!()
                }
            } else {
                JS_ThrowTypeErrorPrivateNotFound(ctx, prop);
                Err(-1)
            }
        }
    }

    // /// check if `self` has own property suggested by `func.<home_object>.<brand>`.
    // #[inline(always)]
    // pub fn check_brand(&self, ctx: &JSContext, func: &JSValue) -> Result<bool, i32> {
    //     self.0.borrow().check_brand(ctx, func)
    // }

    // #[inline(always)]
    // pub fn set_private_field(
    //     &self,
    //     ctx: &JSContext,
    //     name: &JSValue,
    //     val: JSValue,
    // ) -> Result<(), i32> {
    //     self.0.borrow_mut().set_private_field(ctx, name, val)
    // }

    // #[inline(always)]
    // pub fn get_private_field(&self, ctx: &JSContext, name: &JSValue) -> JSResult {
    //     self.0.borrow().get_private_field(ctx, name)
    // }

    /// Private fields can be added even on non extensible objects or Proxies
    pub fn define_private_field(
        &self,
        ctx: &JSContext,
        name: &JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if !name.is_symbol() {
            ctx.JS_ThrowTypeErrorNotASymbol();
            return Err(-1);
        }

        let prop = js_symbol_to_atom(name);

        if self.has_shape_property(prop) {
            ctx.JS_ThrowTypeError(&format!(
                "private class field '{}' already exists",
                prop.to_str(ctx.rt())
            ));
            Err(-1)
        } else {
            self.add_shape_property_then(ctx, prop, JS_PROP_C_W_E, |pr| {
                *pr = JSProperty::Normal(val);
            })
        }
    }
}

impl JSValue {
    /// check if `self` has own property suggested by `func.<home_object>.<brand>`.
    pub fn check_brand(&self, ctx: &JSContext, func: &JSValue) -> Result<bool, i32> {
        if let JSValue::Object(ob) = self {
            ob.check_brand(ctx, func)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    // add a private brand field to 'home_obj' if not already present and
    // if self is non-null object, add a private brand to it.
    pub fn add_brand(&self, ctx: &JSContext, home_obj: &JSValue) -> Result<(), i32> {
        if let JSValue::Object(home) = home_obj {
            let brand: JSValue = if let Some((idx, _)) =
                home.find_shape_property(JS_ATOM_Private_brand)
            {
                if let JSProperty::Normal(val) = home.0.prop_values.borrow().get(idx).unwrap() {
                    val.clone()
                } else {
                    unreachable!()
                }
            } else {
                // the private brand prop is not present, add it
                let br = unsafe {
                    JS_NewSymbolFromAtom(ctx, JS_ATOM_brand, JSAtomTypeHint::Private)
                        .check_exception()?
                };
                home.add_shape_property_then(ctx, JS_ATOM_Private_brand, JS_PROP_C_W_E, |pr| {
                    *pr = JSProperty::Normal(br.clone());
                })?;
                br
            };

            let brand_atom = js_symbol_to_atom(&brand);

            if let Some(p1) = self.object() {
                if p1.has_shape_property(brand_atom) {
                    JS_FreeAtom(ctx, brand_atom);
                    ctx.JS_ThrowTypeError("private method is already present");
                    Err(-1)
                } else {
                    let r = p1.add_shape_property_then(ctx, brand_atom, JS_PROP_C_W_E, |pr| {
                        *pr = JSProperty::Normal(JSValue::UNDEFINED);
                    });
                    JS_FreeAtom(ctx, brand_atom);
                    r
                }
            } else {
                JS_FreeAtom(ctx, brand_atom);
                Ok(())
            }
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    pub fn set_private_field(
        &self,
        ctx: &JSContext,
        name: &JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let JSValue::Object(ob) = self {
            ob.set_private_field(ctx, name, val)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    pub fn get_private_field(&self, ctx: &JSContext, name: &JSValue) -> JSResult {
        if let Self::Object(ob) = self {
            ob.get_private_field(ctx, name)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }

    /// Private fields can be added even on non extensible objects or Proxies
    pub fn define_private_field(
        &self,
        ctx: &JSContext,
        name: &JSValue,
        val: JSValue,
    ) -> Result<(), i32> {
        if let Self::Object(ob) = self {
            ob.define_private_field(ctx, name, val)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            Err(-1)
        }
    }
}

// check if the brand of the home object of 'func' is present on 'obj'
#[deprecated(note = "use JSValue::check_brand() instead")]
pub unsafe fn JS_CheckBrand(ctx: &JSContext, obj: &JSValue, func: &JSValue) -> Result<bool, i32> {
    obj.check_brand(ctx, func)
}

// add a private brand field to 'home_obj' if not already present and
// if obj is != null add a private brand to it
#[deprecated(note = "use JSValue::add_brand() instead")]
pub fn JS_AddBrand(ctx: &JSContext, obj: &JSValue, home_obj: &JSValue) -> Result<(), i32> {
    obj.add_brand(ctx, home_obj)

    // if let JSValue::Object(home) = home_obj {
    //     let brand = home
    //         .with_own_property(JS_ATOM_Private_brand, |_, pr| -> Result<JSValue, i32> {
    //             if let JSProperty::Value(val) = pr {
    //                 Ok(val.clone())
    //             } else {
    //                 unreachable!()
    //             }
    //         })
    //         .unwrap_or_else(|| {
    //             // if the brand is not present, add it
    //             let br = unsafe {
    //                 JS_NewSymbolFromAtom(ctx, JS_ATOM_brand, JSAtomTypeHint::Private)
    //                     .check_exception()?
    //             };
    //             home.add_property_then(ctx, JS_ATOM_Private_brand, JS_PROP_C_W_E, |pr| {
    //                 *pr = JSProperty::Value(br.clone());
    //             })?;
    //             Ok(br)
    //         })?;

    //     let brand_atom = js_symbol_to_atom(ctx, &brand);

    //     if let Some(p1) = obj.object() {
    //         if p1.has_own_property(brand_atom) {
    //             JS_FreeAtom(ctx, brand_atom);
    //             ctx.JS_ThrowTypeError(c"private method is already present");
    //             Err(-1)
    //         } else {
    //             let r = p1.add_property_then(ctx, brand_atom, JS_PROP_C_W_E, |pr| {
    //                 *pr = JSProperty::Value(JSValue::UNDEFINED);
    //             });
    //             JS_FreeAtom(ctx, brand_atom);
    //             r
    //         }
    //     } else {
    //         JS_FreeAtom(ctx, brand_atom);
    //         Ok(())
    //     }
    // } else {
    //     ctx.JS_ThrowTypeErrorNotAnObject();
    //     Err(-1)
    // }
}
