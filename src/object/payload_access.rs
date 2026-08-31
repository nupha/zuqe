// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{USER_CLASS_ID_START, class::ClassID};

include!(concat!(env!("OUT_DIR"), "/payload_accessors.rs"));

/// Monomorphized payload free channel, uniform for builtin and user classes.
/// Wired into `ClassDef::payload_drop` at registration — builtin classes by
/// the generator (`wire_builtin_payload_drops`), user classes by
/// `def_native_object!` — so `ObjectImpl::finalize` frees every payload
/// through the drop callback registered in the class registry, keyed by
/// class_id alone. `PayloadPtr` additionally captures the same function in
/// the handle so the creation error path can free without touching the
/// registry (Drop cannot take a `&RuntimeImpl`).
///
/// Classes whose payload the engine does not own (zapi opaque pointers) and
/// payload-less classes register no drop callback and are never freed here.
///
/// # Safety
/// `ptr` must either be null or have been produced by
/// `Box::into_raw(Box::<P>::new(...))`.
pub unsafe fn drop_payload_fn<P>(ptr: *mut c_void) {
    if !ptr.is_null() {
        unsafe {
            drop(Box::from_raw(ptr.cast::<P>()));
        }
    }
}

/// GC trace channel for user-class payloads: traces `*ptr` as `P` (the
/// payload type must implement [`GcTrace`] — typically via
/// `#[derive(GcTrace)]`). Wired into `ClassDef::on_gc_trace` at registration.
pub fn user_payload_trace<P: GcTrace>(ptr: *mut c_void, gcx: &mut GcTraceCtx, _class_id: usize) {
    debug_assert!(!ptr.is_null());
    unsafe { (*ptr.cast::<P>()).trace(gcx) };
}

/// Typed, owning handle to a freshly allocated object payload.
///
/// A payload starts its life boxed and unattached: between `Box::into_raw`
/// and the object entering the GC heap there is fallible work (shape lookup,
/// allocation), and a dropped/failed construction must not leak the box.
/// `PayloadPtr` owns the box during that window — on error it is dropped and
/// the payload freed; on success [`PayloadPtr::into_raw`] transfers ownership
/// into `ObjectImpl::payload`, whose `finalize` frees via the drop callback
/// registered in `ClassDef::payload_drop`.
///
/// The handle carries the monomorphized [`drop_payload_fn::<P>`] itself, so
/// the error path frees without needing the class registry (and `Drop` needs
/// no `&RuntimeImpl`, which it structurally cannot obtain).
#[derive(Debug)]
pub(crate) struct PayloadPtr {
    class_id: ClassID,
    ptr: *mut c_void,
    /// Drop function captured at construction (`drop_payload_fn::<P>`).
    /// `None` only for payloads the engine does not own (opaque, zapi) and
    /// for the null payload of `none()`.
    drop_fn: Option<unsafe fn(*mut c_void)>,
}

impl PayloadPtr {
    /// Box `value` as the payload of class `class_id` (builtin or user
    /// class — the free channel is uniform for both).
    #[inline]
    pub(crate) fn new<T: GcTrace>(class_id: ClassID, value: T) -> Self {
        #[cfg(debug_assertions)]
        if class_id < USER_CLASS_ID_START {
            let expected = payload_type_name(class_id);
            debug_assert!(
                expected == Some(std::any::type_name::<T>()),
                "payload type mismatch for class {class_id}: registry expects {expected:?}, got {}",
                std::any::type_name::<T>(),
            );
        }
        Self {
            class_id,
            ptr: Box::into_raw(Box::new(value)) as *mut c_void,
            drop_fn: Some(drop_payload_fn::<T>),
        }
    }

    /// An engine-unowned opaque payload (zapi): stored as-is, never freed or
    /// dereferenced by the engine — passed back verbatim in trace/dispose
    /// callbacks.
    #[inline]
    pub(crate) fn opaque(class_id: ClassID, ptr: *mut c_void) -> Self {
        debug_assert!(class_id >= USER_CLASS_ID_START, "user class expected");
        Self {
            class_id,
            ptr,
            drop_fn: None,
        }
    }

    /// No payload (OBJECT, ERROR, RAWJSON, MODULE_NS, ...). The class id is
    /// still carried: `ObjectImpl::create` derives the object's class solely
    /// from the handle, so even a payload-less handle must know its class.
    #[inline]
    pub(crate) fn none(class_id: ClassID) -> Self {
        debug_assert!(
            class_id > 0 && class_id < USER_CLASS_ID_START,
            "payload-less classes are builtin classes"
        );
        #[cfg(debug_assertions)]
        debug_assert!(
            payload_type_name(class_id).is_none(),
            "class {class_id} has a registered payload type; use new() instead of none()"
        );
        Self {
            class_id,
            ptr: std::ptr::null_mut(),
            drop_fn: None,
        }
    }

    /// The class this payload belongs to. Single source of truth for the
    /// created object: `ObjectImpl::create` derives its class from here.
    #[inline]
    pub(crate) const fn class_id(&self) -> ClassID {
        self.class_id
    }

    #[inline]
    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.ptr
    }
}

impl Drop for PayloadPtr {
    #[inline]
    fn drop(&mut self) {
        if let Some(f) = self.drop_fn {
            unsafe { f(self.ptr) };
        }
    }
}

/// Payload argument bound of the `*_class` create helpers. Implementations
/// are generated per registered payload type (see `build_stdclass.rs
/// gen_payload_accessors`), so only types the trace dispatch knows can
/// become a payload — anything else (including a wrongly re-boxed `Box<T>`)
/// fails at compile time.
///
/// Deliberately NOT implemented for [`PayloadPtr`]: a pre-built handle
/// already carries its class, and the `*_class` helpers take `class_id` —
/// passing a handle there would duplicate the class id with nothing keeping
/// the copies in sync. Handle callers must use the `*_payload` helpers,
/// which take no `class_id` at all; the type system keeps the two families
/// apart.
pub(crate) trait IntoPayloadPtr {
    fn into_payload_ptr(self, class_id: ClassID) -> PayloadPtr;
}
