use std::marker::PhantomData;

use crate::{
    context::ContextImpl,
    error::JSResult,
    object::{ObjectImpl, ObjectRef},
    utils::unlikely,
    value::{JS_GetPrototypePrimitive, JSValue},
};

/// To iterate over shape proto chain of an object
pub struct ObjectShapeProtoIter<'a> {
    obj: Option<ObjectRef>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for ObjectShapeProtoIter<'a> {
    type Item = ObjectRef;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(obj) = self.obj.take() {
            self.obj = obj.shape().and_then(|s| s.proto());
            Some(obj)
        } else {
            None
        }
    }
}

impl ObjectImpl {
    /// get shape proto of object `self`
    #[inline]
    pub fn shape_proto(&self) -> Option<ObjectRef> {
        self.shape().and_then(|s| s.proto())
    }

    /// Get shape proto chain iterator, to iterate over the proto chain of an object.
    ///
    /// **Note: this iterator only concerns the Shape `proto` field, no proxy case is considered.**
    pub fn shape_proto_iter<'a>(&self, include_self: bool) -> ObjectShapeProtoIter<'a> {
        ObjectShapeProtoIter {
            obj: if include_self {
                Some(unsafe { ObjectRef::from_ref_unchecked(self) })
            } else {
                self.shape_proto()
            },
            _mark: PhantomData,
        }
    }

    /// Get prototype object of `self`, both shape and proxy are considered.
    ///
    /// Returns:
    /// - Some(object) if it has prototype, None for no prototype.
    /// - Err in case Proxy object calling getPrototypeOf() failed.
    pub fn prototype(&self, ctx: &ContextImpl) -> Result<Option<ObjectRef>, i32> {
        let o = unsafe { ObjectRef::from_ref_unchecked(self) };
        if let Some(p) = o.as_Proxy() {
            Ok(ObjectRef::try_from(p.get_prototype_of(ctx)?).ok())
        } else {
            Ok(self.shape_proto())
        }
    }

    /// Modify object `self`'s prototype.
    /// - `proto_val`: the new prototype, object or null
    /// - `throw_flag`: Whether to throw on error
    pub(crate) fn set_prototype_internal(
        &mut self,
        ctx: &ContextImpl,
        proto_val: JSValue,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        // Guard: can't set prototype of `Object.prototype`
        if unlikely(self.id() == ctx.object_proto().id()) {
            return if throw_flag {
                ctx.throw_type_error("can't set prototype of Object.prototype");
                Err(-1)
            } else {
                Ok(false)
            };
        }

        let proto = if let JSValue::Object(o) = &proto_val {
            Some(*o)
        } else if proto_val.is_null() {
            None
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        };

        let mut o = unsafe { ObjectRef::from_ref_unchecked(self) };
        if let Some(px) = o.as_Proxy_mut() {
            return px.set_prototype_of(ctx, proto_val, throw_flag);
        }

        if self.shape().unwrap().proto == proto {
            return Ok(true);
        }

        if !self.flags().extensible() {
            return if throw_flag {
                ctx.throw_type_error("object is not extensible");
                Err(-1)
            } else {
                Ok(false)
            };
        }

        if let Some(up) = proto {
            // check if there is a cycle
            let mut p1 = up;

            loop {
                if p1.id() == self.id() {
                    if throw_flag {
                        ctx.throw_type_error("circular prototype chain");
                        return Err(-1);
                    } else {
                        return Ok(false);
                    }
                }

                // Note: for Proxy object, proto is NULL
                if let Some(p2) = p1.shape().and_then(|s| s.proto()) {
                    p1 = p2;
                } else {
                    break;
                }
            }
        }

        self.modify_shape(ctx, |sh| {
            sh.proto = proto;
        })?;

        Ok(true)
    }
}

impl JSValue {
    /// Get prototype of value.
    /// Returns an object value if it has prototype, NULL for no prototype.
    /// Returns Err in case of Proxy object.
    pub fn prototype(&self, ctx: &ContextImpl) -> JSResult {
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
        &mut self,
        ctx: &ContextImpl,
        proto_val: JSValue,
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
