// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use gc_lite::GcNode;

use crate::{
    USER_CLASS_ID_START,
    atom::Atom,
    class::ClassID,
    context::{ContextPtr, ScopedContext},
    object::{Object, ObjectImpl, Shape},
    value::JSValue,
};

impl<'c> ScopedContext<'c> {
    /// Create new class instance with prototype `proto`, `class_id` and `payload` raw pointer.
    pub(crate) fn create_instance_with_proto_class(
        &mut self,
        proto: Option<Object>,
        class_id: ClassID,
        payload: *mut c_void,
    ) -> Result<Object, *mut c_void> {
        let shape = match self.rt().find_shape_by_proto(proto) {
            Some(sh) => sh,
            None => {
                let Ok(sh) = Shape::create(self, proto, 0) else {
                    return Err(payload);
                };
                sh
            }
        };

        ObjectImpl::create(self, Some(shape), class_id, payload)
    }

    #[inline(always)]
    pub(crate) fn create_instance_with_class(
        &mut self,
        class_id: ClassID,
        payload: *mut c_void,
    ) -> Result<Object, *mut c_void> {
        debug_assert!(
            class_id < USER_CLASS_ID_START,
            "create_instance_with_class is for builtin class only"
        );
        self.create_instance_with_proto_class(self.class_proto(class_id), class_id, payload)
    }

    pub(crate) fn ordinary_create_from_constructor_class(
        &mut self,
        ctor: JSValue,
        class_id: ClassID,
        payload: *mut c_void,
    ) -> Result<Object, *mut c_void> {
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
                return Err(payload);
            }
        } else {
            return Err(payload);
        };

        self.create_instance_with_proto_class(proto, class_id, payload)
    }

    pub fn create_class_instance_with_opaque(
        &mut self,
        new_target: JSValue,
        class_id: ClassID,
        opaque: *mut c_void,
    ) -> Result<Object, i32> {
        let other = Box::new(crate::class::OtherPayload { class_id, opaque });
        let payload = Box::into_raw(other) as *mut c_void;
        if class_id < USER_CLASS_ID_START {
            let _ = self.throw_internal_error("expect non-builtin class");
            Err(-1)
        } else {
            self.ordinary_create_from_constructor_class(new_target, class_id, payload)
                .map_err(|_| -1)
        }
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
        self.create_instance_with_proto_class(proto, crate::JS_CLASS_OBJECT, std::ptr::null_mut())
            .map_err(|_| -1)
    }

    #[inline(always)]
    pub fn create_base_object(&mut self) -> Result<Object, i32> {
        self.create_instance_with_class(crate::JS_CLASS_OBJECT, std::ptr::null_mut())
            .map_err(|_| -1)
    }
}
