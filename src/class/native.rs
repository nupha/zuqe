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

    /// Class name for debugging/logging.
    fn class_name() -> &'static str;

    /// Check if an Object is an instance of this type.
    #[inline(always)]
    fn is_instance_of(obj: &Object) -> bool {
        obj.class_id() == Self::class_id()
    }

    /// Safely convert from `&Object` to `&Self`.
    #[inline(always)]
    fn from_object(obj: &Object) -> Option<&Self> {
        if obj.class_id() == Self::class_id() {
            Some(unsafe { std::mem::transmute::<&Object, &Self>(obj) })
        } else {
            None
        }
    }

    /// Safely convert from `JSValue` to `Option<Self>`.
    #[inline(always)]
    fn from_value(val: JSValue) -> Option<Self> {
        let obj = val.as_object()?;
        Self::from_object(&obj).cloned()
    }
}

impl Object {
    #[deprecated(note = "use NativeObject::from_object() instead")]
    pub fn as_native_object<O: NativeObject>(&self) -> Option<&O> {
        NativeObject::from_object(self)
    }
}

impl JSValue {
    #[deprecated(note = "use NativeObject::from_value() instead")]
    pub fn as_native_object<O: NativeObject>(&self) -> Option<O> {
        NativeObject::from_value(*self)
    }
}

