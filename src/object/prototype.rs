use crate::{
    branch_hints::unlikely,
    context::JSContext,
    error::JSResult,
    object::JSObject,
    value::{JS_GetPrototypePrimitive, JSValue},
};

/// To iterate over shape proto chain of an object
pub struct ObjectShapeProtoIter<'a> {
    obj: Option<JSObject<'a>>,
}

impl<'a> Iterator for ObjectShapeProtoIter<'a> {
    type Item = JSObject<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(obj) = self.obj.take() {
            let proto = obj.shape().and_then(|s| s.0.borrow().proto.clone());
            self.obj = proto;
            Some(obj)
        } else {
            None
        }
    }
}

impl<'a> JSObject<'a> {
    /// get shape proto of object `self`
    #[inline(always)]
    pub fn shape_proto(&self) -> Option<JSObject<'a>> {
        self.shape().and_then(|s| s.0.borrow().proto.clone())
    }

    /// Get shape proto chain iterator, to iterate over the proto chain of an object.
    ///
    /// **Note: this iterator only concerns the `proto` field of shape, no proxy case is considered.**
    pub fn shape_proto_iter(&self, include_self: bool) -> ObjectShapeProtoIter<'a> {
        ObjectShapeProtoIter {
            obj: if include_self {
                Some(self.clone())
            } else {
                self.shape_proto()
            },
        }
    }

    /// Get prototype object of `self`, both shape and proxy are considered.
    ///
    /// Returns:
    /// - Some(object) if it has prototype, None for no prototype.
    /// - Err in case Proxy object calling getPrototypeOf() failed.
    pub fn prototype(&self, ctx: &JSContext<'a>) -> Result<Option<JSObject<'a>>, i32> {
        if let Some(p) = self.as_Proxy() {
            Ok(JSObject::try_from(p.get_prototype_of(ctx)?).ok())
        } else {
            Ok(self.shape_proto())
        }
    }

    /// Modify object `self`'s prototype.
    /// - `proto_val`: the new prototype, object or null
    /// - `throw_flag`: Whether to throw on error
    pub(crate) fn set_prototype_internal(
        &self,
        ctx: &JSContext<'a>,
        proto_val: JSValue<'a>,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        // Guard: can't set prototype of `Object.prototype`
        if unlikely(self.eq(&ctx.object_proto())) {
            return if throw_flag {
                ctx.throw_type_error("can't set prototype of Object.prototype");
                Err(-1)
            } else {
                Ok(false)
            };
        }

        let proto = if let JSValue::Object(o) = &proto_val {
            Some(o)
        } else if proto_val.is_null() {
            None
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        };

        if let Some(px) = self.as_Proxy() {
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

impl<'a> JSValue<'a> {
    /// Get prototype of value.
    /// Returns an object value if it has prototype, NULL for no prototype.
    /// Returns Err in case of Proxy object.
    pub fn prototype(&self, ctx: &JSContext<'a>) -> JSResult<'a> {
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
        ctx: &JSContext<'a>,
        proto_val: JSValue<'a>,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        if unlikely(throw_flag && self.is_undefined_or_null()) {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else if let JSValue::Object(o) = self {
            o.set_prototype_internal(ctx, proto_val, throw_flag)
        } else if throw_flag {
            Ok(true)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }
}
