// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_array;

pub use class_array::*;

use crate::{
    Atom,
    class::{ClassCtorDef, ClassDef, ClassInitType},
};

pub const CLASS_DEF_ARRAY: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Array,
    call: None,
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: js_array_proto_funcs,
        ctor: Some(
            ClassCtorDef::new(js_array_constructor, 1, true)
                .entries(js_array_funcs)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_ARRAY_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Array_Iterator,
    call: None,
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: js_array_iterator_proto_funcs,
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
