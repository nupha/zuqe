mod host;
mod stub;

use zuqe_zapi::ffi::ZqClassDef;

use crate::ClassDef;

/// A zapi-registered class, keyed by class id in the runtime's per-runtime
/// table (index = `class_id - static_class_count()`): the engine-native
/// `ClassDef` plus the extension's ABI `ZqClassDef`.
pub(crate) struct ZapiClassDef {
    pub(crate) def: ClassDef,
    pub(crate) zq_def: *const ZqClassDef,
}

