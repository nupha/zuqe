use std::ops::Deref;

use crate::{
    JS_CLASS_PROXY,
    atom::{JSAtom, consts::*},
    context::JSContext,
    function::JS_Call,
    value::{JSValue, JSValueConst, object::JSObjectIsa},
};

// Value operators

impl JSValue {
    /// get typeof value, equivelent to js `typeof val`
    ///
    /// Note:
    /// returned atom is not cloned, if caller want keep reference, should clone it.
    pub fn type_of(&self) -> JSAtom {
        match self {
            Self::Int(_) | Self::Float64(_) => JS_ATOM_number,
            Self::Bool(_) => JS_ATOM_boolean,
            Self::String(_) => JS_ATOM_string,
            Self::Object(p) => {
                if p.flags().is_htmldda() {
                    JS_ATOM_undefined
                } else if self.is_callable() {
                    JS_ATOM_function
                } else {
                    JS_ATOM_object
                }
            }
            JSValue::NULL => JS_ATOM_object,
            Self::UNDEFINED => JS_ATOM_undefined,
            JSValue::Symbol(_) => JS_ATOM_symbol,
            _ => JS_ATOM_unknown,
        }
    }

    /// check if object value `self` is instance of type `ty`.
    /// equivelent to js: `self instanceof ty`
    pub(crate) fn ordinary_instance_of(&self, ctx: &JSContext, ty: &JSValue) -> Result<bool, i32> {
        if !self.is_callable() {
            return Ok(false);
        }

        let p = unsafe { self.object_unchecked() };
        if let JSObjectIsa::BOUND_FUNCTION(bfunc) = p.0.isa.borrow().deref() {
            unsafe {
                return JS_IsInstanceOf(ctx, ty, &bfunc.func_obj);
            }
        }

        // Only explicitly boxed values are instances of constructors
        if !ty.is_object() {
            return Ok(false);
        }

        let obj_proto = self.get_property(ctx, JS_ATOM_prototype);
        if !obj_proto.is_object() {
            if !obj_proto.is_exception() {
                ctx.JS_ThrowTypeError("operand 'prototype' property is not an object");
            }
            return Err(-1);
        }

        let proto = unsafe { obj_proto.object_unchecked() };
        let mut p = unsafe { ty.object_unchecked().clone() };

        loop {
            let up = p.shape().unwrap().0.borrow().proto.clone();
            if let Some(up) = up {
                p = up;
                if p.eq(proto) {
                    break Ok(true);
                }
            } else {
                let r = if p.class_id() == JS_CLASS_PROXY {
                    // slow case if proxy in the prototype chain
                    let obj1 = JSValue::Object(p);
                    loop {
                        let obj1 = obj1.prototype(ctx)?;
                        if obj1.is_null() {
                            break Ok(false);
                        }
                        if proto == unsafe { obj1.object_unchecked() } {
                            break Ok(true);
                        }

                        // must check for timeout to avoid infinite loop
                        ctx.poll_interrupts()?;
                    }
                } else {
                    Ok(false)
                };
                break r;
            }
        }
    }

    /// check if value `self` is instance of type `ty`.
    /// equivelent to js: `self instanceof ty`
    pub fn instance_of(&self, ctx: &JSContext, ty: &JSValue) -> Result<bool, i32> {
        if !self.is_object() {
            ctx.JS_ThrowTypeError("invalid 'instanceof' right operand");
            return Err(-1);
        }

        let method = self
            .get_property(ctx, JS_ATOM_Symbol_hasInstance)
            .check_exception()?;

        if !method.is_undefined_or_null() {
            let ret = JS_Call(ctx, &method, self, std::slice::from_ref(ty));
            return ret.coerce_to_bool(ctx);
        }

        // legacy case
        if !self.is_callable() {
            ctx.JS_ThrowTypeError("invalid 'instanceof' right operand");
            return Err(-1);
        }

        self.ordinary_instance_of(ctx, ty)
    }
}

