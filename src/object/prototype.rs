// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::marker::PhantomData;

use gc_lite::GcNode;

use crate::{
    context::ScopedContext,
    error::JSResult,
    object::{Object, ObjectImpl},
    utils::unlikely,
    value::JSValue,
};

/// To iterate over shape proto chain of an object
pub struct ObjectShapeProtoIter<'a> {
    obj: Option<Object>,
    _mark: PhantomData<&'a ()>,
}

impl<'a> Iterator for ObjectShapeProtoIter<'a> {
    type Item = Object;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(o) = self.obj {
            self.obj = o.shape().and_then(|s| s.proto());
            Some(o)
        } else {
            None
        }
    }
}

impl ObjectImpl {
    /// Get the prototype cached in this shape. Fast, no context needed.
    /// Only works for normal objects; does NOT handle Proxy or other exotic objects.
    /// Used internally for shape-based operations where exotic objects are not expected.
    pub(crate) fn shape_proto(&self) -> Option<Object> {
        self.shape().and_then(|s| s.proto())
    }

    /// Get shape proto chain iterator, to iterate over the proto chain of an object.
    ///
    /// **Note: this iterator only concerns the Shape `proto` field, no proxy case is considered.**
    #[inline]
    pub(crate) fn shape_proto_iter<'a>(&self, include_self: bool) -> ObjectShapeProtoIter<'a> {
        ObjectShapeProtoIter {
            obj: if include_self {
                Some(Object::from(self.gc_ref()))
            } else {
                self.shape_proto()
            },
            _mark: PhantomData,
        }
    }

    /// Get prototype object of `self`, both shape and proxy are considered.
    ///
    /// # Description
    ///
    /// This method returns the actual prototype of the object, equivelent to `Object.getPrototypeOf()`.
    /// Handles all object types:
    ///
    /// - normal objects (via shape cache) and
    /// - exotic objects like Proxy (via class registry callback).
    /// - Returns Err if the exotic handler fails.
    ///
    /// This is the correct API for general-purpose prototype lookup.
    pub fn prototype(&self, ctx: &mut ScopedContext) -> Result<Option<Object>, i32> {
        if self.flags().is_exotic()
            && let Some(f) = ctx
                .rt()
                .class_registry
                .get(self.class_id() as usize)
                .and_then(|c| c.exotic)
                .and_then(|em| em.get_prototype)
        {
            // exotic: Proxy
            f(ctx, Object::from(self.gc_ref()).into()).map(|v| Object::try_from(v).ok())
        } else {
            Ok(self.shape_proto())
        }
    }

    /// Modify object `self`'s prototype.
    /// - `proto_val`: the new prototype, object or null
    /// - `throw_flag`: Whether to throw on error
    pub(crate) fn set_prototype_internal(
        &mut self,
        ctx: &mut ScopedContext,
        proto_val: JSValue,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        let proto = if let Some(&p) = proto_val.as_object() {
            Some(p)
        } else if proto_val.is_null() {
            None
        } else {
            ctx.throw_type_error_not_an_object();
            return Err(-1);
        };

        let obj = Object::from(self.gc_ref());

        if self.flags().is_exotic()
            && let Some(f) = ctx
                .rt()
                .class_registry
                .get(self.class_id() as usize)
                .and_then(|c| c.exotic)
                .and_then(|em| em.set_prototype)
        {
            // exotic handler: Proxy
            let ret = f(ctx, JSValue::from(obj), proto_val);
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

    /// Modify object `self`'s prototype.
    pub fn set_prototype(
        &mut self,
        ctx: &mut ScopedContext,
        proto_val: JSValue,
    ) -> Result<bool, i32> {
        self.set_prototype_internal(ctx, proto_val, true)
    }
}

impl JSValue {
    /// Get the actual prototype of the object, same as Object.getPrototypeOf().
    ///
    /// This is the correct API for general-purpose prototype lookup.
    /// Err case only happens for exotic objects like Proxy.
    pub fn prototype(&self, ctx: &mut ScopedContext) -> JSResult {
        if let Some(o) = self.as_object() {
            Ok(o.prototype(ctx)?.map_or(JSValue::NULL, JSValue::from))
        } else {
            self.get_prototype_primitive(ctx).err()
        }
    }

    /// Modify value `self`'s prototype.
    /// - `proto_val`: the new prototype, object or null
    /// - `throw_flag`: Whether to throw on error
    pub(crate) fn set_prototype_internal(
        &mut self,
        ctx: &mut ScopedContext,
        proto_val: JSValue,
        throw_flag: bool,
    ) -> Result<bool, i32> {
        if unlikely(throw_flag && self.is_undefined_or_null()) {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        } else if let Some(o) = self.as_object_mut() {
            o.set_prototype_internal(ctx, proto_val, throw_flag)
        } else if throw_flag {
            Ok(true)
        } else {
            ctx.throw_type_error_not_an_object();
            Err(-1)
        }
    }

    /// Modify value `self`'s prototype.
    pub fn set_prototype(
        &mut self,
        ctx: &mut ScopedContext,
        proto_val: JSValue,
    ) -> Result<bool, i32> {
        self.set_prototype_internal(ctx, proto_val, true)
    }
}
