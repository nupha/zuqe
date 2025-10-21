use crate::{
    JS_CLASS_PROXY,
    atom::{AtomRef, consts::*},
    context::JSContext,
    object::{JSObject, ObjectPayload},
    value::JSValue,
};

impl JSObject {
    /// test if object `self` is instance of `ctor`.
    /// Equivalent to: `self instanceof ctor`
    pub(crate) fn ordinary_instance_of(
        &self,
        ctx: &JSContext,
        ctor: &JSValue,
    ) -> Result<bool, i32> {
        if !ctor.is_function() {
            return Ok(false);
        }

        // ctor is sure to be object now.
        let ctor = unsafe { ctor.as_object_unchecked() };

        if let ObjectPayload::BOUND_FUNCTION(b) = ctor.payload() {
            // bound function case
            return self.instance_of(ctx, &b.func_obj);
        }

        match ctor.get_property(ctx, &JS_ATOM_prototype)? {
            JSValue::Object(proto) => {
                let mut p = self.clone();
                loop {
                    if let Some(up) = p.shape().unwrap().0.borrow().proto.clone() {
                        if up.eq(&proto) {
                            break Ok(true);
                        }
                        p = up;
                    } else {
                        let r = if p.class_id() == JS_CLASS_PROXY {
                            // slow case if proxy in the prototype chain
                            let mut obj1 = p;
                            'px_proto: loop {
                                if let Some(o) = obj1.prototype(ctx)? {
                                    if proto == o {
                                        break Ok(true);
                                    }
                                    obj1 = o;
                                    // must check for timeout to avoid infinite loop
                                    ctx.poll_interrupts()?;
                                } else {
                                    break 'px_proto Ok(false);
                                }
                            }
                        } else {
                            Ok(false)
                        };
                        break r;
                    }
                }
            }
            _ => {
                ctx.throw_type_error("operand 'prototype' property is not an object");
                Err(-1)
            }
        }
    }

    /// Test if value `self` is instance of `ctor`.
    /// Equivalent to `self instanceof ctor`
    pub fn instance_of(&self, ctx: &JSContext, ctor: &JSValue) -> Result<bool, i32> {
        if let Some(o) = ctor.as_object() {
            if let Some(r) =
                o.invoke_defined(ctx, &JS_ATOM_Symbol_hasInstance, &[self.clone().into()])?
            {
                return r.and_then(|v| v.to_bool());
            } else {
                // no Symbol.hasInstance method: check legacy way
                return if o.is_function() {
                    self.ordinary_instance_of(ctx, ctor)
                } else {
                    Ok(false)
                };
            }
        }

        ctx.throw_type_error("invalid 'instanceof' right operand");
        Err(-1)
    }
}

impl JSValue {
    /// get typeof value, equivalent to js `typeof val`
    ///
    /// Note:
    /// returned atom is not cloned, if caller want keep reference, should clone it.
    pub fn type_of(&self) -> AtomRef {
        match self {
            Self::Int(_) | Self::Float64(_) => JS_ATOM_number,
            Self::Bool(_) => JS_ATOM_boolean,
            Self::String(_) => JS_ATOM_string,
            Self::Object(p) => {
                if p.flags().is_htmldda() {
                    JS_ATOM_undefined
                } else if self.is_function() {
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

    /// Test if value `self` is instance of `ctor`.
    /// Equivalent to `self instanceof ctor`
    pub fn instance_of(&self, ctx: &JSContext, ctor: &JSValue) -> Result<bool, i32> {
        // if ctor.is_object() {
        //     let method = ctor.get_property(ctx, JS_ATOM_Symbol_hasInstance)?;
        //     if !method.is_undefined_or_null() {
        //         return method
        //             .call(ctx, &ctor, std::slice::from_ref(self))
        //             .and_then(|v| v.to_bool());
        //     }
        // }

        // if ctor.is_function() {
        //     // legacy case
        //     if let Some(o) = self.as_object() {
        //         o.ordinary_instance_of(ctx, ctor)
        //     } else {
        //         Ok(false)
        //     }
        // } else {
        //     ctx.throw_type_error("invalid 'instanceof' right operand");
        //     Err(-1)
        // }

        if let Some(o) = ctor.as_object() {
            if let Some(r) =
                o.invoke_defined(ctx, &JS_ATOM_Symbol_hasInstance, std::slice::from_ref(self))?
            {
                return r.and_then(|v| v.to_bool());
            } else {
                // no Symbol.hasInstance method: check legacy way
                if o.is_function() {
                    return if let Some(o) = self.as_object() {
                        o.ordinary_instance_of(ctx, ctor)
                    } else {
                        Ok(false)
                    };
                }
            }
        }

        ctx.throw_type_error("invalid 'instanceof' right operand");
        Err(-1)
    }
}