/// Define a native object class.
///
/// Optionally supports `extends: $class_id` for inheritance — see `@register` doc.
#[macro_export]
macro_rules! def_native_object {
    // ── @struct_core: shared struct definition + trait impls ──
    // Generates the struct, CLASS_NAME, as_object methods, and the conversion
    // trait impls shared by every class. `$kind` is the literal `native` or
    // `builtin`. Class registration into the global table happens at engine
    // init via linkme/distributed slice (native) or the generated
    // `BUILTIN_CLASS_DEFS` (builtin) — no per-runtime register() is generated.
    (@struct_core $name:expr, $native_ty:ident, $proto_entries:expr, $ctor:expr, $kind:tt
        $(, class_id: $class_id:expr)?
        $(, extends: $extends_class:expr)?
        $(, @wire_user_payload: $wire:ty)?
        $(, init_fn: $init_fn:expr)?
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?) => {
        #[repr(transparent)]
        #[derive(Debug, Clone, Copy, PartialEq)]
        pub struct $native_ty($crate::Object);

        impl $native_ty {
            pub const CLASS_NAME: &str = $name;

            #[inline(always)]
            pub const fn as_object(&self) -> &$crate::Object {
                &self.0
            }

            #[inline(always)]
            pub const fn as_object_mut(&mut self) -> &mut $crate::Object {
                &mut self.0
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
                <Self as $crate::NativeObject>::from_value(v).ok_or(-1)
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

    // ── @make_cd: construct the ClassDef ──
    (@make_cd $name:expr, $proto_entries:expr, $ctor:expr, native) => {
        $crate::ClassDef::new($name, $proto_entries, $ctor)
    };
    (@make_cd $name:expr, $proto_entries:expr, $ctor:expr, native
        , extends: $extends_class:expr) => {
        $crate::ClassDef::new_extends($name, $proto_entries, $ctor, $extends_class)
    };
    (@make_cd $name:expr, $proto_entries:expr, $ctor:expr, builtin
        , class_id: $class_id:expr) => {
        $crate::ClassDef::new_with_atom($name, $class_id.1, $proto_entries, $ctor)
    };
    (@make_cd $name:expr, $proto_entries:expr, $ctor:expr, builtin
        , class_id: $class_id:expr, extends: $extends_class:expr) => {{
        let mut cd = $crate::ClassDef::new_extends($name, $proto_entries, $ctor, $extends_class);
        cd.name_atom = $class_id.1;
        cd
    }};
    (@make_cd $name:expr, $proto_entries:expr, $ctor:expr, builtin
        , class_id: $class_id:expr, init_fn: $init_fn:expr) => {{
        let mut cd = $crate::ClassDef::new_with($name, $init_fn);
        cd.name_atom = $class_id.1;
        cd
    }};

    // ── @apply_extra: apply builtin-only hooks (exotic/on_call/on_finalize) ──
    (@apply_extra $cd:ident, builtin
        $(, extends: $extends_class:expr)?
        $(, @wire_user_payload: $wire:ty)?
        $(, init_fn: $init_fn:expr)?
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?) => {
        $($cd = $cd.exotic($exotic);)?
        $($cd = $cd.on_call($on_call);)?
        $($cd = $cd.on_finalize($on_finalize);)?
    };
    (@apply_extra $cd:ident, native
        $(, extends: $extends_class:expr)?
        $(, @wire_user_payload: $wire:ty)?
        $(, init_fn: $init_fn:expr)?
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?) => {};

    // ── @build_cd: combine @make_cd + @apply_extra + payload wiring ──
    // Produces a value expression for the ClassDef. Used in const position for
    // builtin classes and in linkme element initializers for native classes.
    (@build_cd $name:expr, $proto_entries:expr, $ctor:expr, $kind:tt
        $(, class_id: $class_id:expr)?
        $(, extends: $extends_class:expr)?
        $(, @wire_user_payload: $wire:ty)?
        $(, init_fn: $init_fn:expr)?
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?) => {{
        #[allow(unused_mut)]
        let mut cd = $crate::def_native_object!(@make_cd $name, $proto_entries, $ctor, $kind
            $(, class_id: $class_id)?
            $(, extends: $extends_class)?
            $(, init_fn: $init_fn)?);

        $crate::def_native_object!(@apply_extra cd, $kind
            $(, extends: $extends_class)?
            $(, @wire_user_payload: $wire)?
            $(, init_fn: $init_fn)?
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?);
        $(
            cd = cd.payload_drop($crate::drop_payload_fn::<$wire>);
            cd = cd.on_gc_trace($crate::user_payload_trace::<$wire>);
        )?
        cd
    }};

    // ── @impl: generate NativeObject trait impl for native classes ──
    // Reads the class_id from the linkme element's AtomicU16 (assigned at
    // engine init). Returns 0 until then (e.g. if used before init or the
    // class is never registered).
    (@impl $native_ty:ident) => {
        $crate::paste::paste! {
            unsafe impl $crate::NativeObject for $native_ty {
                fn class_id() -> $crate::ClassID {
                    [< $native_ty _DEF >].class_id.load(std::sync::atomic::Ordering::Relaxed)
                }
                fn class_name() -> &'static str {
                    Self::CLASS_NAME
                }
            }
        }
    };

    // ── @payload_accessors: shared payload accessor methods ──
    // Generates payload(), payload_mut(), take_payload(), payload_of(),
    // payload_mut_of(), take_payload_of() on the current type.
    (@payload_accessors $native_ty:ident, $payload_ty:ty) => {
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

            pub fn take_payload(&mut self) -> Option<Box<$payload_ty>> {
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

            pub fn payload_of(val: &$crate::JSValue) -> Option<&$payload_ty> {
                let o = val.as_object()?;
                if o.class_id() != <Self as $crate::NativeObject>::class_id() {
                    return None;
                }
                let ptr = o.payload_raw();
                if !ptr.is_null() {
                    Some(unsafe { &*ptr.cast::<$payload_ty>() })
                } else {
                    None
                }
            }

            pub fn payload_mut_of(val: &mut $crate::JSValue) -> Option<&mut $payload_ty> {
                let o = val.as_object()?;
                if o.class_id() != <Self as $crate::NativeObject>::class_id() {
                    return None;
                }
                let ptr = o.payload_raw();
                if !ptr.is_null() {
                    Some(unsafe { &mut *ptr.cast::<$payload_ty>() })
                } else {
                    None
                }
            }

            pub fn take_payload_of(val: &mut $crate::JSValue) -> Option<Box<$payload_ty>> {
                let mut o = val.as_object()?;
                if o.class_id() != <Self as $crate::NativeObject>::class_id() {
                    return None;
                }
                unsafe {
                    let ptr = o.replace_payload(std::ptr::null_mut());
                    if !ptr.is_null() {
                        Some(Box::from_raw(ptr.cast::<$payload_ty>()))
                    } else {
                        None
                    }
                }
            }
        }
    };

    // ── @native-no-payload: without payload ──
    (@native-no-payload $name:expr, $native_ty:ident, $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@struct_core $name, $native_ty, $proto_entries, $ctor, native
            $(, extends: $extends_class)?);

        // linkme element: collected into the process-global HOST_CLASS_DEFS.
        // class_id is assigned at engine init (init_static_class_defs).
        $crate::paste::paste! {
            #[doc(hidden)]
            #[linkme::distributed_slice($crate::HOST_CLASS_DEFS)]
            static [< $native_ty _DEF >]: $crate::HostClassDef = $crate::HostClassDef {
                def: $crate::def_native_object!(@build_cd $name, $proto_entries, $ctor, native
                    $(, extends: $extends_class)?),
                class_id: std::sync::atomic::AtomicU16::new(0),

                #[cfg(debug_assertions)]
                payload_type_name: || None,
            };
        }

        impl $native_ty {
            /// create an instance of the class
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
            ) -> Result<Self, i32> {
                ctx.create_class_instance_no_payload(new_target, <Self as $crate::NativeObject>::class_id())
                    .map(Self)
            }
        }
    };

    // ── @native-with-payload: with payload ──
    (@native-with-payload $name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@struct_core $name, $native_ty, $proto_entries, $ctor, native
            $(, extends: $extends_class)?
            , @wire_user_payload: $payload_ty);

        $crate::paste::paste! {
            #[doc(hidden)]
            #[linkme::distributed_slice($crate::HOST_CLASS_DEFS)]
            static [< $native_ty _DEF >]: $crate::HostClassDef = $crate::HostClassDef {
                def: $crate::def_native_object!(@build_cd $name, $proto_entries, $ctor, native
                    $(, extends: $extends_class)?
                    , @wire_user_payload: $payload_ty),
                class_id: std::sync::atomic::AtomicU16::new(0),

                #[cfg(debug_assertions)]
                payload_type_name: || Some(std::any::type_name::<$payload_ty>()),
            };
        }

        $crate::def_native_object!(@payload_accessors $native_ty, $payload_ty);

        impl $native_ty {
            /// create an instance of the class with attached payload
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
                payload: $payload_ty,
            ) -> Result<Self, i32> {
                ctx.create_class_instance(new_target, <Self as $crate::NativeObject>::class_id(), payload)
                    .map(Self)
                    .map_err(|_| -1)
            }
        }
    };

    // ── @builtin-struct: generate struct for builtin class ──
    // class_id is a compile-time constant; the ClassDef const is generated via
    // `paste!` (name = `<Type>_DEF`) and referenced by generated BUILTIN_CLASS_DEFS.
    // When init_fn is provided, $proto_entries/$ctor are ignored (use () for both).
    (@builtin-struct $name:expr, $native_ty:ident, $class_id:expr, $proto_entries:expr, $ctor:expr
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?
        $(, extends: $extends_class:expr)?
        $(, @wire_user_payload: $wire:ty)?
        $(, init_fn: $init_fn:expr)?) => {
        $crate::def_native_object!(@struct_core $name, $native_ty, $proto_entries, $ctor, builtin
            , class_id: $class_id
            $(, extends: $extends_class)?
            $(, @wire_user_payload: $wire)?
            $(, init_fn: $init_fn)?
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?);

        // The const ClassDef — referenced by generated BUILTIN_CLASS_DEFS.
        $crate::paste::paste! {
            #[doc(hidden)]
            pub const [< $native_ty _DEF >]: $crate::ClassDef =
                $crate::def_native_object!(@build_cd $name, $proto_entries, $ctor, builtin
                    , class_id: $class_id
                    $(, extends: $extends_class)?
                    $(, @wire_user_payload: $wire)?
                    $(, init_fn: $init_fn)?
                    $(, on_finalize: $on_finalize)?
                    $(, exotic: $exotic)?
                    $(, on_call: $on_call)?);
        }
    };

    // ── @builtin-no-payload: builtin without payload ──
    (@builtin-no-payload $name:expr, $native_ty:ident, $class_id:expr, $proto_entries:expr, $ctor:expr
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?
        $(, extends: $extends_class:expr)?
        $(, init_fn: $init_fn:expr)?) => {
        $crate::def_native_object!(@builtin-struct $name, $native_ty, $class_id, $proto_entries, $ctor
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?
            $(, extends: $extends_class)?
            $(, init_fn: $init_fn)?);

        impl $native_ty {
            /// create an instance of the class
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
            ) -> Result<Self, i32> {
                ctx.create_class_instance_no_payload(new_target, <Self as $crate::NativeObject>::class_id())
                    .map(Self)
            }
        }
    };

    // ── @builtin-with-payload: builtin with payload ──
    (@builtin-with-payload $name:expr, $native_ty:ident, $class_id:expr, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, init_fn: $init_fn:expr)?
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@builtin-struct $name, $native_ty, $class_id, $proto_entries, $ctor
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?
            $(, extends: $extends_class)?
            , @wire_user_payload: $payload_ty
            $(, init_fn: $init_fn)?);

        $crate::def_native_object!(@payload_accessors $native_ty, $payload_ty);

        impl $native_ty {
            /// create an instance of the class with attached payload
            #[inline(always)]
            pub fn new_instance(
                ctx: &mut $crate::ScopedContext,
                new_target: $crate::JSValue,
                payload: $payload_ty,
            ) -> Result<Self, i32> {
                ctx.create_class_instance(new_target, <Self as $crate::NativeObject>::class_id(), payload)
                    .map(Self)
            }
        }
    };

    // ── @builtin-impl: generate NativeObject impl for builtin (compile-time class_id) ──
    (@builtin-impl $native_ty:ident, $class_id:expr) => {
        unsafe impl $crate::NativeObject for $native_ty {
            fn class_id() -> $crate::ClassID {
                $class_id.0
            }
            fn class_name() -> &'static str {
                Self::CLASS_NAME
            }
        }
    };

    // ── Public: builtin no payload ──
    // def_native_object!(builtin, "Name", Type, class_id: (ID, ATOM), (), proto, ctor, ...)
    (builtin, $name:expr, $native_ty:ident,
        class_id: $class_id:expr,
        (), $proto_entries:expr, $ctor:expr
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@builtin-no-payload $name, $native_ty, $class_id, $proto_entries, $ctor
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?
            $(, extends: $extends_class)?);
        $crate::def_native_object!(@builtin-impl $native_ty, $class_id);
    };

    // ── Public: builtin with payload ──
    // def_native_object!(builtin, "Name", Type, class_id: (ID, ATOM), PayloadType, proto, ctor, ...)
    (builtin, $name:expr, $native_ty:ident,
        class_id: $class_id:expr,
        $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, init_fn: $init_fn:expr)?
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@builtin-with-payload $name, $native_ty, $class_id, $payload_ty, $proto_entries, $ctor
            $(, init_fn: $init_fn)?
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?
            $(, extends: $extends_class)?);
        $crate::def_native_object!(@builtin-impl $native_ty, $class_id);
    };

    // ── Public: builtin with init_fn (custom init, no proto/ctor) ──
    // def_native_object!(builtin, "Name", Type, class_id: (ID, ATOM), init_fn: INIT_FN, ...)
    (builtin, $name:expr, $native_ty:ident,
        class_id: $class_id:expr,
        init_fn: $init_fn:expr
        $(, on_finalize: $on_finalize:expr)?
        $(, exotic: $exotic:expr)?
        $(, on_call: $on_call:expr)?) => {
        $crate::def_native_object!(@builtin-no-payload $name, $native_ty, $class_id, (), ()
            $(, on_finalize: $on_finalize)?
            $(, exotic: $exotic)?
            $(, on_call: $on_call)?
            , init_fn: $init_fn);
        $crate::def_native_object!(@builtin-impl $native_ty, $class_id);
    };

    // ── Public: no payload (host class) ──
    ($name:expr, $native_ty:ident, (), $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@native-no-payload $name, $native_ty, $proto_entries, $ctor
            $(, extends: $extends_class)?);
        $crate::def_native_object!(@impl $native_ty);
    };

    // ── Public: with payload (host class) ──
    ($name:expr, $native_ty:ident, $payload_ty:ty, $proto_entries:expr, $ctor:expr
        $(, extends: $extends_class:expr)?) => {
        $crate::def_native_object!(@native-with-payload $name, $native_ty, $payload_ty, $proto_entries, $ctor
            $(, extends: $extends_class)?);
        $crate::def_native_object!(@impl $native_ty);
    };
}

