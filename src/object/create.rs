// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use gc_lite::GcNode;

use crate::{
    USER_CLASS_ID_START,
    atom::Atom,
    class::ClassID,
    context::{ContextPtr, ScopedContext},
    object::{IntoPayloadPtr, Object, ObjectImpl, PayloadPtr, Shape},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    // ── value-payload family ────────────────────────────────────────────
    //
    // For raw payload values (bool, JSValue, data structs, ...) that do NOT
    // know their class: `class_id` appears exactly once at the call site and
    // is the single binding point (`IntoPayloadPtr` is implemented only for
    // registered payload types, so a pre-built `PayloadPtr` cannot enter
    // this family — use the `*_payload` family instead).

    /// Create new class instance with prototype `proto`, `class_id` and `payload`.
    ///
    /// `payload` is consumed: on failure it is freed automatically.
    pub(crate) fn create_instance_with_proto_class(
        &mut self,
        proto: Option<Object>,
        class_id: ClassID,
        payload: impl IntoPayloadPtr,
    ) -> Result<Object, i32> {
        self.create_instance_with_proto_payload(proto, payload.into_payload_ptr(class_id))
    }

    #[inline(always)]
    pub(crate) fn create_instance_with_class(
        &mut self,
        class_id: ClassID,
        payload: impl IntoPayloadPtr,
    ) -> Result<Object, i32> {
        debug_assert!(
            class_id < USER_CLASS_ID_START,
            "create_instance_with_class is for builtin class only"
        );
        self.create_instance_with_class_payload(payload.into_payload_ptr(class_id))
    }

    pub(crate) fn ordinary_create_from_constructor_class(
        &mut self,
        ctor: JSValue,
        class_id: ClassID,
        payload: impl IntoPayloadPtr,
    ) -> Result<Object, i32> {
        self.ordinary_create_from_constructor_payload(ctor, payload.into_payload_ptr(class_id))
    }

    // ── handle-payload family ───────────────────────────────────────────
    //
    // For pre-built `PayloadPtr` handles (incl. `none(class_id)` /
    // `other(class_id, ..)`): the class is carried by the handle, so these
    // signatures deliberately take no `class_id` — there is no second copy
    // of the information to get wrong.

    /// Create new class instance with prototype `proto` and a pre-built payload.
    pub(crate) fn create_instance_with_proto_payload(
        &mut self,
        proto: Option<Object>,
        payload: PayloadPtr,
    ) -> Result<Object, i32> {
        let shape = match self.rt().find_shape_by_proto(proto) {
            Some(sh) => sh,
            None => {
                let Ok(sh) = Shape::create(self, proto, 0) else {
                    return Err(-1);
                };
                sh
            }
        };

        ObjectImpl::create(self, Some(shape), payload)
    }

    /// Create new builtin class instance with the class proto and a
    /// pre-built payload (the class comes from the payload).
    pub(crate) fn create_instance_with_class_payload(
        &mut self,
        payload: PayloadPtr,
    ) -> Result<Object, i32> {
        debug_assert!(
            payload.class_id() < USER_CLASS_ID_START,
            "create_instance_with_class_payload is for builtin class only"
        );
        let proto = self.class_proto(payload.class_id());
        self.create_instance_with_proto_payload(proto, payload)
    }

    /// Create an instance whose prototype comes from `ctor.prototype`, with
    /// a pre-built payload (the class comes from the payload).
    pub(crate) fn ordinary_create_from_constructor_payload(
        &mut self,
        ctor: JSValue,
        payload: PayloadPtr,
    ) -> Result<Object, i32> {
        let class_id = payload.class_id();
        let proto: Option<Object> = if ctor.is_undefined() {
            self.class_proto(class_id)
        } else if let Ok(p) = ctor.get_property(self, Atom::CONST_prototype) {
            if let Some(o) = p.as_object() {
                Some(o)
            } else if let Ok(realm) = {
                let cx = ContextPtr::from(self.gc_ref());
                ctor.as_object().map_or(Ok(cx), |o| o.realm(cx))
            } {
                realm.class_proto(class_id)
            } else {
                return Err(-1);
            }
        } else {
            return Err(-1);
        };

        self.create_instance_with_proto_payload(proto, payload)
    }

    // ── public wrappers ─────────────────────────────────────────────────

    pub fn create_class_instance_with_opaque(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
        opaque: *mut c_void,
    ) -> Result<Object, i32> {
        if class_id < USER_CLASS_ID_START {
            let _ = self.throw_internal_error("expect non-builtin class");
            return Err(-1);
        }
        let payload = PayloadPtr::other(class_id, opaque);
        self.ordinary_create_from_constructor_payload(new_target, payload)
    }

    #[inline(always)]
    pub fn create_class_instance(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
    ) -> Result<Object, i32> {
        self.create_class_instance_with_opaque(new_target, class_id, std::ptr::null_mut())
    }

    #[inline(always)]
    pub fn create_base_object_with_proto(&mut self, proto: Option<Object>) -> Result<Object, i32> {
        self.create_instance_with_proto_payload(proto, PayloadPtr::none(crate::JS_CLASS_OBJECT))
            .map_err(|_| -1)
    }

    #[inline(always)]
    pub fn create_base_object(&mut self) -> Result<Object, i32> {
        self.create_instance_with_class_payload(PayloadPtr::none(crate::JS_CLASS_OBJECT))
            .map_err(|_| -1)
    }
}
