use crate::{
    context::JSContext,
    error::JSResult,
    object::JSObject,
    value::{JS_GetPrototypePrimitive, JSValue},
};

/// To iterate over the prototype chain of an object
pub struct ObjectPrototypeIter {
    ctx: JSContext,
    cur_obj: Option<JSObject>,
}

impl Iterator for ObjectPrototypeIter {
    type Item = JSObject;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(obj) = self.cur_obj.take() {
            let proto = obj.shape().and_then(|s| s.0.borrow().proto.clone());
            self.cur_obj = proto;
            Some(obj)
        } else {
            None
        }
    }
}

impl JSObject {
    /// Get prototype object of `self`.
    /// Return an object if it has prototype, None for no prototype.
    /// Returns Err in case of Proxy object.
    pub fn prototype(&self, ctx: &JSContext) -> Result<Option<JSObject>, i32> {
        if let Some(p) = self.as_proxy() {
            Ok(JSObject::try_from(p.get_prototype_of(ctx)?).ok())
        } else {
            Ok(self.shape().and_then(|s| s.0.borrow().proto.clone()))
        }
    }

    /// get prototype chain walk iterator.
    /// To iterate over the prototype chain of an object.
    /// Note: only check the `proto` field of shape, no proxy considered.
    pub fn proto_iter(&self, ctx: &JSContext, include_self: bool) -> ObjectPrototypeIter {
        ObjectPrototypeIter {
            ctx: ctx.clone(),
            cur_obj: if include_self {
                Some(self.clone())
            } else {
                self.shape().and_then(|s| s.0.borrow().proto.clone())
            },
        }
    }

    /// Modify object `self`'s prototype.
    /// - `proto_val`: the new prototype, object or null
    /// - `throw_flag`: Whether to throw on error
    pub(crate) fn set_prototype_internal(
        &self,
        ctx: &JSContext,
        proto_val: JSValue,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        let proto = if let JSValue::Object(o) = &proto_val {
            Some(o)
        } else if proto_val.is_null() {
            None
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        };

        if let Some(px) = self.as_proxy() {
            return px.set_prototype_of(ctx, proto_val, throw_flag);
        }
        if self.shape().unwrap().0.borrow().proto.as_ref() == proto {
            return Ok(true);
        }

        if !self.flags().extensible() {
            if throw_flag {
                ctx.throw_type_error("object is not extensible");
                return Err(-1);
            } else {
                return Ok(false);
            }
        }

        if let Some(up) = proto {
            // check if there is a cycle
            let mut p1 = up.clone();

            loop {
                if p1.eq(self) {
                    if throw_flag {
                        ctx.throw_type_error("circular prototype chain");
                        return Err(-1);
                    } else {
                        return Ok(false);
                    }
                }

                // Note: for Proxy object, proto is NULL
                if let Some(p2) = p1.shape().and_then(|s| s.0.borrow().proto.clone()) {
                    p1 = p2;
                } else {
                    break;
                }
            }
        }

        self.shape_prepare_update(ctx)?;
        let sh = self.shape().unwrap();
        sh.0.borrow_mut().proto = proto.cloned();

        Ok(true)
    }
}

impl JSValue {
    /// Get prototype of value.
    /// Returns an object value if it has prototype, NULL for no prototype.
    /// Returns Err in case of Proxy object.
    pub fn prototype(&self, ctx: &JSContext) -> JSResult {
        if let Self::Object(o) = self {
            Ok(o.prototype(ctx)?.map_or(JSValue::NULL, JSValue::Object))
        } else {
            JS_GetPrototypePrimitive(ctx, self).err()
        }
    }

    /// Modify value `self`'s prototype.
    /// - `proto_val`: the new prototype, object or null
    /// - `throw_flag`: Whether to throw on error
    pub(crate) fn set_prototype_internal(
        &self,
        ctx: &JSContext,
        proto_val: JSValue,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        if throw_flag && self.is_undefined_or_null() {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        }

        if let JSValue::Object(p) = self {
            p.set_prototype_internal(ctx, proto_val, throw_flag)
        } else {
            if throw_flag {
                Ok(true)
            } else {
                ctx.throw_type_error_not_an_object();
                Err(-1)
            }
        }
    }
}
