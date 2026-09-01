// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcNode, GcTrace};

use crate::{
    atom::Atom,
    class::ClassID,
    context::{ContextPtr, ScopedContext},
    object::{Object, ObjectImpl, PayloadPtr, Shape},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    /// Create new class instance with prototype `proto` and a pre-built payload.
    pub(crate) fn create_instance_with_proto(
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
        self.create_instance_with_proto(proto, payload)
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

        self.create_instance_with_proto(proto, payload)
    }

    /// Create base Object
    #[inline(always)]
    pub fn create_base_object(&mut self) -> Result<Object, i32> {
        self.create_instance(PayloadPtr::none(crate::JS_CLASS_OBJECT))
    }

    /// Create base Object with prototype `proto`
    #[inline(always)]
    pub fn create_base_object_with_proto(&mut self, proto: Option<Object>) -> Result<Object, i32> {
        self.create_instance_with_proto(proto, PayloadPtr::none(crate::JS_CLASS_OBJECT))
    }

    /// Create a class instance without payload.
    ///
    /// This is only for def_native_object! use. Don't call this directly.
    #[inline(always)]
    pub fn create_class_instance_no_payload(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
    ) -> Result<Object, i32> {
        self.ordinary_create_from_constructor_payload(new_target, PayloadPtr::none(class_id))
    }

    /// Create a class instance owning a typed payload `P`.
    ///
    /// This is only for def_native_object! use. Don't call this directly.
    pub fn create_class_instance<P: GcTrace>(
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
}
