use std::ffi::c_void;

use crate::{ClassID, JSValue, Object, RuntimeImpl};

/// Native payload for class instance
pub trait NativePayload {
    fn into_raw(self) -> *mut c_void;
    unsafe fn from_raw(ptr: *mut c_void) -> Self;
    unsafe fn raw_as_ref<'a>(ptr: *mut c_void) -> &'a Self;
}

/// the dummy payload
impl NativePayload for () {
    #[inline(always)]
    fn into_raw(self) -> *mut c_void {
        std::ptr::null_mut()
    }
    #[inline(always)]
    unsafe fn from_raw(_: *mut c_void) -> Self {}
    #[inline(always)]
    unsafe fn raw_as_ref<'a>(_: *mut c_void) -> &'a Self {
        &()
    }
}

pub trait NativeObjectWithPayload<T: NativePayload>:
    Clone + TryFrom<JSValue> + TryFrom<Object>
{
    /// get registered class id. Should return 0 if class has not been registered.
    fn class_id() -> ClassID;

    /// called before class will be registered to runtime
    #[inline(always)]
    fn on_register(rt: &mut RuntimeImpl) {}
    /// called after class has been unregistered from runtime
    #[inline(always)]
    fn on_unregister() {}

    fn as_object(&self) -> &Object;
}

impl Object {
    pub fn as_native_object<P: NativePayload, NOBJ: NativeObjectWithPayload<P>>(
        &self,
    ) -> Option<&NOBJ> {
        if self.class_id() == NOBJ::class_id() {
            Some(unsafe { std::mem::transmute::<&Object, &NOBJ>(self) })
        } else {
            None
        }
    }
}

impl JSValue {
    pub fn as_native_object<P: NativePayload, NOBJ: NativeObjectWithPayload<P>>(
        &self,
    ) -> Option<&NOBJ> {
        self.as_object()
            .and_then(|o| o.as_native_object::<P, NOBJ>())
    }
}

// Helper: generate struct definition, CLASS_ID, and TryFrom impls
#[macro_export]
macro_rules! def_native_object_struct {
    ($name:expr, $native_ty:ident, $proto:expr, $ctor:expr, $payload_ty:ty $(, $payload_gc:expr)?) => {
        /// the native object type
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $native_ty($crate::Object);

        impl $native_ty {
            thread_local! {
                static CLASS_ID: std::cell::Cell<$crate::ClassID> = const { Cell::new(0) };
            }

            /// register this native class to runtime, returns class id.
            pub fn register_native_class(rt: &mut $crate::RuntimeImpl) -> Result<$crate::ClassID, i32> {
                debug_assert_eq!(Self::class_id(), 0, "class has been registered");

                Self::on_register(rt);

                let mut cd = $crate::ClassDef::new($name, $proto, $ctor)
                    .on_unregister(Self::on_unregister)
                    .on_dispose(|_, opaque: *mut std::ffi::c_void, _| {
                        if !opaque.is_null() {
                            unsafe {
                                let _ = <$payload_ty as $crate::NativePayload>::from_raw(opaque);
                            }
                        }
                    });

                // gc trace for payload
                $(
                    cd = cd.on_gc_trace(|opaque, gcx| {
                        debug_assert!(!opaque.is_null());
                        let p = unsafe { <$payload_ty as $crate::NativePayload>::raw_as_ref(opaque) };
                        $payload_gc(p, gcx);
                    });
                )?

                let class_id = rt.register_class_def(cd)?;
                Self::CLASS_ID.with(|x| x.set(class_id));

                Ok(class_id)
            }
        }

        impl TryFrom<$crate::Object> for $native_ty {
            type Error = i32;

            fn try_from(o: Object) -> Result<Self, Self::Error> {
                if o.class_id() == <Self as NativeObjectWithPayload<$payload_ty>>::class_id() {
                    Ok(Self(o))
                } else {
                    Err(-1)
                }
            }
        }

        impl TryFrom<$crate::JSValue> for $native_ty {
            type Error = i32;

            fn try_from(v: JSValue) -> Result<Self, Self::Error> {
                if let JSValue::Object(o) = v {
                    TryFrom::<$crate::Object>::try_from(o)
                } else {
                    Err(-1)
                }
            }
        }
    };
}

