// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_array;

pub use class_array::*;

use crate::{
    Atom,
    class::{ClassCtorDef, ClassDef},
};

pub(crate) static CLASS_DEF_ARRAY: ClassDef = ClassDef::builtin(
    Atom::CONST_Array,
    js_array_proto_funcs,
    Some(
        ClassCtorDef::new(js_array_constructor, 1, true)
            .entries(js_array_funcs)
            .global(),
    ),
);

pub(crate) static CLASS_DEF_ARRAY_ITERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_Array_Iterator,
    js_array_iterator_proto_funcs,
    None,
);
