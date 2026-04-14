// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_regexp;
mod class_regexp_iter;
mod re;

pub use {class_regexp::*, class_regexp_iter::*, re::*};

use crate::{
    Atom,
    class::{ClassCtorDef, ClassDef, ClassInitType},
};

pub(crate) const CLASS_DEF_REGEXP: ClassDef = ClassDef::builtin(
    Atom::CONST_RegExp,
    ClassInitType::Normal {
        proto_entries: js_regexp_proto_funcs,
        ctor: Some(
            ClassCtorDef::new(js_regexp_constructor, 2, true)
                .entries(js_regexp_funcs)
                .export_to_global(),
        ),
    },
);

pub(crate) const CLASS_DEF_REGEXP_STRING_ITERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_RegExp_String_Iterator,
    ClassInitType::InitFn(RegExp::class_init),
);
