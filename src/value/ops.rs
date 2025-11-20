use crate::{
    JS_CLASS_PROXY,
    atom::Atom,
    context::ContextImpl,
    object::{ObjectPayload, ObjectRef},
    value::JSValue,
};

impl ObjectRef {
    /// test if object `self` is instance of `ctor`.
    /// Equivalent to: `self instanceof ctor`
    pub(crate) fn ordinary_instance_of(
        &self,
        ctx: &ContextImpl,
        ctor: JSValue,
    ) -> Result<bool, i32> {
        if !ctor.is_function() {
            return Ok(false);
        }

        // ctor is sure to be object now.
        let ctor = unsafe { ctor.as_object_unchecked() };

        if let ObjectPayload::BOUND_FUNCTION(b) = ctor.payload() {
            // bound function case
            return self.instance_of(ctx, b.func_obj);
        }

        match ctor.get_property(ctx, Atom::CONST_prototype)? {
            JSValue::Object(proto) => {
                let mut p = *self;

                loop {
                    if let Some(up) = p.shape().unwrap().proto {
                        if up == proto {
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
    pub fn instance_of(&self, ctx: &ContextImpl, ctor: JSValue) -> Result<bool, i32> {
        if let Some(o) = ctor.as_object() {
            if let Some(r) =
                o.invoke_defined(ctx, Atom::CONST_Symbol_hasInstance, &[(*self).into()])?
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
    pub fn type_of(&self) -> Atom {
        match self {
            Self::Int(_) | Self::Float64(_) => Atom::CONST_number,
            Self::Bool(_) => Atom::CONST_boolean,
            Self::String(_) => Atom::CONST_string,
            Self::Object(p) => {
                if p.flags().is_htmldda() {
                    Atom::CONST_undefined
                } else if self.is_function() {
                    Atom::CONST_function
                } else {
                    Atom::CONST_object
                }
            }
            Self::NULL => Atom::CONST_object,
            Self::UNDEFINED => Atom::CONST_undefined,
            Self::Symbol(_) => Atom::CONST_symbol,
            _ => Atom::CONST_unknown,
        }
    }

    /// Test if value `self` is instance of `ctor`.
    /// Equivalent to `self instanceof ctor`
    pub fn instance_of(&self, ctx: &ContextImpl, ctor: JSValue) -> Result<bool, i32> {
        if let Some(o) = ctor.as_object() {
            if let Some(r) = o.invoke_defined(
                ctx,
                Atom::CONST_Symbol_hasInstance,
                std::slice::from_ref(self),
            )? {
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
