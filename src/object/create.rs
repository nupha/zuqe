use crate::{
    atom::Atom,
    context::{ContextImpl, ContextRef},
    function::JS_GetFunctionRealm,
    object::{ObjectImpl, ObjectPayload, ObjectRef, ShapeHandle},
    value::JSValue,
};

impl ContextImpl {
    /// create object using `shape` and `payload`.
    /// when failed, `payload` is returned back in Err, caller should free it.
    pub(crate) fn create_object_with_shape(
        &self,
        shape: ShapeHandle,
        payload: ObjectPayload,
    ) -> Result<ObjectRef, ObjectPayload> {
        ObjectImpl::create(self, Some(shape), payload)
    }

    /// create an object with `payload` data and optional prototype `proto`:
    ///
    /// - New object will apply the shape determined by `proto`, if provided.
    /// - The `payload` determines what class the new object is.
    pub fn create_object_with_proto(
        &self,
        proto: Option<ObjectRef>,
        payload: ObjectPayload,
    ) -> Result<ObjectRef, ObjectPayload> {
        // find shape by prototype
        let shape = {
            match self.rt().shape_registry.find_shape_by_proto(proto) {
                Some(sh) => sh,
                None => {
                    if let Ok(sh) = ShapeHandle::new(self, proto, 0) {
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
    ) -> Result<ObjectRef, ObjectPayload> {
        let class_id = payload.class_id();

        let proto: Option<ObjectRef> = if ctor.is_undefined() {
            self.class_proto(class_id)
        } else if let Ok(p) = ctor.get_property(self, Atom::CONST_prototype) {
            if let Some(o) = p.as_object().copied() {
                Some(o)
            } else if let Ok(realm) =
                JS_GetFunctionRealm(unsafe { ContextRef::from_ref_unchecked(self) }, ctor)
            {
                realm.class_proto(class_id)
            } else {
                return Err(payload);
            }
        } else {
            return Err(payload);
        };

        self.create_object_with_proto(proto, payload)
    }

    // /// create object applying constructor `ctor`, with payload data returned by factory function.
    // /// new object's class is determined by `payload` kind.
    // ///
    // /// if `ctor` is:
    // /// - undefined, use the `payload` kind associated class prototype,
    // /// - otherwise, retrieve property `ctor.prototype` as prototype.
    // pub(crate) fn create_object_from_ctor_with(
    //     &self,
    //     ctor: JSValue,
    //     payload_fn: impl FnOnce() -> Result<ObjectPayload, i32>,
    // ) -> Result<ObjectRef, i32> {
    //     let (proto, isa) = if ctor.is_undefined() {
    //         let payload = payload_fn()?;
    //         (self.class_proto(payload.class_id()), payload)
    //     } else {
    //         // don't change following 2 lines order
    //         let p = ctor.get_property(self, Atom::CONST_prototype)?; // <- may exec user script here
    //         let payload = payload_fn()?;

    //         if let Some(o) = p.as_object() {
    //             (Some(*o), payload)
    //         } else {
    //             let realm = JS_GetFunctionRealm(ContextRef::try_from_ref(self).unwrap(), ctor)?;
    //             (realm.class_proto(payload.class_id()), payload)
    //         }
    //     };

    //     self.create_object_with_proto(proto, isa).map_err(|_| -1)
    // }

    /// create object with `payload` data.
    /// New object's prototype and shape are determined by `payload` automatically.
    #[inline]
    pub fn create_object(&self, payload: ObjectPayload) -> Result<ObjectRef, ObjectPayload> {
        self.create_object_with_proto(self.class_proto(payload.class_id()), payload)
    }

    /// create basic object with given prototype `proto`.
    #[inline(always)]
    pub fn create_base_object_with_proto(
        &self,
        proto: Option<ObjectRef>,
    ) -> Result<ObjectRef, i32> {
        self.create_object_with_proto(proto, ObjectPayload::OBJECT)
            .map_err(|_| -1)
    }

    /// Create new basic object using default Object prototype
    #[inline(always)]
    pub fn create_base_object(&self) -> Result<ObjectRef, i32> {
        self.create_object(ObjectPayload::OBJECT).map_err(|_| -1)
    }
}
