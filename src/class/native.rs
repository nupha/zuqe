// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ffi::c_void, sync::Arc};

use crate::{ClassID, JSValue, Object};

/// Native object payload data
pub trait NativePayload {
    type Target: ?Sized;

    fn into_raw(self) -> *mut c_void;
    unsafe fn from_raw(ptr: *mut c_void) -> Self;
    unsafe fn raw_as_ref<'a>(ptr: *mut c_void) -> &'a Self::Target;
}

impl<P> NativePayload for Box<P> {
    type Target = P;

    #[inline(always)]
    fn into_raw(self) -> *mut c_void {
        Box::into_raw(self).cast()
    }

    #[inline(always)]
    unsafe fn from_raw(ptr: *mut c_void) -> Self {
        unsafe { Box::from_raw(ptr.cast::<P>()) }
    }

    #[inline(always)]
    unsafe fn raw_as_ref<'a>(ptr: *mut c_void) -> &'a Self::Target {
        debug_assert!(!ptr.is_null());
        unsafe { &*ptr.cast::<P>() }
    }
}

impl<P> NativePayload for Arc<P> {
    type Target = Arc<P>;

    #[inline(always)]
    fn into_raw(self) -> *mut c_void {
        // leak Box<Arc<P>>
        Box::into_raw(Box::new(self)).cast::<c_void>() as _
    }

    #[inline(always)]
    unsafe fn from_raw(ptr: *mut c_void) -> Self {
        // restore Box<Arc<P>> then deref to Arc<P>
        unsafe { *Box::from_raw(ptr.cast::<Arc<P>>()) }
    }

    #[inline(always)]
    unsafe fn raw_as_ref<'a>(ptr: *mut c_void) -> &'a Self::Target {
        debug_assert!(!ptr.is_null());
        unsafe { &*ptr.cast() } // &Arc<P>
    }
}

/// the dummy payload
impl NativePayload for () {
    type Target = ();

    #[inline(always)]
    fn into_raw(self) -> *mut c_void {
        std::ptr::null_mut()
    }

    #[inline(always)]
    unsafe fn from_raw(_: *mut c_void) -> Self {}

    #[inline(always)]
    unsafe fn raw_as_ref<'a>(_: *mut c_void) -> &'a Self::Target {
        &()
    }
}

pub trait NativeObject: Clone + TryFrom<JSValue> + TryFrom<Object> {
    /// Get registered class id.
    /// Should return 0 if class has not been registered.
    fn class_id() -> ClassID;
}

impl Object {
    pub fn as_native_object<O: NativeObject>(&self) -> Option<&O> {
        if self.class_id() == O::class_id() {
            Some(unsafe { std::mem::transmute::<&Self, &O>(self) })
        } else {
            None
        }
    }

    pub fn as_native_object_mut<O: NativeObject>(&mut self) -> Option<&mut O> {
        if self.class_id() == O::class_id() {
            Some(unsafe { std::mem::transmute::<&mut Self, &mut O>(self) })
        } else {
            None
        }
    }
}

impl JSValue {
    pub fn as_native_object<O: NativeObject>(&self) -> Option<&O> {
        self.as_object().and_then(|o| o.as_native_object::<O>())
    }

    pub fn as_native_object_mut<O: NativeObject>(&mut self) -> Option<&mut O> {
        self.as_object_mut()
            .and_then(|o| o.as_native_object_mut::<O>())
    }
}

