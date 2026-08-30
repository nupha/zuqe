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
}

impl JSValue {
    pub fn as_native_object<O: NativeObject>(&self) -> Option<O> {
        self.as_object()
            .and_then(|o| o.as_native_object::<O>().cloned())
    }
}

/// Define a native object class.
///
/// Optionally supports `extends: $class_id` for inheritance — see `@register` doc.
#[macro_export]
macro_rules! def_native_object {
    // ── @struct: generate struct definition and built-in impls ──
    // `@wire_user_payload` is passed only by the with-payload variants: it
    // wires the monomorphized free (`drop_payload_fn::<P>`) and gc-trace
    // (`user_payload_trace::<P>`) channels into the class def. Both fns carry
    // a `P: GcTrace` bound, so a payload type that is not `GcTrace` (e.g. a
    // missing `#[derive(GcTrace)]`) fails to compile right at the macro call.
    (@struct $name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?
        $(, @wire_user_payload: $wire:ty)?) => {
        /// Native object type
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $native_ty($crate::Object);

        impl $native_ty {
            pub const CLASS_NAME: &str = $name;

            thread_local! {
                static CLASS_ID: std::cell::Cell<$crate::ClassID> = const { std::cell::Cell::new(0) };
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

                // Default: no extends (normal class)
                let mut cd = $crate::ClassDef::new($name, $proto_entries, $ctor);
                // If extends: was provided, use new_extends helper
                $(
                    cd = $crate::ClassDef::new_extends($name, $proto_entries, $ctor, $extends_class);
                )?

                // Owned payload: engine-side free + gc-trace channels,
                // monomorphized on the payload type.
                $(
                    cd = cd.payload_drop($crate::drop_payload_fn::<$wire>);
                    cd = cd.on_gc_trace($crate::user_payload_trace::<$wire>);
                )?

                // set class unregister handler
                $(cd = cd.on_unregister($on_unregister);)?

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

            #[inline(always)]
            fn try_from(v: $crate::JSValue) -> Result<Self, Self::Error> {
                v.as_native_object::<Self>().ok_or(-1)
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
        $(, extends: $extends_class:expr)?
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object!(@struct $name, $native_ty, (), $proto_entries, $ctor
            $(, extends: $extends_class)?
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
        $(, extends: $extends_class:expr)?
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object!(@struct $name, $native_ty, $payload_ty, $proto_entries, $ctor
            $(, extends: $extends_class)?
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?
            , @wire_user_payload: $payload_ty);

        impl $native_ty {
            /// get payload data
            pub fn payload(&self) -> Option<&$payload_ty> {
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());

                let ptr = self.as_object().payload_raw();
                if !ptr.is_null() {
                    Some(unsafe { &*ptr.cast::<$payload_ty>() })
                } else {
                    None
                }
            }

            /// get payload data mut
            pub fn payload_mut(&mut self) -> Option<&mut $payload_ty> {
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());

                let ptr = self.as_object_mut().payload_raw();
                if !ptr.is_null() {
                    Some(unsafe { &mut *ptr.cast::<$payload_ty>() })
                } else {
                    None
                }
            }

            pub fn take_payload(&mut self) -> Option<Box<$payload_ty>>{
                debug_assert_eq!(<Self as $crate::NativeObject>::class_id(), self.as_object().class_id());
                unsafe {
                    let ptr = self.as_object_mut().replace_payload(std::ptr::null_mut());
                    if !ptr.is_null() {
                        Some(Box::from_raw(ptr.cast::<$payload_ty>()))
                    } else {
                        None
                    }
                }
            }

            /// Extract payload from a `JSValue` by class id check.
            ///
            /// Returns `Some(&'a $payload_ty)` if `val` is an object of this native class,
            /// or `None` otherwise.
            ///
            /// The returned reference borrows from the GC heap. Its lifetime `'a` is tied to
            /// the borrow of `val`, which keeps the underlying JS object (a GC root) alive for
            /// as long as the payload reference is in use — preventing the object from being
            /// reclaimed while the payload is still borrowed.
            pub fn payload_of<'a>(val: &'a $crate::JSValue) -> Option<&'a $payload_ty> {
                let o = val.as_object()?;
                if o.class_id() != <Self as $crate::NativeObject>::class_id() {
                    return None;
                }
                // Safety: the payload pointer points to a heap-allocated Box<$payload_ty>
                // (created via Box::into_raw in new_instance). As long as the JS object
                // is alive in the GC heap, this allocation remains valid.
                let ptr = o.payload_raw();
                if !ptr.is_null() {
                    Some(unsafe { &*ptr.cast::<$payload_ty>() })
                } else {
                    None
                }
            }

            /// Extract mutable payload from a `&mut JSValue` by class id check.
            ///
            /// Returns `Some(&'a mut $payload_ty)` if `val` is an object of this native
            /// class, or `None` otherwise.
            ///
            /// # Safety
            ///
            /// The returned reference borrows from the GC heap, not from `val`. The caller
            /// must ensure the underlying JS object remains uniquely GC-rooted for the
            /// lifetime `'a` and that no aliasing `&mut` access to the payload exists.
            pub fn payload_mut_of<'a>(val: &'a mut $crate::JSValue) -> Option<&'a mut $payload_ty> {
                let mut o = val.as_object()?;
                if o.class_id() != <Self as $crate::NativeObject>::class_id() {
                    return None;
                }
                // Safety: the payload pointer points to a heap-allocated Box<$payload_ty>
                // (created via Box::into_raw in new_instance). Mutability is gated by the
                // &'a mut JSValue — the caller must not alias the payload during 'a.
                let ptr = o.payload_raw();
                if !ptr.is_null() {
                    Some(unsafe { &mut *ptr.cast::<$payload_ty>() })
                } else {
                    None
                }
            }

            /// Take (detach) the payload from a `&mut JSValue` by class id check.
            ///
            /// Returns `Some(Box<$payload_ty>)` if `val` is an object of this native
            /// class with an attached payload, replacing the object's payload pointer
            /// with null so ownership of the payload transfers to the caller. Returns
            /// `None` otherwise (wrong class or already detached).
            pub fn take_payload_of(val: &mut $crate::JSValue) -> Option<Box<$payload_ty>> {
                let mut o = val.as_object()?;
                if o.class_id() != <Self as $crate::NativeObject>::class_id() {
                    return None;
                }
                // Safety: the payload pointer, if non-null, points to a
                // heap-allocated Box<$payload_ty> created in new_instance.
                unsafe {
                    let ptr = o.replace_payload(std::ptr::null_mut());
                    if !ptr.is_null() {
                        Some(Box::from_raw(ptr.cast::<$payload_ty>()))
                    } else {
                        None
                    }
                }
            }

            /// create an instance of the class with attached payload
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
                payload: $payload_ty,
            ) -> Result<Self, i32> {
                ctx.create_class_instance_with_payload(new_target, <Self as $crate::NativeObject>::class_id(), payload)
                    .map(Self)
                    .map_err(|_| -1)
            }
        }
    };

    // ── Public: no payload ──
    ($name:expr, $native_ty:ident, (), $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object!(@native-no-payload $name, $native_ty, $proto_entries, $ctor
            $(, extends: $extends_class)?
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);
        $crate::def_native_object!(@impl $native_ty);
    };

    // ── Public: with payload ──
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?
        $(, on_class_register: $on_register:expr)?
        $(, on_class_unregister: $on_unregister:expr)?) => {
        $crate::def_native_object!(@native-with-payload $name, $native_ty, $payload_ty, $proto_entries, $ctor
            $(, extends: $extends_class)?
            $(, on_class_register: $on_register)?
            $(, on_class_unregister: $on_unregister)?);
        $crate::def_native_object!(@impl $native_ty);
    };
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    use gc_lite::GcTrace;

    use crate::{ClassDef, JSValue, Runtime, RuntimeImpl, ScopedContext};

    /// Payload carrying a magic value: `on_dispose` must observe it while the
    /// allocation is still valid.
    #[derive(GcTrace)]
    struct DispPayload {
        magic: u32,
    }

    /// Monotonic sequence: bumped once when the free channel runs.
    static FREE_SEQ: AtomicUsize = AtomicUsize::new(0);
    /// Sequence value observed by `on_dispose` — `0` proves dispose ran
    /// strictly before the free channel.
    static DISPOSE_SEQ: AtomicUsize = AtomicUsize::new(usize::MAX);
    /// Magic read through the payload pointer inside `on_dispose`.
    static DISPOSE_MAGIC: AtomicU32 = AtomicU32::new(0);

    /// Free channel for the test class (custom, to make the ordering
    /// observable; the macro normally wires `drop_payload_fn::<P>` here).
    unsafe fn test_free(ptr: *mut c_void) {
        FREE_SEQ.fetch_add(1, Ordering::SeqCst);
        if !ptr.is_null() {
            drop(Box::from_raw(ptr as *mut DispPayload));
        }
    }

    fn test_dispose(_rt: &mut RuntimeImpl, ptr: *mut c_void, _cid: usize, _id: usize) {
        DISPOSE_SEQ.store(FREE_SEQ.load(Ordering::SeqCst), Ordering::SeqCst);
        if !ptr.is_null() {
            // Reading through the pointer is only sound because dispose runs
            // BEFORE the free channel — exactly the contract under test.
            let p = unsafe { &*(ptr as *const DispPayload) };
            DISPOSE_MAGIC.store(p.magic, Ordering::SeqCst);
        }
    }

    /// 回归(U1):`on_dispose` 必须在 payload 释放之前、携带仍有效的 payload
    /// 指针触发,释放随后由 `payload_drop` 完成。
    #[test]
    fn on_dispose_fires_before_payload_free() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let class_id = rt.register_class_def(
            ClassDef::new("DispTest", &[], None)
                .on_dispose(test_dispose)
                .payload_drop(test_free),
        )?;

        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            cx.use_class(class_id)?;
            Ok(())
        })?;

        // Create the instance in a child scope and let it die with the scope:
        // objects created in a live scope are scope-locals and survive
        // collection until the scope pops (the context's root scope outlives
        // the test).
        ctx.with_new_scope(|cx| {
            let _obj = cx.create_class_instance_with_payload(
                JSValue::UNDEFINED,
                class_id,
                DispPayload { magic: 0xCAFE },
            )?;
            Ok::<(), i32>(())
        })?;

        rt.gc_collect();

        // dispose 时 payload 仍有效(magic 完整)...
        assert_eq!(DISPOSE_MAGIC.load(Ordering::SeqCst), 0xCAFE);
        // ...且严格先于 free 通道(free 之后不会再触发 dispose)。
        assert_eq!(DISPOSE_SEQ.load(Ordering::SeqCst), 0);
        assert!(FREE_SEQ.load(Ordering::SeqCst) >= 1);
        Ok(())
    }
}
