// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod base64hex;
mod class_dataview;
mod class_typedarray;
mod dv;
mod ta;

pub use {class_dataview::*, class_typedarray::*, dv::*, ta::*};

pub use crate::object::{DataView, TypedArray};

use crate::{Atom, class::ClassDef};

macro_rules! ta_class_def {
    ($name:ident) => {
        ClassDef::builtin_with_init_func(Atom::$name, TypedArray::class_init)
            .on_finalize(TypedArray::class_finalize)
    };
}

pub(crate) static CLASS_DEF_UINT8C_ARRAY: ClassDef = ta_class_def!(CONST_Uint8ClampedArray);
pub(crate) static CLASS_DEF_UINT8_ARRAY: ClassDef = ta_class_def!(CONST_Uint8Array);
pub(crate) static CLASS_DEF_INT8_ARRAY: ClassDef = ta_class_def!(CONST_Int8Array);
pub(crate) static CLASS_DEF_UINT16_ARRAY: ClassDef = ta_class_def!(CONST_Uint16Array);
pub(crate) static CLASS_DEF_INT16_ARRAY: ClassDef = ta_class_def!(CONST_Int16Array);
pub(crate) static CLASS_DEF_UINT32_ARRAY: ClassDef = ta_class_def!(CONST_Uint32Array);
pub(crate) static CLASS_DEF_INT32_ARRAY: ClassDef = ta_class_def!(CONST_Int32Array);
pub(crate) static CLASS_DEF_FLOAT16_ARRAY: ClassDef = ta_class_def!(CONST_Float16Array);
pub(crate) static CLASS_DEF_FLOAT32_ARRAY: ClassDef = ta_class_def!(CONST_Float32Array);
pub(crate) static CLASS_DEF_FLOAT64_ARRAY: ClassDef = ta_class_def!(CONST_Float64Array);
pub(crate) static CLASS_DEF_BIG_INT64_ARRAY: ClassDef = ta_class_def!(CONST_BigInt64Array);
pub(crate) static CLASS_DEF_BIG_UINT64_ARRAY: ClassDef = ta_class_def!(CONST_BigUint64Array);

pub(crate) static CLASS_DEF_DATAVIEW: ClassDef = ClassDef::builtin(
    Atom::CONST_DataView,
    js_dataview_proto_funcs,
    Some(crate::class::ClassCtorDef::new(js_dataview_constructor, 1, false).expose_to_global()),
);
