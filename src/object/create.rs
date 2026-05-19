// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use gc_lite::GcNode;

use crate::{
    USER_CLASS_ID_START,
    atom::Atom,
    class::{ClassID, OtherPayload},
    context::{ContextPtr, ScopedContext},
    object::{Object, ObjectImpl, ObjectPayload, Shape},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    /// Create new class instance with prototype `proto` and `payload`:
    ///
    /// - New object will apply the shape determined by `proto`, if provided.
    /// - The `payload` determines what class the new object is.
    ///
    /// This method is the most fundamental way to instantiate a class.
    /// Higher-level methods (like `create_instance_with_payload` which provide sugars)
    /// are finally directed to this method.
    pub(crate) fn create_instance_with_proto_payload(
        &mut self,
        proto: Option<Object>,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        // find shape by prototype
        let shape = match self.rt().find_shape_by_proto(proto) {
            Some(sh) => sh,
            None => {
                let Ok(sh) = Shape::create(self, proto, 0) else {
                    return Err(payload);
                };
                sh
            }
        };

        ObjectImpl::create(self, Some(shape), payload)
    }

    /// Create an instance with `payload` data, applying payload's implied (default) class and proto.
    ///
    /// This method is for **builtin class only**. For user-defined (non-builtin) classes,
    /// use `create_class_instance_with_opaque` instead, which respects `newTarget`
    /// per ES `OrdinaryCreateFromConstructor`.
    #[inline(always)]
    pub(crate) fn create_instance_with_payload(
        &mut self,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        debug_assert!(
            payload.class_id() < USER_CLASS_ID_START,
            "create_instance_with_payload is for builtin class only; \
             use create_class_instance_with_opaque for user-defined classes"
        );
        self.create_instance_with_proto_payload(self.class_proto(payload.class_id()), payload)
    }

    /// create instance applying constructor `ctor`, with `payload` data.
    /// new object's class is implied by `payload` kind.
    ///
    /// the `ctor` could be:
    /// - undefined: use the `payload` kind associated class prototype,
    /// - otherwise retrieve property `ctor.prototype` for prototype.
    pub(crate) fn create_instance_with_ctor(
        &mut self,
        ctor: JSValue,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        let class_id = payload.class_id();

        let proto: Option<Object> = if ctor.is_undefined() {
            self.class_proto(class_id)
        } else if let Ok(p) = ctor.get_property(self, Atom::CONST_prototype) {
            if let Some(o) = p.as_object().copied() {
                Some(o)
            } else if let Ok(realm) = {
                let cx = ContextPtr::from(self.gc_ref());
                ctor.as_object().map_or(Ok(cx), |o| o.realm(cx))
            } {
                realm.class_proto(class_id)
            } else {
                return Err(payload);
            }
        } else {
            return Err(payload);
        };

        self.create_instance_with_proto_payload(proto, payload)
    }

    /// Create an instance of a user-defined (non-builtin) class with an attached raw pointer,
    /// respecting `newTarget.prototype` for the object's prototype chain.
    ///
    /// This is the `newTarget`-aware version of `create_class_instance_with_opaque`,
    /// analogous to how builtin constructors use `create_instance_with_ctor`.
    ///
    /// When called from a native constructor, pass `cc.this()` (which is `newTarget`)
    /// so that `class MySub extends NativeClass` creates instances with the correct
    /// prototype (`MySub.prototype` instead of `NativeClass.prototype`).
    ///
    /// - If `new_target` is undefined, falls back to the class's default prototype.
    /// - If `new_target.prototype` is an object, uses it as the instance prototype.
    /// - If `new_target.prototype` is not an object, falls back to the class's default prototype.
    pub fn create_class_instance_with_opaque(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
        opaque: *mut c_void,
    ) -> Result<Object, ObjectPayload> {
        if class_id < USER_CLASS_ID_START {
            self.throw_internal_error("expect non-builtin class");
            Err(ObjectPayload::OTHER(OtherPayload::new(class_id, opaque)))
        } else {
            self.create_instance_with_ctor(
                new_target,
                ObjectPayload::OTHER(OtherPayload::new(class_id, opaque)),
            )
        }
    }

    /// Create base Object with prototype `proto`.
    #[inline(always)]
    pub fn create_base_object_with_proto(&mut self, proto: Option<Object>) -> Result<Object, i32> {
        self.create_instance_with_proto_payload(proto, ObjectPayload::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new base Object using default Object prototype
    #[inline(always)]
    pub fn create_base_object(&mut self) -> Result<Object, i32> {
        self.create_instance_with_payload(ObjectPayload::OBJECT)
            .map_err(|_| -1)
    }
}
