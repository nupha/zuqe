use std::ops::Deref;

use crate::{
    JS_CLASS_PROXY,
    atom::{JSAtom, consts::*},
    context::JSContext,
    function::JS_Call,
    object::{JSObject, ObjectDetail},
    value::JSValue,
};

impl JSObject {
    /// check if object `self` is instance of type `ty`.
    /// equivelent to js: `self instanceof ty`
    pub(crate) fn ordinary_instance_of(&self, ctx: &JSContext, ty: &JSValue) -> Result<bool, i32> {
        if !self.is_callable() {
            return Ok(false);
        }

        if let ObjectDetail::BOUND_FUNCTION(bfunc) = self.0.payload.borrow().deref() {
            return bfunc.func_obj.instance_of(ctx, ty);
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

    /// check if object `self` is instance of type `ty`.
    /// equivelent to js: `self instanceof ty`
    pub fn instance_of(&self, ctx: &JSContext, ty: &JSValue) -> Result<bool, i32> {
        let method = self
            .get_property(ctx, JS_ATOM_Symbol_hasInstance)
            .check_exception()?;

        if !method.is_undefined_or_null() {
            let ret = JS_Call(
                ctx,
                &method,
                &JSValue::Object(self.clone()),
                std::slice::from_ref(ty),
            );
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
        if let Self::Object(p) = self {
            p.ordinary_instance_of(ctx, ty)
        } else {
            ctx.JS_ThrowTypeErrorNotAnObject();
            return Err(-1);
        }
    }

    /// check if value `self` is instance of type `ty`.
    /// equivelent to js: `self instanceof ty`
    pub fn instance_of(&self, ctx: &JSContext, ty: &JSValue) -> Result<bool, i32> {
        if let Self::Object(p) = self {
            p.instance_of(ctx, ty)
        } else {
            ctx.JS_ThrowTypeError("invalid 'instanceof' right operand");
            return Err(-1);
        }
    }
}
