use crate::{context::JSContext, object::JSObject, value::JSValue};

include!(concat!(env!("OUT_DIR"), "/class_payload.rs"));

impl<'a> JSObject<'a> {
    /// get object's payload.
    /// if the object is not the required class_id (optional),
    /// returns None
    pub(crate) fn retrive_payload0(&self, class_id: ClassID) -> Option<&ObjectPayload<'a>> {
        if class_id == self.class_id() {
            Some(self.payload())
        } else {
            None
        }
    }

    /// get object's payload borrow ref.
    /// if the object is not the required class_id (optional),
    /// throws type error.
    pub(crate) fn retrive_payload(
        &self,
        ctx: &JSContext,
        class_id: ClassID,
    ) -> Result<&ObjectPayload<'a>, i32> {
        self.retrive_payload0(class_id).ok_or_else(|| {
            ctx.throw_type_error_invalid_class(class_id);
            -1
        })
    }
}

impl<'a> JSValue<'a> {
    /// get object value's payload.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub(crate) fn retrive_object_payload0(&self, class_id: ClassID) -> Option<&ObjectPayload<'a>> {
        if let Some(o) = self.as_object() {
            o.retrive_payload0(class_id)
        } else {
            None
        }
    }

    /// get object value's payload borrow ref.
    /// if the value is not an object, or optionally is not the required class_id,
    /// throws type error.
    pub(crate) fn retrive_object_payload(
        &self,
        ctx: &JSContext<'a>,
        class_id: ClassID,
    ) -> Result<&ObjectPayload<'a>, i32> {
        if let Some(o) = self.as_object() {
            o.retrive_payload(ctx, class_id)
        } else {
            ctx.throw_type_error_invalid_class(class_id);
            Err(-1)
        }
    }
}