/// Internal helper: conditionally add on_dispose handler based on payload type.
/// `()` payload uses null opaque, so no disposal is needed.
/// Real types need on_dispose to free the heap-allocated opaque.
#[macro_export]
macro_rules! maybe_on_dispose {
    ((), $cd:ident) => {};

    ($payload_ty:ty, $cd:ident) => {
        $cd = $cd.on_dispose(|_, opaque: *mut std::ffi::c_void, _| {
            if !opaque.is_null() {
                unsafe {
                    let _ = <$payload_ty as $crate::NativePayload>::from_raw(opaque);
                }
            }
        });
    };
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
            pub fn as_object(&self) -> &$crate::Object {
                &self.0
            }

            #[inline(always)]
            pub fn as_object_mut(&mut self) -> &mut $crate::Object {
                &mut self.0
            }

            /// register this native class to runtime, returns class id.
            pub fn register(rt: &mut $crate::RuntimeImpl) -> Result<$crate::ClassID, i32> {
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), 0, "class has been registered");

                // call on_register handler
                $($on_register(rt);)?

                let mut cd = $crate::ClassDef::new($name, $proto, $ctor);

                // optionally add on_dispose if payload is not dummy
                $crate::maybe_on_dispose!($payload_ty, cd);
                // set class unregister handler
                $(cd = cd.on_unregister($on_unregister);)?
                // set payload gc handler
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

        impl std::ops::Deref for $native_ty {
            type Target = $crate::Object;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::ops::DerefMut for $native_ty {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl TryFrom<$crate::Object> for $native_ty {
            type Error = i32;

            fn try_from(o: $crate::Object) -> Result<Self, Self::Error> {
                if o.class_id() == <Self as $crate::NativeObject>::class_id() {
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

        impl From<$native_ty> for $crate::Object {
            fn from(v: $native_ty) -> Self {
                v.0.into()
            }
        }

        impl From<$native_ty> for $crate::JSValue {
            fn from(v: $native_ty) -> Self {
                v.0.into()
            }
        }
    };
}

#[macro_export]
macro_rules! def_native_object_base {
    // Without payload
    ($name:expr, $native_ty:ident, (), $proto:expr, $ctor:expr
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
            ) -> Result<Self, i32> {
                ctx.create_class_instance_with_opaque(new_target, <Self as $crate::NativeObject>::class_id(), std::ptr::null_mut())
                    .map(Self)
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
            pub fn payload(&self) -> Option<&<$payload_ty as $crate::NativePayload>::Target> {
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());

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

            pub fn take_payload(&mut self) -> Option<$payload_ty>{
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());

                if let Some(p) = self.as_object_mut().payload_mut().other_mut() {
                    unsafe {
                        let ptr = p.replace_opaque(std::ptr::null_mut());
                        if !ptr.is_null() {
                            Some(<$payload_ty as $crate::NativePayload>::from_raw(ptr))
                        } else {
                            None
                        }
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
            ) -> Result<Self, i32> {
                let opaque = <$payload_ty as $crate::NativePayload>::into_raw(payload);
                debug_assert!(!opaque.is_null());

                ctx.create_class_instance_with_opaque(new_target, <Self as $crate::NativeObject>::class_id(), opaque)
                    .map(Self)
                    .map_err(|_| {
                        unsafe {
                            let _ = <$payload_ty as $crate::NativePayload>::from_raw(opaque);
                        }
                        -1
                    })
            }
        }
    };
}

// Helper: generate NativeObject impl (shared by both def_native_object! arms)
#[macro_export]
macro_rules! impl_native_object {
    ($native_ty:ident) => {
        impl $crate::NativeObject for $native_ty {
            fn class_id() -> $crate::ClassID {
                Self::CLASS_ID.with(|x| x.get())
            }
        }
    };
}

/// Define a native object class
#[macro_export]
macro_rules! def_native_object {
    // Dummy payload, no constructor
    ($name:expr, $native_ty:ident, (), $proto:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, (), $proto, None
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);

        $crate::impl_native_object!($native_ty);
    };

    // Dummy payload, with constructor
    ($name:expr, $native_ty:ident, (), $proto:expr
        , constructor: $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, (), $proto, Some($ctor)
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);

        $crate::impl_native_object!($native_ty);
    };

    // With payload, no constructor
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, $payload_ty, $proto, None
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            $(, on_payload_gc: $on_payload_gc)?);

        $crate::impl_native_object!($native_ty);
    };

    // With payload, with constructor
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto:expr
        , constructor: $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object_base!($name, $native_ty, $payload_ty, $proto, Some($ctor)
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            $(, on_payload_gc: $on_payload_gc)?);

        $crate::impl_native_object!($native_ty);
    };
}
