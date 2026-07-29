// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{context::ScopedContext, error::ErrCode, value::JSValue};

include!(concat!(env!("OUT_DIR"), "/class_payload.rs"));

/// Payload data types stored in `ObjectPayload` variants.
///
/// Implemented (via build_stdclass.rs codegen) for every payload data type,
/// mapping the type back to its variant accessor. Enables generic extraction
/// through [`JSValue::class_payload`] / [`JSValue::class_payload_mut`].
pub(crate) trait PayloadType {
    fn from_payload(p: &ObjectPayload) -> Option<&Self>;
    fn from_payload_mut(p: &mut ObjectPayload) -> Option<&mut Self>;
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

    /// Get the payload data `T` of a class instance value (no error thrown).
    ///
    /// Returns `None` if the value is not an object of `class_id`.
    ///
    /// # Panics
    /// Panics if `class_id` does not correspond to payload type `T` (a caller bug).
    pub(crate) fn try_class_payload<'a, T: PayloadType>(
        &'a self,
        class_id: ClassID,
    ) -> Option<&'a T> {
        let o = self.as_object()?;
        if o.class_id() != class_id {
            return None;
        }
        let p = unsafe { std::mem::transmute::<&ObjectPayload, &'a ObjectPayload>(o.payload()) };
        Some(T::from_payload(p).expect("class_id does not match payload type T"))
    }

    /// Get the payload data `T` of a class instance value (throws if not the target class).
    ///
    /// If the value is not an object of `class_id`, throws a type error via
    /// `throw_type_error_invalid_class`.
    ///
    /// # Panics
    /// Panics if `class_id` does not correspond to payload type `T` (a caller bug).
    pub(crate) fn class_payload<'a, T: PayloadType>(
        &'a self,
        ctx: &mut ScopedContext,
        class_id: ClassID,
    ) -> Result<&'a T, ErrCode> {
        self.try_class_payload(class_id)
            .ok_or_else(|| ctx.throw_type_error_invalid_class(class_id))
    }

    /// Get the mutable payload data `T` of a class instance value (throws if not the target class).
    ///
    /// If the value is not an object of `class_id`, throws a type error via
    /// `throw_type_error_invalid_class`.
    ///
    /// # Panics
    /// Panics if `class_id` does not correspond to payload type `T` (a caller bug).
    pub(crate) fn class_payload_mut<'a, T: PayloadType>(
        &'a mut self,
        ctx: &mut ScopedContext,
        class_id: ClassID,
    ) -> Result<&'a mut T, ErrCode> {
        if let Some(mut o) = self.as_object()
            && o.class_id() == class_id
        {
            let p = unsafe {
                std::mem::transmute::<&mut ObjectPayload, &'a mut ObjectPayload>(o.payload_mut())
            };
            Ok(T::from_payload_mut(p).expect("class_id does not match payload type T"))
        } else {
            Err(ctx.throw_type_error_invalid_class(class_id))
        }
    }
}