/// Tests for host (external-crate) native classes. These are collected via the
/// linkme `HOST_CLASS_DEFS` slice and assigned ids at engine init — a distinct
/// path from zapi dynamic registration (tested in `src/zapi/stub.rs`), and one
/// that does not require the `zapi` feature.
#[cfg(test)]
mod tests {
    use gc_lite::GcTrace;

    use crate::{JSValue, NativeObject, Runtime, ScopedContext};

    #[derive(GcTrace, Debug, PartialEq)]
    pub struct HostPayload {
        value: u32,
    }

    crate::def_native_object!("HostTest", HostTest, HostPayload, &[], None);

    /// The linkme-collected class is present in the static table with an id
    /// assigned during `init_static_class_defs`.
    #[test]
    fn host_class_registered_at_engine_init() -> Result<(), i32> {
        let rt = Runtime::create()?;

        let cid = HostTest::class_id();
        assert_ne!(cid, 0, "host class id must be assigned during engine init");
        assert!(cid >= crate::HOST_CLASS_ID_START);
        assert_eq!(HostTest::class_name(), "HostTest");

        let cd = rt.class_def(cid).expect("host class def present");
        assert_eq!(cd.name, "HostTest");
        #[cfg(debug_assertions)]
        assert_eq!(
            cd.payload_type_name,
            Some(std::any::type_name::<HostPayload>())
        );
        Ok(())
    }

