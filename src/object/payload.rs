// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{context::ScopedContext, object::ObjectImpl, value::JSValue};

include!(concat!(env!("OUT_DIR"), "/class_payload.rs"));

impl ObjectImpl {
    /// get object's payload.
    /// if the object is not the required class_id (optional),
    /// returns None
    pub(crate) const fn retrive_payload0(&self, class_id: ClassID) -> Option<&ObjectPayload> {
        if class_id == self.class_id() {
            Some(self.payload())
        } else {
            None
        }
    }

    /// get object's payload.
    /// if the object is not the required class_id (optional),
    /// returns None
    pub(crate) const fn retrive_payload0_mut(
        &mut self,
        class_id: ClassID,
    ) -> Option<&mut ObjectPayload> {
        if class_id == self.class_id() {
            Some(self.payload_mut())
        } else {
            None
        }
    }

    /// get object's payload borrow ref.
    /// if the object is not the required class_id (optional),
    /// throws type error.
    pub fn retrive_payload(
        &self,
        ctx: &mut ScopedContext,
        class_id: ClassID,
    ) -> Result<&ObjectPayload, i32> {
        self.retrive_payload0(class_id).ok_or_else(|| ctx.throw_type_error_invalid_class(class_id))
    }

    /// get object's payload borrow ref.
    /// if the object is not the required class_id (optional),
    /// throws type error.
    pub(crate) fn retrive_payload_mut(
        &mut self,
        ctx: &mut ScopedContext,
        class_id: ClassID,
    ) -> Result<&mut ObjectPayload, i32> {
        self.retrive_payload0_mut(class_id).ok_or_else(|| ctx.throw_type_error_invalid_class(class_id))
    }
}

impl JSValue {
    /// if value is an object, then returns the object payload
    pub fn payload<'a>(&'a self) -> Option<&'a ObjectPayload> {
        self.as_object().map(|o| unsafe {
            let p = o.payload();
            std::mem::transmute::<_, &'a ObjectPayload>(p)
        })
    }

    /// if value is an object, then returns the object payload
    pub fn payload_mut<'a>(&'a mut self) -> Option<&'a mut ObjectPayload> {
        self.as_object().map(|mut o| unsafe {
            let p = o.payload_mut();
            std::mem::transmute::<_, &'a mut ObjectPayload>(p)
        })
    }

    /// get object value's payload.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub(crate) fn retrive_object_payload0(&self, class_id: ClassID) -> Option<&ObjectPayload> {
        if let Some(o) = self.as_object() {
            unsafe {
                std::mem::transmute::<Option<&ObjectPayload>, Option<&ObjectPayload>>(
                    o.retrive_payload0(class_id),
                )
            }
        } else {
            None
        }
    }

    /// get object value's payload borrow ref.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub fn retrive_object_payload(
        &self,
        ctx: &mut ScopedContext,
        class_id: ClassID,
    ) -> Result<&ObjectPayload, i32> {
        if let Some(o) = self.as_object() {
            unsafe {
                std::mem::transmute::<Result<&ObjectPayload, i32>, Result<&ObjectPayload, i32>>(
                    o.retrive_payload(ctx, class_id),
                )
            }
        } else {
            Err(ctx.throw_type_error_invalid_class(class_id))
        }
    }

    /// get object value's payload borrow ref.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub fn retrive_object_payload_mut<'a>(
        &'a mut self,
        ctx: &mut ScopedContext,
        class_id: ClassID,
    ) -> Result<&'a mut ObjectPayload, i32> {
        if let Some(mut o) = self.as_object() {
            o.retrive_payload_mut(ctx, class_id).map(|p| unsafe {
                std::mem::transmute::<&mut ObjectPayload, &'a mut ObjectPayload>(p)
            })
        } else {
            Err(ctx.throw_type_error_invalid_class(class_id))
        }
    }
}
