use std::ffi::c_void;

use crate::{
    USER_CLASS_ID_START,
    atom::Atom,
    class::{ClassID, OtherPayload},
    context::{ContextImpl, ContextPtr},
    object::{Object, ObjectImpl, ObjectPayload, Shape},
    value::JSValue,
};

impl ContextImpl {
    /// create object using `shape` and `payload`.
    /// when failed, `payload` is returned back in Err, caller should free it.
    #[inline(always)]
    pub(crate) fn create_object_with_shape(
        &self,
        shape: Shape,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        ObjectImpl::create(self, Some(shape), payload)
    }

    /// create an object with `payload` data and optional prototype `proto`:
    ///
    /// - New object will apply the shape determined by `proto`, if provided.
    /// - The `payload` determines what class the new object is.
    pub fn create_object_with_proto(
        &self,
        proto: Option<Object>,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        // find shape by prototype
        let shape = {
            match self.rt_ptr().find_shape_by_proto(proto) {
                Some(sh) => sh,
                None => {
                    if let Ok(sh) = Shape::new(self, proto, 0) {
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
    /// the `ctor` could be:
    /// - undefined: use the `payload` kind associated class prototype,
    /// - otherwise retrieve property `ctor.prototype` for prototype.
    pub fn create_object_from_ctor(
        &self,
        ctor: JSValue,
        payload: ObjectPayload,
    ) -> Result<Object, ObjectPayload> {
        let class_id = payload.class_id();

        let proto: Option<Object> = if ctor.is_undefined() {
            self.class_proto(class_id)
        } else if let Ok(p) = ctor.get_property(self, Atom::CONST_prototype) {
            if let Some(o) = p.as_object().copied() {
                Some(o)
            } else if let Ok(realm) = {
                let cx = unsafe { ContextPtr::from_ref_unchecked(self) };
                ctor.as_object().map_or(Ok(cx), |o| o.realm(cx))
            } {
                realm.class_proto(class_id)
            } else {
                return Err(payload);
            }
        } else {
            return Err(payload);
        };

        self.create_object_with_proto(proto, payload)
    }

    /// Create object with `payload` data, applying its implied class.
    /// That means the new object's prototype and shape are determined by `payload` automatically.
    #[inline(always)]
    pub fn create_object(&self, payload: ObjectPayload) -> Result<Object, ObjectPayload> {
        self.create_object_with_proto(self.class_proto(payload.class_id()), payload)
    }

    /// Create object of user defined native class
    #[inline]
    pub fn create_object_user_class(
        &self,
        class_id: ClassID,
        opaque: *mut c_void,
    ) -> Result<Object, ObjectPayload> {
        if class_id < USER_CLASS_ID_START {
            self.throw_internal_error("expect user defined native class id");
            Err(ObjectPayload::OTHER(OtherPayload::new(class_id, opaque)))
        } else {
            let payload = ObjectPayload::OTHER(OtherPayload::new(class_id, opaque));
            self.create_object(payload)
        }
    }

    /// create basic object with given prototype `proto`.
    #[inline(always)]
    pub fn create_base_object_with_proto(&self, proto: Option<Object>) -> Result<Object, i32> {
        self.create_object_with_proto(proto, ObjectPayload::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new basic object using default Object prototype
    #[inline(always)]
    pub fn create_base_object(&self) -> Result<Object, i32> {
        self.create_object(ObjectPayload::OBJECT).map_err(|_| -1)
    }
}
