// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_regexp;
mod class_regexp_iter;
mod re;

pub use {class_regexp::*, class_regexp_iter::*, re::*};

use crate::{Atom, ClassDef};

pub const CLASS_DEF_REGEXP: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_RegExp,
    proto_entries: js_regexp_proto_funcs,
    ctor_entries: js_regexp_funcs,
    call: None,
    exotic: None,
    initialize: Some(RegExp::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_REGEXP_STRING_ITERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_RegExp_String_Iterator,
    proto_entries: js_regexp_string_iterator_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: Some(RegExp::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
