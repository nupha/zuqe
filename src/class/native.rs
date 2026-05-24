use std::ffi::c_void;

use crate::{ClassID, JSValue, Object};

/// Native object payload data
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

pub trait NativeObject<T: NativePayload>:
    Clone + TryFrom<JSValue> + TryFrom<Object>
{
    /// Get registered class id.
    /// Should return 0 if class has not been registered.
    fn class_id() -> ClassID;
}

impl Object {
    pub fn as_native_object<P: NativePayload, NOBJ: NativeObject<P>>(
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
    pub fn as_native_object<P: NativePayload, NOBJ: NativeObject<P>>(
        &self,
    ) -> Option<&NOBJ> {
        self.as_object()
            .and_then(|o| o.as_native_object::<P, NOBJ>())
    }
}

#[macro_export]
macro_rules! def_native_object_struct {
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        /// Native object type
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $native_ty($crate::Object);

        impl $native_ty {
            thread_local! {
                static CLASS_ID: std::cell::Cell<$crate::ClassID> = const { Cell::new(0) };
            }

            #[inline(always)]
            pub fn as_object(&self) -> &Object{
                &self.0
            }

            /// register this native class to runtime, returns class id.
            pub fn register_native_class(rt: &mut $crate::RuntimeImpl) -> Result<$crate::ClassID, i32> {
                debug_assert_eq!(Self::class_id(), 0, "class has been registered");

                // call on_register handler
                $($on_register(rt);)?

                let mut cd = $crate::ClassDef::new($name, $proto, $ctor).on_dispose(|_, opaque: *mut std::ffi::c_void, _| {
                    if !opaque.is_null() {
                        unsafe {
                            let _ = <$payload_ty as $crate::NativePayload>::from_raw(opaque);
                        }
                    }
                });

                // on unregister handler
                $(cd = cd.on_unregister($on_unregister);)?

                // payload gc trace handler
                $(
                    cd = cd.on_gc_trace(|opaque, gcx| {
                        debug_assert!(!opaque.is_null());
                        let p = unsafe { <$payload_ty as $crate::NativePayload>::raw_as_ref(opaque) };
                        $on_payload_gc(p, gcx);
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
                if o.class_id() == <Self as NativeObject<$payload_ty>>::class_id() {
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
    // Without payload
    ($name:expr, $native_ty:ident, $proto:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object_struct!($name, $native_ty, (), $proto, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);

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

    // With payload
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object_struct!($name, $native_ty, $payload_ty, $proto, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            $(, on_payload_gc: $on_payload_gc)?);

        impl $native_ty {
            /// get payload data
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

// Helper: generate NativeObject impl (shared by both def_native_object! arms)
#[macro_export]
macro_rules! impl_native_object {
    ($native_ty:ident, $payload_ty:ty) => {
        impl $crate::NativeObject<$payload_ty> for $native_ty {
            fn class_id() -> ClassID {
                Self::CLASS_ID.with(|x| x.get())
            }
        }
    };
}

/// Define a native object class
#[macro_export]
macro_rules! def_native_object {
    // Use dummy payload
    ($name:expr, $native_ty:ident, (), $proto:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, $proto, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);

        $crate::impl_native_object!($native_ty, ());
    };

    // With payload
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, $payload_ty, $proto, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            $(, on_payload_gc: $on_payload_gc)?);

        $crate::impl_native_object!($native_ty, $payload_ty);
    };
}