#[deprecated(note = "use JSValue::ordinary_instance_of() instead")]
pub unsafe fn JS_OrdinaryIsInstanceOf(
    ctx: &JSContext,
    val: &JSValueConst,
    obj: &JSValueConst,
) -> Result<bool, i32> {
    obj.ordinary_instance_of(ctx, val)

    // if !obj.is_callable() {
    //     return Ok(false);
    // }

    // let p = obj.object_unchecked();

    // if let JSObjectIsa::BOUND_FUNCTION(bfunc) = (*p).isa() {
    //     let s = *bfunc;
    //     return JS_IsInstanceOf(ctx, val, (*s).func_obj);
    // }

    // // Only explicitly boxed values are instances of constructors
    // if JS_VALUE_GET_TAG(val) != JS_TAG_OBJECT {
    //     return Ok(false);
    // }
    // let obj_proto = obj.get_property(ctx, JS_ATOM_prototype);
    // if JS_VALUE_GET_TAG(obj_proto) != JS_TAG_OBJECT {
    //     if !obj_proto.is_exception() {
    //         ctx.as_mut()
    //             .unwrap()
    //             .JS_ThrowTypeError(c"operand 'prototype' property is not an object");
    //     }
    //     return Err(-1);
    // }

    // let proto = JS_VALUE_GET_OBJ(obj_proto);
    // let mut p = JS_VALUE_GET_OBJ(val);

    // let ret: Result<bool, i32> = loop {
    //     let proto1 = (*p).shape().unwrap().proto;
    //     if proto1.is_null() {
    //         let r = if unlikely((*p).class_id() == JS_CLASS_PROXY) {
    //             // slow case if proxy in the prototype chain
    //             let mut obj1 = JS_MKPTR!(JSTag::Object, p.cast()).dup();
    //             loop {
    //                 obj1 = JS_GetPrototypeFree(ctx, obj1);
    //                 if obj1.is_exception() {
    //                     break Err(-1);
    //                 } else if obj1.is_null() {
    //                     break Ok(false);
    //                 } else if proto == JS_VALUE_GET_OBJ(obj1) {
    //                     JS_FreeValue(ctx, obj1);
    //                     break Ok(true);
    //                 }

    //                 // must check for timeout to avoid infinite loop
    //                 if js_poll_interrupts(ctx) != 0 {
    //                     JS_FreeValue(ctx, obj1);
    //                     break Err(-1);
    //                 }
    //             }
    //         } else {
    //             Ok(false)
    //         };
    //         break r;
    //     } else {
    //         p = proto1;
    //         if proto == p {
    //             break Ok(true);
    //         }
    //     }
    // };

    // JS_FreeValue(ctx, &obj_proto);
    // ret
}

#[deprecated(note = "use JSValue::instance_of() instead")]
pub unsafe fn JS_IsInstanceOf(
    ctx: &JSContext,
    val: &JSValueConst,
    obj: &JSValueConst,
) -> Result<bool, i32> {
    obj.instance_of(ctx, val)

    // if !obj.is_object() {
    //     ctx.as_mut()
    //         .unwrap()
    //         .JS_ThrowTypeError(c"invalid 'instanceof' right operand");
    //     return Err(-1);
    // }

    // let method = obj.get_property(ctx, JS_ATOM_Symbol_hasInstance);
    // if method.is_exception() {
    //     return Err(-1);
    // }

    // if !method.is_undefined_or_null() {
    //     let mut val = val;
    //     let ret = JS_CallFree(ctx, method, obj, 1, &mut val);
    //     return JS_ToBoolFree(ctx, ret);
    // }

    // // legacy case
    // if !obj.is_callable() {
    //     ctx.as_mut()
    //         .unwrap()
    //         .JS_ThrowTypeError(c"invalid 'instanceof' right operand");
    //     return Err(-1);
    // }

    // JS_OrdinaryIsInstanceOf(ctx, val, obj)
}
