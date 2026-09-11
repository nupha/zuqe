// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_list;
mod list_payload;

pub use class_list::*;
pub use list_payload::*;

use crate::{
    Atom,
    class::{ClassCtorDef, ClassDef},
};

pub(crate) static CLASS_DEF_LIST: ClassDef = ClassDef::builtin(
    Atom::CONST_List,
    js_list_proto_funcs,
    Some(
        ClassCtorDef::new(js_list_constructor, 1, true)
            .entries(js_list_funcs)
            .global(),
    ),
);
