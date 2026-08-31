// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcNode, GcTrace};

use crate::{
    USER_CLASS_ID_START,
    atom::Atom,
    class::ClassID,
    context::{ContextPtr, ScopedContext},
    object::{IntoPayloadPtr, Object, ObjectImpl, PayloadPtr, Shape},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    /// Create new class instance with prototype `proto`, `class_id` and `payload`.
    ///
    /// `payload` is consumed: on failure it is freed automatically.
    pub(crate) fn create_instance_with_proto_class(
        &mut self,
        proto: Option<Object>,
        class_id: ClassID,
        payload: impl IntoPayloadPtr,
    ) -> Result<Object, i32> {
        self.create_instance_using_proto(proto, payload.into_payload_ptr(class_id))
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
        self.create_instance(payload.into_payload_ptr(class_id))
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
        } else {
            let p = ctor.get_property(self, Atom::CONST_prototype)?;
            if let Some(o) = p.as_object() {
                Some(o)
            } else {
                let realm = {
                    let cx = ContextPtr::from(self.gc_ref());
                    ctor.as_object().map_or(Ok(cx), |o| o.realm(cx))?
                };
                realm.class_proto(class_id)
            }
        };

        self.create_instance_using_proto(proto, payload)
    }

    #[inline(always)]
    pub(crate) fn ordinary_create_from_constructor_class(
        &mut self,
        ctor: JSValue,
        class_id: ClassID,
        payload: impl IntoPayloadPtr,
    ) -> Result<Object, i32> {
        self.ordinary_create_from_constructor_payload(ctor, payload.into_payload_ptr(class_id))
    }

    /// Create new class instance with prototype `proto` and a pre-built payload.
    pub(crate) fn create_instance_using_proto(
        &mut self,
        proto: Option<Object>,
        payload: PayloadPtr,
    ) -> Result<Object, i32> {
        let shape = match self.rt().find_shape_by_proto(proto) {
            Some(sh) => sh,
            None => Shape::create(self, proto, 0)?,
        };
        ObjectImpl::create(self, Some(shape), payload)
    }

    /// Create new class instance with the class proto and a payload
    pub(crate) fn create_instance(&mut self, payload: PayloadPtr) -> Result<Object, i32> {
        let proto = self.class_proto(payload.class_id());
        self.create_instance_using_proto(proto, payload)
    }

    /// Create an instance owning a typed payload `P`.
    ///
    /// The engine takes ownership: on instance death the payload is freed via
    /// the monomorphized `drop_payload_fn::<P>` wired into the class's
    /// `payload_drop` (which `def_native_object!` registers), and traced via
    /// `user_payload_trace::<P>`. `P` must implement [`GcTrace`] (typically
    /// `#[derive(GcTrace)]`).
    pub fn create_class_instance_with_payload<P: GcTrace>(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
        payload: P,
    ) -> Result<Object, i32> {
        #[cfg(debug_assertions)]
        {
            let expected = self
                .rt()
                .class_registry
                .get(class_id as usize)
                .and_then(|cd| cd.payload_type_name);

            if let Some(expected) = expected {
                debug_assert!(
                    expected == std::any::type_name::<P>(),
                    "payload type mismatch for user class {class_id}: registry expects {expected}, got {}",
                    std::any::type_name::<P>(),
                );
            }
        }

        self.ordinary_create_from_constructor_payload(
            new_target,
            PayloadPtr::new(class_id, payload),
        )
    }

    /// Create a class instance without payload.
    #[inline(always)]
    pub fn create_class_instance(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
    ) -> Result<Object, i32> {
        self.ordinary_create_from_constructor_payload(new_target, PayloadPtr::none(class_id))
    }

    #[inline(always)]
    pub fn create_base_object_with_proto(&mut self, proto: Option<Object>) -> Result<Object, i32> {
        self.create_instance_using_proto(proto, PayloadPtr::none(crate::JS_CLASS_OBJECT))
    }

    #[inline(always)]
    pub fn create_base_object(&mut self) -> Result<Object, i32> {
        self.create_instance(PayloadPtr::none(crate::JS_CLASS_OBJECT))
    }
}
