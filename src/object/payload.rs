use crate::{context::JSContext, object::JSObject, value::JSValue};

include!(concat!(env!("OUT_DIR"), "/class_payload.rs"));

impl JSObject {
    /// get object's payload.
    /// if the object is not the required class_id (optional),
    /// returns None
    pub(crate) fn retrive_payload0(&self, class_id: ClassID) -> Option<&ObjectPayload> {
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
    ) -> Result<&ObjectPayload, i32> {
        self.retrive_payload0(class_id).ok_or_else(|| {
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
    pub(crate) fn retrive_object_payload(
        &self,
        ctx: &JSContext,
        class_id: ClassID,
    ) -> Result<&ObjectPayload, i32> {
        if let Some(o) = self.as_object() {
            o.retrive_payload(ctx, class_id)
        } else {
            ctx.throw_type_error_invalid_class(class_id);
            Err(-1)
        }
    }
}

// impl ObjectPayload {
//     pub(crate) fn replace_value(&mut self, val: JSValue) -> Result<JSValue, i32> {
//         match (self, &val) {
//             (Self::NUMBER(a), JSValue::Int(_) | JSValue::Float64(_)) => {
//                 Ok(std::mem::replace(a, val))
//             }
//             (Self::BOOLEAN(a), JSValue::Bool(_)) => Ok(std::mem::replace(a, val)),
//             (Self::STRING(a), JSValue::String(_)) => Ok(std::mem::replace(a, val).into()),
//             (Self::SYMBOL(a), JSValue::Symbol(_)) => Ok(std::mem::replace(a, val).into()),
//             _ => Err(-1),
//         }
//     }
// }