#[macro_export]
macro_rules! def_native_object_base {
    // Without payload -> use DummyPayload
    ($name:expr, $native_ty:ident, $proto:expr, $ctor:expr) => {
        $crate::def_native_object_struct!($name, $native_ty, $proto, $ctor, ());

        impl $native_ty {
            /// create an instance of the class
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
            ) -> Result<Object, i32> {
                ctx.create_class_instance_with_opaque(new_target, Self::class_id(), std::ptr::null_mut())
                    .map_err(|_| {
                        -1
                    })
            }
        }
    };

    // With payload, optionally with gc trace
    ($name:expr, $native_ty:ident, $proto:expr, $ctor:expr, payload: $payload_ty:ty $(, payload_gc_trace: $payload_gc:expr)?) => {
        $crate::def_native_object_struct!($name, $native_ty, $proto, $ctor, $payload_ty $(, $payload_gc)?);

        impl $native_ty {
            pub fn payload(&self) -> Option<&$payload_ty> {
                debug_assert_eq!(Self::class_id(), self.as_object().class_id());

                if let Some(p) = self.as_object().payload().other() {
                    let ptr = p.opaque();
                    if !ptr.is_null() {
                        Some(unsafe { <$payload_ty as $crate::NativePayload>::raw_as_ref(ptr) })
                    } else {
                        None
                    }
                } else {
                    unreachable!()
                }
            }

            /// create an instance of the class with attached payload
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
                payload: $payload_ty,
            ) -> Result<Object, i32> {
                let opaque = <$payload_ty as $crate::NativePayload>::into_raw(payload);
                debug_assert!(!opaque.is_null());

                ctx.create_class_instance_with_opaque(new_target, Self::class_id(), opaque)
                    .map_err(|_| {
                        unsafe {
                            let _ = <$payload_ty as $crate::NativePayload>::from_raw(opaque);
                        }
                        -1
                    })
            }
        }

        impl $crate::NativePayload for $payload_ty {
            fn into_raw(self) -> *mut std::ffi::c_void {
                Box::into_raw(Box::new(self)).cast()
            }
            unsafe fn from_raw(ptr: *mut std::ffi::c_void) -> Self {
                *unsafe { Box::from_raw(ptr.cast::<Self>()) }
            }
            unsafe fn raw_as_ref<'a>(ptr: *mut std::ffi::c_void) -> &'a Self {
                unsafe { &*ptr.cast::<Self>() }
            }
        }
    };
}

// Helper: generate NativeObjectWithPayload impl (shared by both def_native_object! arms)
#[macro_export]
macro_rules! impl_native_object_with_payload {
    ($native_ty:ident, $payload_ty:ty) => {
        impl $crate::NativeObjectWithPayload<$payload_ty> for $native_ty {
            fn class_id() -> ClassID {
                Self::CLASS_ID.with(|x| x.get())
            }

            #[inline(always)]
            fn as_object(&self) -> &Object {
                &self.0
            }
        }
    };
}

#[macro_export]
macro_rules! def_native_object {
    // Without payload -> use DummyPayload
    ($name:expr, $native_ty:ident, $proto:expr, $ctor:expr) => {
        $crate::def_native_object_base!($name, $native_ty, $proto, $ctor);
        $crate::impl_native_object_with_payload!($native_ty, ());
    };

    // With payload, optionally with gc trace
    ($name:expr, $native_ty:ident, $proto:expr, $ctor:expr
        , payload: $payload_ty:ty $(, payload_gc_trace: $payload_gc:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, $proto, $ctor, payload: $payload_ty $(, payload_gc_trace: $payload_gc)?);
        $crate::impl_native_object_with_payload!($native_ty, $payload_ty);
    };
}
