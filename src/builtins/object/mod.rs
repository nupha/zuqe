// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod func;
mod proto;

pub use {func::*, proto::*};

pub(crate) const CLASS_DEF_OBJECT: crate::ClassDef = crate::ClassDef::builtin(
    crate::Atom::CONST_Object,
    crate::class::ClassInitType::Normal {
        proto_entries: js_object_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(func::js_object_constructor, 1, true)
                .entries(js_object_funcs)
                .export_to_global(),
        ),
    },
);
