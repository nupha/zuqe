// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod func;
mod proto;

pub use {func::*, proto::*};

crate::def_native_object!(
    builtin,
    "Object",
    CLASS_DEF_OBJECT,
    class_id: (crate::JS_CLASS_OBJECT, crate::Atom::CONST_Object),
    (),
    js_object_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(func::js_object_constructor, 1, true)
            .entries(js_object_funcs)
            .expose_to_global(),
    )
);
