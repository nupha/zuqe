// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod base64hex;
mod class_dataview;
mod class_typedarray;
mod dv;
mod ta;

pub use {class_dataview::*, class_typedarray::*, dv::*, ta::*};

use crate::object::{DataView, TypedArray};

crate::def_native_object!(
    builtin,
    "Uint8ClampedArray",
    CLASS_DEF_UINT8C_ARRAY,
    class_id: (crate::JS_CLASS_UINT8C_ARRAY, crate::Atom::CONST_Uint8ClampedArray),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Uint8Array",
    CLASS_DEF_UINT8_ARRAY,
    class_id: (crate::JS_CLASS_UINT8_ARRAY, crate::Atom::CONST_Uint8Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Int8Array",
    CLASS_DEF_INT8_ARRAY,
    class_id: (crate::JS_CLASS_INT8_ARRAY, crate::Atom::CONST_Int8Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Uint16Array",
    CLASS_DEF_UINT16_ARRAY,
    class_id: (crate::JS_CLASS_UINT16_ARRAY, crate::Atom::CONST_Uint16Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Int16Array",
    CLASS_DEF_INT16_ARRAY,
    class_id: (crate::JS_CLASS_INT16_ARRAY, crate::Atom::CONST_Int16Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Uint32Array",
    CLASS_DEF_UINT32_ARRAY,
    class_id: (crate::JS_CLASS_UINT32_ARRAY, crate::Atom::CONST_Uint32Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Int32Array",
    CLASS_DEF_INT32_ARRAY,
    class_id: (crate::JS_CLASS_INT32_ARRAY, crate::Atom::CONST_Int32Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Float16Array",
    CLASS_DEF_FLOAT16_ARRAY,
    class_id: (crate::JS_CLASS_FLOAT16_ARRAY, crate::Atom::CONST_Float16Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Float32Array",
    CLASS_DEF_FLOAT32_ARRAY,
    class_id: (crate::JS_CLASS_FLOAT32_ARRAY, crate::Atom::CONST_Float32Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "Float64Array",
    CLASS_DEF_FLOAT64_ARRAY,
    class_id: (crate::JS_CLASS_FLOAT64_ARRAY, crate::Atom::CONST_Float64Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "BigInt64Array",
    CLASS_DEF_BIG_INT64_ARRAY,
    class_id: (crate::JS_CLASS_BIG_INT64_ARRAY, crate::Atom::CONST_BigInt64Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "BigUint64Array",
    CLASS_DEF_BIG_UINT64_ARRAY,
    class_id: (crate::JS_CLASS_BIG_UINT64_ARRAY, crate::Atom::CONST_BigUint64Array),
    crate::builtins::typed_array::TypedArrayPayload, (), None,
    init_fn: TypedArray::class_init
);

crate::def_native_object!(
    builtin,
    "DataView",
    CLASS_DEF_DATAVIEW,
    class_id: (crate::JS_CLASS_DATAVIEW, crate::Atom::CONST_DataView),
    crate::builtins::typed_array::DataViewPayload,
    js_dataview_proto_funcs,
    Some(crate::class::ClassCtorDef::new(js_dataview_constructor, 1, false).expose_to_global())
);
