// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod abuf;
mod class_arraybuffer;

pub use {abuf::*, class_arraybuffer::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub const CLASS_DEF_ARRAY_BUFFER: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_ArrayBuffer,
    proto_entries: js_array_buffer_proto_funcs,
    ctor_entries: js_array_buffer_funcs,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_array_buffer_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::Constructor(js_array_buffer_constructor),
            length: 1,
            magic: 0,
            entries: js_array_buffer_funcs,
            global_name: Some(Atom::CONST_ArrayBuffer),
        }),
    },
    finalize: Some(ArrayBuffer::class_finalize),
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_SHARED_ARRAY_BUFFER: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_SharedArrayBuffer,
    proto_entries: js_shared_array_buffer_proto_funcs,
    ctor_entries: js_shared_array_buffer_funcs,
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_shared_array_buffer_proto_funcs,
        ctor: Some(crate::class::ClassCtorDef {
            callback: crate::Callback::Constructor(js_shared_array_buffer_constructor),
            length: 1,
            magic: 0,
            entries: js_shared_array_buffer_funcs,
            global_name: Some(Atom::CONST_SharedArrayBuffer),
        }),
    },
    finalize: Some(ArrayBuffer::class_finalize),
    finalize_opaque: None,
    unregister: None,
};
