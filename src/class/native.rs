// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{ClassID, JSValue, Object};

/// Marker trait for native object types.
///
/// # Safety
/// The type must be a `#[repr(transparent)]` wrapper around `Object`.
/// Only rely on `def_native_object!` to auto-implement it. Do NOT manually impl this trait.
#[doc(hidden)]
pub unsafe trait NativeObject: Clone + TryFrom<JSValue> + TryFrom<Object> {
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
        self.as_object().and_then(Object::as_native_object::<O>)
    }

    pub fn as_native_object_mut<O: NativeObject>(&mut self) -> Option<&mut O> {
        self.as_object_mut()
            .and_then(Object::as_native_object_mut::<O>)
    }
}

/// Define a native object class
#[macro_export]
macro_rules! def_native_object {
    // ── @struct: generate struct definition and built-in impls ──
    (@struct $name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        /// Native object type
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $native_ty($crate::Object);

        impl $native_ty {
            pub const CLASS_NAME: &str = $name;

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

                let mut cd = $crate::ClassDef::new($name, $proto_entries, $ctor);

                // Set on_dispose to free heap-allocated opaque.
                // () payload uses null opaque so the null-check short-circuits;
                // real types need Box::from_raw to reclaim the allocation.
                cd = cd.on_dispose(|_, opaque: *mut std::ffi::c_void, _| {
                    if !opaque.is_null() {
                        unsafe {
                            let _ = Box::from_raw(opaque.cast::<$payload_ty>());
                        }
                    }
                });

                // set class unregister handler
                $(cd = cd.on_unregister($on_unregister);)?

                // set payload gc handler
                $(
                    cd = cd.on_gc_trace(|opaque, gcx| {
                        debug_assert!(!opaque.is_null());
                        let p = unsafe { &*opaque.cast::<$payload_ty>() };
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

            #[inline(always)]
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl std::ops::DerefMut for $native_ty {
            #[inline(always)]
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

            fn try_from(v: $crate::JSValue) -> Result<Self, Self::Error> {
                v.as_native_object::<Self>().cloned().ok_or(-1)
            }
        }

        impl From<$native_ty> for $crate::Object {
            #[inline(always)]
            fn from(v: $native_ty) -> Self {
                v.0.into()
            }
        }

        impl From<$native_ty> for $crate::JSValue {
            #[inline(always)]
            fn from(v: $native_ty) -> Self {
                v.0.into()
            }
        }
    };

    // ── @impl: generate NativeObject trait impl ──
    (@impl $native_ty:ident) => {
        unsafe impl $crate::NativeObject for $native_ty {
            fn class_id() -> $crate::ClassID {
                Self::CLASS_ID.with(|x| x.get())
            }
        }
    };

    // ── @native-no-payload: without payload ──
    (@native-no-payload $name:expr, $native_ty:ident, $proto_entries:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object!(@struct $name, $native_ty, (), $proto_entries, $ctor
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

    // ── @native-with-payload: with payload ──
    (@native-with-payload $name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object!(@struct $name, $native_ty, $payload_ty, $proto_entries, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            $(, on_payload_gc: $on_payload_gc)?);

        impl $native_ty {
            /// get payload data
            pub fn payload(&self) -> Option<&$payload_ty> {
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());

                if let Some(p) = self.as_object().payload().other() {
                    let ptr = p.opaque();
                    if !ptr.is_null() {
                        Some(unsafe { &*ptr.cast::<$payload_ty>() })
                    } else {
                        None
                    }
                } else {
                    unreachable!()
                }
            }

            /// get payload data mut
            pub fn payload_mut(&mut self) -> Option<&mut $payload_ty> {
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());

                if let Some(p) = self.as_object_mut().payload_mut().other_mut() {
                    let ptr = p.opaque();
                    if !ptr.is_null() {
                        Some(unsafe { &mut *ptr.cast::<$payload_ty>() })
                    } else {
                        None
                    }
                } else {
                    unreachable!()
                }
            }

            pub fn take_payload(&mut self) -> Option<Box<$payload_ty>>{
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());
                if let Some(p) = self.as_object_mut().payload_mut().other_mut() {
                    unsafe {
                        let ptr = p.replace_opaque(std::ptr::null_mut());
                        if !ptr.is_null() {
                            Some(Box::from_raw(ptr.cast::<$payload_ty>()))
                        } else {
                            None
                        }
                    }
                } else {
                    None
                }
            }

            /// create an instance of the class with attached payload
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
                payload: $payload_ty,
            ) -> Result<Self, i32> {
                let p = Box::new(payload);
                let opaque = Box::into_raw(p).cast::<std::ffi::c_void>();
                debug_assert!(!opaque.is_null());

                ctx.create_class_instance_with_opaque(new_target, <Self as $crate::NativeObject>::class_id(), opaque)
                    .map(Self)
                    .map_err(|_| {
                        unsafe {
                            let _ = Box::from_raw(opaque.cast::<$payload_ty>());
                        }
                        -1
                    })
            }
        }
    };

    // ── Public: no payload ──
    ($name:expr, $native_ty:ident, (), $proto_entries:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object!(@native-no-payload $name, $native_ty, (), $proto_entries, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);
        $crate::def_native_object!(@impl $native_ty);
    };

    // ── Public: with payload ──
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, on_payload_gc: $on_payload_gc:expr)?) => {
        $crate::def_native_object!(@native-with-payload $name, $native_ty, $payload_ty, $proto_entries, $ctor
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            $(, on_payload_gc: $on_payload_gc)?);
        $crate::def_native_object!(@impl $native_ty);
    };
}
