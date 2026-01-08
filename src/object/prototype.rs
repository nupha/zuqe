use std::marker::PhantomData;

use crate::{
    context::{ContextImpl, ContextPtr},
    error::JSResult,
    object::{Object, ObjectImpl},
    utils::unlikely,
    value::{JS_GetPrototypePrimitive, JSValue},
};

/// To iterate over shape proto chain of an object
pub struct ObjectShapeProtoIter<'a> {
    obj: Option<Object>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for ObjectShapeProtoIter<'a> {
    type Item = Object;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(o) = self.obj.take() {
            self.obj = o.shape().and_then(|s| s.proto());
            Some(o)
        } else {
            None
        }
    }
}

impl ObjectImpl {
    /// get shape proto of `self`
    #[inline(always)]
    pub fn shape_proto(&self) -> Option<Object> {
        self.shape().and_then(|s| s.proto())
    }

    /// Get shape proto chain iterator, to iterate over the proto chain of an object.
    ///
    /// **Note: this iterator only concerns the Shape `proto` field, no proxy case is considered.**
    #[inline]
    pub fn shape_proto_iter<'a>(&self, include_self: bool) -> ObjectShapeProtoIter<'a> {
        ObjectShapeProtoIter {
            obj: if include_self {
                Some(unsafe { Object::from_ref_unchecked(self) })
            } else {
                self.shape_proto()
            },
            _mark: PhantomData,
        }
    }

    /// Get prototype object of `self`, both shape and proxy are considered.
    ///
    /// Returns:
    /// - Some(object) if object has prototype
    /// - None if object has no prototype.
    /// - Err(_) in case Proxy object calling getPrototypeOf() failed.
    pub fn prototype(&self, ctx: &ContextImpl) -> Result<Option<Object>, i32> {
        if self.flags().is_exotic()
            && let Some(cb) = ctx
                .rt_ref()
                .class_registry
                .get(self.class_id() as usize)
                .and_then(|c| c.exotic.as_deref())
                .and_then(|em| em.get_prototype)
        {
            // exotic: Proxy
            cb(
                unsafe { ContextPtr::from_ref_unchecked(ctx) },
                JSValue::Object(unsafe { Object::from_ref_unchecked(self) }),
            )
            .map(|v| Object::try_from(v).ok())
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
        let proto = if let JSValue::Object(p) = proto_val {
            Some(p)
        } else if proto_val.is_null() {
            None
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        };

        let obj = unsafe { Object::from_ref_unchecked(self) };

        if self.flags().is_exotic()
            && let Some(cb) = ctx
                .rt_ref()
                .class_registry
                .get(self.class_id() as usize)
                .and_then(|c| c.exotic.as_deref())
                .and_then(|em| em.set_prototype)
        {
            // exotic handler: Proxy
            let ret = cb(
                unsafe { ContextPtr::from_ref_unchecked(ctx) },
                JSValue::Object(obj),
                proto_val,
            );

            return if ret == Ok(false) && throw_flag {
                ctx.throw_type_error("proxy: bad prototype");
                Err(-1)
            } else {
                ret
            };
        }

        if self.shape().unwrap().proto == proto {
            return Ok(true);
        }

        let flags = self.flags();
        if flags.immutable_prototype() {
            return if throw_flag {
                ctx.throw_type_error("prototype is immutable");
                Err(-1)
            } else {
                Ok(false)
            };
        } else if !flags.extensible() {
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
