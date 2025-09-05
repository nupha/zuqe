use crate::{
    context::JSContext,
    error::JSResult,
    object::JSObject,
    value::{JS_GetPrototypePrimitive, JSValue},
};

/// To iterate over the prototype chain of an object
pub struct ObjectPrototypeIter {
    ctx: JSContext,
    cur_obj: Option<JSObject>,
}

impl Iterator for ObjectPrototypeIter {
    type Item = JSObject;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(obj) = self.cur_obj.take() {
            let proto = obj.shape().and_then(|s| s.0.borrow().proto.clone());
            self.cur_obj = proto;
            Some(obj)
        } else {
            None
        }
    }
}

impl JSObject {
    /// Get prototype object of `self`.
    /// Return an object if it has prototype, None for no prototype.
    /// Returns Err in case of Proxy object.
    pub fn prototype(&self, ctx: &JSContext) -> Result<Option<JSObject>, i32> {
        if let Some(p) = self.as_proxy() {
            Ok(JSObject::try_from(p.get_prototype_of(ctx)?).ok())
        } else {
            Ok(self.shape().and_then(|s| s.0.borrow().proto.clone()))
        }
    }

    /// get prototype chain walk iterator.
    /// To iterate over the prototype chain of an object.
    /// Note: only check the `proto` field of shape, no proxy considered.
    pub fn proto_iter(&self, ctx: &JSContext, include_self: bool) -> ObjectPrototypeIter {
        ObjectPrototypeIter {
            ctx: ctx.clone(),
            cur_obj: if include_self {
                Some(self.clone())
            } else {
                self.shape().and_then(|s| s.0.borrow().proto.clone())
            },
        }
    }
}

impl JSValue {
    /// Get prototype of value.
    /// Returns an object value if it has prototype, NULL for no prototype.
    /// Returns Err in case of Proxy object.
    pub fn prototype(&self, ctx: &JSContext) -> JSResult {
        if let Self::Object(o) = self {
            Ok(o.prototype(ctx)?.map_or(JSValue::NULL, JSValue::Object))
        } else {
            JS_GetPrototypePrimitive(ctx, self).err()
        }
    }
}
