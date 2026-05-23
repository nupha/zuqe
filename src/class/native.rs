use std::ffi::c_void;

use crate::{
    ClassCtorDef, ClassDef, ClassID, FieldDef, JSValue, Object, RuntimeImpl, ScopedContext,
};

/// Native payload for class instance
pub trait NativePayload {
    unsafe fn into_raw(self) -> *mut c_void;
    unsafe fn from_raw(ptr: *mut c_void) -> Self;
    unsafe fn raw_as_ref<'a>(ptr: *mut c_void) -> &'a Self;
}

pub trait NativeObject: Clone + TryFrom<JSValue> + TryFrom<Object> {
    fn class_name() -> &'static str;

    /// get class prototype entries
    fn class_proto_entries() -> &'static [FieldDef];
    /// get class constructor definition
    fn class_ctor_def() -> Option<ClassCtorDef> {
        None
    }

    /// set class id
    fn set_class_id(class_id: ClassID);
    /// get registered class id. Should return 0 if class has not been registered.
    fn class_id() -> ClassID;

    /// called before class will be registered to runtime
    fn on_register(rt: &mut RuntimeImpl) {}
    /// called after class has been unregistered from runtime
    fn on_unregister() {}
    /// called when a class instance is collected, just before Object::drop()
    fn on_dispose(rt: &mut RuntimeImpl, obj_id: usize) {}

    fn as_object(&self) -> &Object;

    fn new_instance(ctx: &mut ScopedContext, new_target: JSValue) -> Result<Object, i32> {
        ctx.create_class_instance(new_target, Self::class_id())
            .map_err(|_| -1)
    }
}

pub trait NativeObjectWithPayload<T: NativePayload>:
    Clone + TryFrom<JSValue> + TryFrom<Object>
{
    fn class_name() -> &'static str;

    /// get class prototype entries
    fn class_proto_entries() -> &'static [FieldDef];
    /// get class constructor definition
    fn class_ctor_def() -> Option<ClassCtorDef> {
        None
    }

    /// set class id
    fn set_class_id(class_id: ClassID);
    /// get registered class id. Should return 0 if class has not been registered.
    fn class_id() -> ClassID;

    /// called before class will be registered to runtime
    fn on_register(rt: &mut RuntimeImpl) {}
    /// called after class has been unregistered from runtime
    fn on_unregister() {}
    /// called when a class instance is collected, just before Object::drop()
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn on_dispose(rt: &mut RuntimeImpl, opaque: *mut c_void, obj_id: usize) {
        debug_assert!(!opaque.is_null());
        unsafe {
            let _ = T::from_raw(opaque);
        }
    }

    fn as_object(&self) -> &Object;

    fn payload(&self) -> Option<&T> {
        debug_assert_eq!(Self::class_id(), self.as_object().class_id());

        if let Some(p) = self.as_object().payload().other() {
            let ptr = p.opaque();
            if !ptr.is_null() {
                Some(unsafe { T::raw_as_ref(ptr) })
            } else {
                None
            }
        } else {
            unreachable!()
        }
    }

    /// create an instance of the class with attached payload
    fn new_instance(
        ctx: &mut ScopedContext,
        new_target: JSValue,
        payload: T,
    ) -> Result<Object, i32> {
        let opaque = unsafe { T::into_raw(payload) };
        debug_assert!(!opaque.is_null());

        ctx.create_class_instance_with_opaque(new_target, Self::class_id(), opaque)
            .map_err(|_| {
                unsafe {
                    let _ = T::from_raw(opaque);
                }
                -1
            })
    }
}

impl RuntimeImpl {
    /// register a native class to runtime
    pub fn register_native_class<C: NativeObject>(&mut self) -> Result<ClassID, i32> {
        debug_assert_eq!(C::class_id(), 0, "class has been registered");

        C::on_register(self);

        let cd = ClassDef::new(
            C::class_name(),
            C::class_proto_entries(),
            C::class_ctor_def(),
        )
        .on_dispose(|rt, _, id| C::on_dispose(rt, id))
        .on_unregister(C::on_unregister);

        let class_id = self.register_class_def(cd)?;
        C::set_class_id(class_id);

        Ok(class_id)
    }

    /// register a native class to runtime
    pub fn register_native_class_with_payload<T: NativePayload, C: NativeObjectWithPayload<T>>(
        &mut self,
    ) -> Result<ClassID, i32> {
        debug_assert_eq!(C::class_id(), 0, "class has been registered");

        C::on_register(self);

        let cd = ClassDef::new(
            C::class_name(),
            C::class_proto_entries(),
            C::class_ctor_def(),
        )
        .on_dispose(C::on_dispose)
        .on_unregister(C::on_unregister);

        let class_id = self.register_class_def(cd)?;
        C::set_class_id(class_id);

        Ok(class_id)
    }
}
