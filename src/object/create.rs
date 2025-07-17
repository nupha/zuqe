use std::rc::Rc;

use crate::atom::consts::JS_ATOM_prototype;
use crate::function::JS_GetFunctionRealm;
use crate::{
    context::JSContext,
    object::{JSObject, JSShape, ObjectDetail, ObjectInner},
    value::JSValue,
};

impl JSContext {
    /// create object using `shape` and `payload`.
    /// when failed, `payload` is returned back in Err, caller should free it.
    pub(crate) fn create_object_with_shape(
        &self,
        shape: JSShape,
        payload: ObjectDetail,
    ) -> Result<JSObject, ObjectDetail> {
        let obj = JSObject(Rc::new(ObjectInner::new(self, Some(shape), payload)?));
        self.rt_mut()
            .gc_add_object(obj.id(), obj.downgrade().into());
        Ok(obj)
    }

    /// create an object with `payload` data.
    /// when failed, detail data is returned back in Err, caller should free it.
    /// `proto` is Prototype object (can be JSValue::NULL).
    ///
    /// Note:
    /// - New object will use the shape determined by `proto`.
    /// - The `payload` determines what class the new object is.
    pub fn create_object_from_proto(
        &self,
        proto: JSValue,
        payload: ObjectDetail,
    ) -> Result<JSObject, ObjectDetail> {
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
                    if let Ok(sh) = JSShape::new(self, proto_ob) {
                        sh
                    } else {
                        return Err(payload);
                    }
                }
            }
        };
        self.create_object_with_shape(shape, payload)
    }

    /// create object applying class constructor `ctor` call, with `payload` data.
    /// new object's class is determined by `payload` kind.
    ///
    /// if `ctor` is:
    /// - undefined, use the `payload` kind associated class prototype,
    /// - otherwise, retrieve property `ctor.prototype` as prototype.
    pub fn create_object_apply_ctor(
        &self,
        ctor: &JSValue,
        payload: ObjectDetail,
    ) -> Result<JSObject, ObjectDetail> {
        let class_id = payload.class_id();

        let proto = if ctor.is_undefined() {
            self.0.borrow().class_proto[class_id as usize].clone()
        } else {
            if let Ok(p) = ctor.get_property(self, JS_ATOM_prototype).check_exception() {
                if !p.is_object() {
                    if let Ok(realm) = JS_GetFunctionRealm(self, &ctor) {
                        realm.0.borrow().class_proto[class_id as usize].clone()
                    } else {
                        return Err(payload);
                    }
                } else {
                    p
                }
            } else {
                return Err(payload);
            }
        };

        self.create_object_from_proto(proto, payload)
    }

    /// create new object with `payload` data.
    /// New object's prototype and shape are determined by `payload` automatically.
    #[inline(always)]
    pub fn create_object(&self, payload: ObjectDetail) -> Result<JSObject, ObjectDetail> {
        self.create_object_from_proto(
            self.0.borrow().class_proto[payload.class_id() as usize].clone(),
            payload,
        )
    }

    /// create new basic object with specified prototype `proto`.
    #[inline(always)]
    pub fn create_base_object_from_proto(&self, proto: JSValue) -> Result<JSObject, i32> {
        self.create_object_from_proto(proto, ObjectDetail::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new basic object using default Object prototype
    #[inline(always)]
    pub fn create_base_object(&self) -> Result<JSObject, i32> {
        self.create_object(ObjectDetail::OBJECT).map_err(|_| -1)
    }

    /// create new instance with detailed object data.
    /// when failed, detail data is returned back in Err, caller should free it.
    /// `proto` is Prototype object (can be JSValue::NULL).
    ///
    /// Note:
    /// - New instance will apply the shape determined by `proto`.
    /// - The `detail` data determines what class the new instance is.
    #[deprecated(note = "use JSContext::create_object_from_proto() instead")]
    pub fn create_instance_from_proto(
        &self,
        proto: JSValue,
        payload: ObjectDetail,
    ) -> Result<JSValue, ObjectDetail> {
        self.create_object_from_proto(proto, payload)
            .map(JSValue::Object)
    }
}
