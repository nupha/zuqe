use std::{ops::Deref, rc::Rc};

use crate::{
    atom::consts::JS_ATOM_prototype,
    context::JSContext,
    function::JS_GetFunctionRealm,
    object::{JSObject, JSShape, ObjectInner, ObjectPayload},
    value::JSValue,
};

impl JSContext {
    /// create object using `shape` and `payload`.
    /// when failed, `payload` is returned back in Err, caller should free it.
    pub(crate) fn create_object_with_shape(
        &self,
        shape: JSShape,
        payload: ObjectPayload,
    ) -> Result<JSObject, ObjectPayload> {
        let obj = JSObject(Rc::new(ObjectInner::new(self, Some(shape), payload)?));
        self.rt_mut()
            .gc_add_object(obj.id(), obj.downgrade().into());
        Ok(obj)
    }

    /// create object with `payload` data and specified `proto` as prototype.
    /// when failed, detail data is returned back in Err, caller should free it.
    /// `proto` is Prototype object (can be JSValue::NULL).
    ///
    /// Note:
    /// - New object will use the shape determined by `proto`.
    /// - The `payload` determines what class the new object is.
    pub fn create_object_from_proto(
        &self,
        proto: JSValue,
        payload: ObjectPayload,
    ) -> Result<JSObject, ObjectPayload> {
        // find shape by prototype
        let shape = {
            let proto_ob = if let JSValue::Object(o) = proto {
                Some(o)
            } else {
                None
            };
            match self.rt().find_shape_by_proto(proto_ob.as_ref()) {
                Some(sh) => sh,
                None => {
                    if let Ok(sh) = JSShape::new(self, proto_ob, 0) {
                        sh
                    } else {
                        return Err(payload);
                    }
                }
            }
        };
        self.create_object_with_shape(shape, payload)
    }

    /// create object applying constructor `ctor`, with `payload` data.
    /// new object's class is determined by `payload` kind.
    ///
    /// if `ctor` is:
    /// - undefined, use the `payload` kind associated class prototype,
    /// - otherwise, retrieve property `ctor.prototype` as prototype.
    pub fn create_object_from_ctor(
        &self,
        ctor: &JSValue,
        payload: ObjectPayload,
    ) -> Result<JSObject, ObjectPayload> {
        let class_id = payload.class_id();

        let proto = if ctor.is_undefined() {
            self.0.borrow().class_proto[class_id as usize].clone()
        } else if let Ok(p) = ctor.get_property(self, &JS_ATOM_prototype) {
            if !p.is_object() {
                if let Ok(realm) = JS_GetFunctionRealm(self, ctor) {
                    realm.0.borrow().class_proto[class_id as usize].clone()
                } else {
                    return Err(payload);
                }
            } else {
                p
            }
        } else {
            return Err(payload);
        };

        self.create_object_from_proto(proto, payload)
    }

    /// create object applying constructor `ctor`, with payload data returned by factory function.
    /// new object's class is determined by `payload` kind.
    ///
    /// if `ctor` is:
    /// - undefined, use the `payload` kind associated class prototype,
    /// - otherwise, retrieve property `ctor.prototype` as prototype.
    pub fn create_object_from_ctor_with(
        &self,
        ctor: &JSValue,
        payload_fn: impl FnOnce() -> Result<ObjectPayload, i32>,
    ) -> Result<JSObject, i32> {
        let (proto, isa) = if ctor.is_undefined() {
            let payload = payload_fn()?;
            (
                self.0.borrow().class_proto[payload.class_id() as usize].clone(),
                payload,
            )
        } else {
            // don't change following2 lines order
            let p = ctor.get_property(self, JS_ATOM_prototype.deref())?; // <- may exec user script here
            let payload = payload_fn()?;

            if !p.is_object() {
                let realm = JS_GetFunctionRealm(self, ctor)?;
                (
                    realm.0.borrow().class_proto[payload.class_id() as usize].clone(),
                    payload,
                )
            } else {
                (p, payload)
            }
        };

        self.create_object_from_proto(proto, isa).map_err(|_| -1)
    }

    /// create object with `payload` data.
    /// New object's prototype and shape are determined by `payload` automatically.
    #[inline]
    pub fn create_object(&self, payload: ObjectPayload) -> Result<JSObject, ObjectPayload> {
        self.create_object_from_proto(
            self.0.borrow().class_proto[payload.class_id() as usize].clone(),
            payload,
        )
    }

    /// create basic object with specified prototype `proto`.
    #[inline(always)]
    pub fn create_base_object_from_proto(&self, proto: JSValue) -> Result<JSObject, i32> {
        self.create_object_from_proto(proto, ObjectPayload::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new basic object using default Object prototype
    #[inline(always)]
    pub fn create_base_object(&self) -> Result<JSObject, i32> {
        self.create_object(ObjectPayload::OBJECT).map_err(|_| -1)
    }
}
