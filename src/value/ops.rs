// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    object::{Object, ObjectImpl},
    value::JSValue,
};

impl ObjectImpl {
    /// test if object `self` is instance of `ctor`.
    /// Equivalent to: `self instanceof ctor`
    pub(crate) fn ordinary_instance_of(
        &self,
        ctx: &mut ScopedContext,
        ctor: JSValue,
    ) -> Result<bool, i32> {
        if !ctor.is_callable() {
            return Ok(false);
        }

        // ctor is sure to be object now.
        let ctor = unsafe { ctor.as_object_unchecked() };

        if let Some(b) = ctor.payload_bound_function() {
            // bound function case
            return self.instance_of(ctx, b.func_obj.into());
        }

        let Some(proto) = ctor.get_property(ctx, Atom::CONST_prototype)?.as_object() else {
            return Err(ctx.throw_type_error("operand 'prototype' property is not an object"));
        };

        let mut p = Object::from_ref(self);
        loop {
            if let Some(up) = p.shape().unwrap().proto {
                if up == proto {
                    break Ok(true);
                }
                p = up;
            } else {
                let r = {
                    #[cfg(feature = "builtin_proxy")]
                    if p.class_id() == crate::JS_CLASS_PROXY {
                        // slow case if proxy in the prototype chain
                        let mut obj1 = p;
                        'px_proto: loop {
                            if let Some(o) = obj1.prototype(ctx)? {
                                if proto == o {
                                    break Ok(true);
                                }
                                obj1 = o;

                                // check to avoid infinite loop
                                if ctx.poll_interrupts() {
                                    return Err(-1);
                                }
                            } else {
                                break 'px_proto Ok(false);
                            }
                        }
                    } else {
                        Ok(false)
                    }

                    #[cfg(not(feature = "builtin_proxy"))]
                    Ok(false)
                };
                break r;
            }
        }
    }

    /// Test if value `self` is instance of `ctor`.
    /// Equivalent to `self instanceof ctor`
    pub fn instance_of(&self, ctx: &mut ScopedContext, ctor: JSValue) -> Result<bool, i32> {
        if let Some(o) = ctor.as_object() {
            if let Some(method) = o.get_method(ctx, Atom::CONST_Symbol_hasInstance)? {
                return method
                    .call(ctx, ctor, &[Object::from_ref(self).into()])
                    .and_then(|v| v.to_bool());
            }

            // no Symbol.hasInstance method: check legacy way
            return if o.is_callable() {
                self.ordinary_instance_of(ctx, ctor)
            } else {
                Err(ctx.throw_type_error("invalid 'instanceof' right operand"))
            };
        }

        Err(ctx.throw_type_error("invalid 'instanceof' right operand"))
    }
}

impl JSValue {
    /// Test if value `self` is instance of `ctor`.
    /// Equivalent to `self instanceof ctor`
    pub fn instance_of(&self, ctx: &mut ScopedContext, ctor: JSValue) -> Result<bool, i32> {
        if let Some(o) = ctor.as_object() {
            if let Some(method) = o.get_method(ctx, Atom::CONST_Symbol_hasInstance)? {
                return method
                    .call(ctx, ctor, std::slice::from_ref(self))
                    .and_then(|v| v.to_bool());
            }

            // no Symbol.hasInstance method: check legacy way
            if o.is_callable() {
                return if let Some(o) = self.as_object() {
                    o.ordinary_instance_of(ctx, ctor)
                } else {
                    Ok(false)
                };
            }
        }

        Err(ctx.throw_type_error("invalid 'instanceof' right operand"))
    }
}
