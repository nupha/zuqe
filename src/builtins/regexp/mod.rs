// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_regexp;
mod class_regexp_iter;
mod re;

pub use {class_regexp::*, class_regexp_iter::*, re::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub const CLASS_DEF_REGEXP: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_RegExp,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_regexp_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_regexp_constructor, 2, true)
                .entries(js_regexp_funcs)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_REGEXP_STRING_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_RegExp_String_Iterator,
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(RegExp::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
