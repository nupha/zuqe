use crate::{context::ContextImpl, object::ObjectImpl, value::JSValue};

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
        ctx: &ContextImpl,
        class_id: ClassID,
    ) -> Result<&ObjectPayload, i32> {
        self.retrive_payload0(class_id).ok_or_else(|| {
            ctx.throw_type_error_invalid_class(class_id);
            -1
        })
    }

    /// get object's payload borrow ref.
    /// if the object is not the required class_id (optional),
    /// throws type error.
    pub(crate) fn retrive_payload_mut(
        &mut self,
        ctx: &ContextImpl,
        class_id: ClassID,
    ) -> Result<&mut ObjectPayload, i32> {
        self.retrive_payload0_mut(class_id).ok_or_else(|| {
            ctx.throw_type_error_invalid_class(class_id);
            -1
        })
    }
}

impl JSValue {
    /// get object value's payload.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub(crate) fn retrive_object_payload0(&self, class_id: ClassID) -> Option<&ObjectPayload> {
        if let Some(o) = self.as_object() {
            o.retrive_payload0(class_id)
        } else {
            None
        }
    }

    /// get object value's payload borrow ref.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub fn retrive_object_payload(
        &self,
        ctx: &ContextImpl,
        class_id: ClassID,
    ) -> Result<&ObjectPayload, i32> {
        if let Some(o) = self.as_object() {
            o.retrive_payload(ctx, class_id)
        } else {
            ctx.throw_type_error_invalid_class(class_id);
            Err(-1)
        }
    }

    /// get object value's payload borrow ref.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub fn retrive_object_payload_mut(
        &mut self,
        ctx: &ContextImpl,
        class_id: ClassID,
    ) -> Result<&mut ObjectPayload, i32> {
        if let Some(o) = self.as_object_mut() {
            o.retrive_payload_mut(ctx, class_id)
        } else {
            ctx.throw_type_error_invalid_class(class_id);
            Err(-1)
        }
    }
}
