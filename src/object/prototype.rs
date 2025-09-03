use crate::{
    context::JSContext,
    error::JSResult,
    object::JSObject,
    value::{JS_GetPrototypePrimitive, JSValue},
};

pub struct ObjectPrototypeIter {
    ctx: JSContext,
    cur_obj: Option<Result<JSObject, i32>>,
}

impl Iterator for ObjectPrototypeIter {
    type Item = Result<JSObject, i32>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(p) = self.cur_obj.take() {
            if let Ok(p) = &p {
                match p.prototype(&self.ctx) {
                    Ok(Some(up)) => {
                        self.cur_obj.replace(Ok(up));
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.cur_obj.replace(Err(-1));
                    }
                }
            }
            Some(p)
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
    pub fn prototype_iter(&self, ctx: &JSContext, include_self: bool) -> ObjectPrototypeIter {
        ObjectPrototypeIter {
            ctx: ctx.clone(),
            cur_obj: if include_self {
                Some(Ok(self.clone()))
            } else {
                match self.prototype(ctx) {
                    Ok(Some(p)) => Some(Ok(p)),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                }
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