    /// Payload accessors generated by the macro round-trip through a live
    /// instance.
    #[test]
    fn host_class_payload_accessors_roundtrip() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let cid = HostTest::class_id();

        let mut ctx = rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            cx.use_class(cid)?;
            Ok(())
        })?;

        ctx.with_new_scope(|cx| {
            let mut obj =
                HostTest::new_instance(cx, JSValue::UNDEFINED, HostPayload { value: 42 })?;

            assert!(HostTest::is_instance_of(obj.as_object()));
            assert_eq!(obj.payload().map(|p| p.value), Some(42));

            obj.payload_mut().expect("mutable payload").value = 7;
            assert_eq!(obj.payload().map(|p| p.value), Some(7));

            assert_eq!(obj.take_payload().map(|p| p.value), Some(7));
            assert!(obj.payload().is_none(), "payload was moved out");
            Ok::<(), i32>(())
        })?;
        Ok(())
    }

    /// 回归：class id 表在进程级 once 初始化，多个 runtime 必须看到一致的
    /// static class def（builtin 与 host class 的 id/name 一致、class_count 一致）。
    /// per-runtime 仅做动态扩展（zapi class defs），不重排静态表。
    #[test]
    fn class_defs_consistent_across_runtimes() -> Result<(), i32> {
        let rt_a = Runtime::create()?;
        let rt_b = Runtime::create()?;

        assert_eq!(rt_a.class_count(), rt_b.class_count());

        let cid = HostTest::class_id();
        assert_eq!(rt_a.class_def(cid).map(|c| c.name), Some("HostTest"));
        assert_eq!(
            rt_a.class_def(cid).map(|c| c.name),
            rt_b.class_def(cid).map(|c| c.name),
            "host class def must be identical across runtimes"
        );

        for cid in [
            crate::JS_CLASS_OBJECT,
            crate::JS_CLASS_ARRAY,
            crate::JS_CLASS_ARRAY_BUFFER,
            crate::JS_CLASS_SHARED_ARRAY_BUFFER,
        ] {
            let a = rt_a.class_def(cid).map(|c| c.name);
            let b = rt_b.class_def(cid).map(|c| c.name);
            assert!(a.is_some(), "static class {cid} is registered");
            assert_eq!(a, b, "static class {cid} def must agree across runtimes");
        }
        Ok(())
    }
}
