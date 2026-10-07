mod host;
mod stub;

use std::ffi::c_void;

/// The zapi ABI/API contract this engine build speaks.
///
/// Re-exported so embedders (e.g. aike) can report the numbers they must agree
/// on without taking a direct `zuqe_zapi` dependency — `zuqe` stays the single
/// source of truth for the contract.
pub use zuqe_zapi::ffi::{ZQ_ABI_BASE, ZQ_ABI_VERSION, ZQ_API_BASE, ZQ_API_VERSION};

use zuqe_zapi::ffi::{ZqClassDef, ZqGcTraceFn};

use crate::ClassDef;

/// Reborrow a string that lives in a zapi extension's static data as `'static`.
///
/// # Safety
/// `s` must point into a cdylib that is kept mapped for the whole process
/// lifetime (see `host::load_ext_lib`). Every string the extension hands to the
/// host — `ZqClassDef.name`, `ZqFieldDef.name`/`str_val` — is a literal in the
/// extension's read-only data, so this holds. Used instead of `Box::leak` to
/// avoid a per-runtime name copy (which would accumulate across runtimes).
pub(crate) unsafe fn ext_str_static(s: &str) -> &'static str {
    unsafe { std::mem::transmute::<&str, &'static str>(s) }
}

/// A zapi-registered class, keyed by class id in the runtime's per-runtime
/// table (index = `class_id - static_class_count()`): the engine-native
/// `ClassDef` plus the extension's ABI `ZqClassDef`.
pub(crate) struct ZapiClassDef {
    pub(crate) def: ClassDef,
    pub(crate) zq_def: *const ZqClassDef,
}

/// A registered global (thread_local) GC trace from zapi: the extension's callback plus
/// its opaque. Owned by the runtime (`RuntimeImpl::zapi_gc_tracers`) and
/// freed when the runtime drops.
pub(crate) struct ZapiGcTracer {
    pub(crate) trace_fn: ZqGcTraceFn,
    pub(crate) opaque: *mut c_void,
}
