// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use gc_lite::GcTrace;

use crate::{USER_CLASS_ID_START, class::ClassID};

include!(concat!(env!("OUT_DIR"), "/payload_accessors.rs"));

impl super::ObjectImpl {
    #[inline(always)]
    pub fn payload_ptr(&self) -> *mut c_void {
        self.payload
    }

    #[inline(always)]
    pub(crate) fn payload_ptr_set(&mut self, ptr: *mut c_void) {
        self.payload = ptr;
    }
}

/// The single payload free exit. Both `ObjectImpl::finalize` and
/// [`PayloadPtr::drop`] release through here, so a payload allocation can
/// never be freed by two different code paths with different type views.
///
/// # Safety
/// `ptr` must either be null or have been produced by
/// `Box::into_raw(Box::new(...))` of the payload type registered for
/// `class_id` (i.e. exactly what [`PayloadPtr`] hands out).
pub(crate) unsafe fn free_payload(class_id: ClassID, ptr: *mut c_void) {
    if ptr.is_null() {
        return;
    }
    if class_id < USER_CLASS_ID_START {
        drop_payload(class_id, ptr);
    } else {
        drop(Box::from_raw(ptr as *mut crate::class::OtherPayload));
    }
}

/// Typed, owning handle to a freshly allocated object payload.
///
/// A payload starts its life boxed and unattached: between `Box::into_raw`
/// and the object entering the GC heap there is fallible work (shape lookup,
/// allocation), and a dropped/failed construction must not leak the box.
/// `PayloadPtr` owns the box during that window — on error it is dropped and
/// the payload freed via [`free_payload`]; on success [`PayloadPtr::into_raw`]
/// transfers ownership into `ObjectImpl::payload`.
///
/// The concrete payload type is never recovered from the handle itself: it is
/// reconstructed from `class_id` by the generated `drop_payload` dispatch
/// (single source of truth: `CLASS_PAYLOAD_TYPE` in `build_stdclass.rs`).
#[derive(Debug)]
pub(crate) struct PayloadPtr {
    class_id: ClassID,
    ptr: *mut c_void,
}

impl PayloadPtr {
    /// Box `value` as the payload of builtin class `class_id`.
    #[inline]
    pub(crate) fn new<T: GcTrace>(class_id: ClassID, value: T) -> Self {
        debug_assert!(class_id < USER_CLASS_ID_START, "builtin class expected");
        #[cfg(debug_assertions)]
        {
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
        }
    }

    /// Payload of a user class instance: always `Box<OtherPayload>`.
    #[inline]
    pub(crate) fn other(class_id: ClassID, opaque: *mut c_void) -> Self {
        debug_assert!(class_id >= USER_CLASS_ID_START, "user class expected");
        Self {
            class_id,
            ptr: Box::into_raw(Box::new(crate::class::OtherPayload::new(class_id, opaque)))
                as *mut c_void,
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

    /// Transfer ownership into a successfully created `ObjectImpl`: consume
    /// the handle without freeing. The object's `finalize` becomes the owner
    /// and frees via [`free_payload`].
    #[inline]
    pub(crate) fn into_raw(self) -> *mut c_void {
        let ptr = self.ptr;
        std::mem::forget(self);
        ptr
    }
}

impl Drop for PayloadPtr {
    #[inline]
    fn drop(&mut self) {
        unsafe { free_payload(self.class_id, self.ptr) };
    }
}

/// Payload argument bound of the `*_class` create helpers. Implementations
/// are generated per registered payload type (see `build_stdclass.rs
/// gen_payload_accessors`), so only types the drop/trace dispatch knows can
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
