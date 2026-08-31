// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_array;

pub use class_array::*;

crate::def_native_object!(
    builtin,
    "Array",
    CLASS_DEF_ARRAY,
    class_id: (crate::JS_CLASS_ARRAY, crate::Atom::CONST_Array),
    crate::array::ArrayPayload,
    js_array_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_array_constructor, 1, true)
            .entries(js_array_funcs)
            .expose_to_global(),
    )
);

crate::def_native_object!(
    builtin,
    "ArrayIterator",
    CLASS_DEF_ARRAY_ITERATOR,
    class_id: (crate::JS_CLASS_ARRAY_ITERATOR, crate::Atom::CONST_Array_Iterator),
    crate::builtins::iterator::ArrayIterPayload,
    js_array_iterator_proto_funcs,
    None
);
