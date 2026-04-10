// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod func;
mod proto;

pub use {func::*, proto::*};

pub(crate) const CLASS_DEF_OBJECT: crate::ClassDef = crate::ClassDef {
    name: "",
    name_atom: crate::Atom::CONST_Object,
    proto_entries: js_object_proto_funcs,
    ctor_entries: js_object_funcs,
    call: None,
    exotic: None,
    initialize: None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
