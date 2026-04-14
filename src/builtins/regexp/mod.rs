// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_regexp;
mod class_regexp_iter;
mod re;

pub use {class_regexp::*, class_regexp_iter::*, re::*};

use crate::{
    Atom,
    class::{ClassCtorDef, ClassDef},
};

pub(crate) static CLASS_DEF_REGEXP: ClassDef = ClassDef::builtin(
    Atom::CONST_RegExp,
    js_regexp_proto_funcs,
    Some(
        ClassCtorDef::new(js_regexp_constructor, 2, true)
            .entries(js_regexp_funcs)
            .global(),
    ),
);

pub(crate) static CLASS_DEF_REGEXP_STRING_ITERATOR: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_RegExp_String_Iterator, RegExp::class_init);
