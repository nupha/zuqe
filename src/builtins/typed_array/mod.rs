// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_dataview;
mod class_typedarray;
mod dv;
mod ta;

pub use {class_dataview::*, class_typedarray::*, dv::*, ta::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

macro_rules! ta_class_def {
    ($name:ident) => {
        ClassDef {
            name: "",
            name_atom: Atom::$name,
            call: None,
            exotic: None,
            initialize: ClassInitType::InitFn(TypedArray::class_init),
            finalize: Some(TypedArray::class_finalize),
            finalize_opaque: None,
            unregister: None,
        }
    };
}

pub const CLASS_DEF_UINT8C_ARRAY: ClassDef = ta_class_def!(CONST_Uint8ClampedArray);
pub const CLASS_DEF_UINT8_ARRAY: ClassDef = ta_class_def!(CONST_Uint8Array);
pub const CLASS_DEF_INT8_ARRAY: ClassDef = ta_class_def!(CONST_Int8Array);
pub const CLASS_DEF_UINT16_ARRAY: ClassDef = ta_class_def!(CONST_Uint16Array);
pub const CLASS_DEF_INT16_ARRAY: ClassDef = ta_class_def!(CONST_Int16Array);
pub const CLASS_DEF_UINT32_ARRAY: ClassDef = ta_class_def!(CONST_Uint32Array);
pub const CLASS_DEF_INT32_ARRAY: ClassDef = ta_class_def!(CONST_Int32Array);
pub const CLASS_DEF_FLOAT32_ARRAY: ClassDef = ta_class_def!(CONST_Float32Array);
pub const CLASS_DEF_FLOAT64_ARRAY: ClassDef = ta_class_def!(CONST_Float64Array);
pub const CLASS_DEF_BIG_INT64_ARRAY: ClassDef = ta_class_def!(CONST_BigInt64Array);
pub const CLASS_DEF_BIG_UINT64_ARRAY: ClassDef = ta_class_def!(CONST_BigUint64Array);

pub const CLASS_DEF_DATAVIEW: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_DataView,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_dataview_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef::new(js_dataview_constructor, 1, false).export_to_global()),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
