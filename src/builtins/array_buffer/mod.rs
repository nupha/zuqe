// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod abuf;
mod class_arraybuffer;

pub use {abuf::*, class_arraybuffer::*};

use crate::{Atom, ClassDef};

pub const CLASS_DEF_ARRAY_BUFER: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_ArrayBuffer,
    proto_entries: js_array_buffer_proto_funcs,
    ctor_entries: js_array_buffer_funcs,
    call: None,
    exotic: None,
    initialize: Some(ArrayBuffer::class_init),
    finalize: Some(ArrayBuffer::class_finalize),
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_SHARED_ARRAY_BUFER: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_SharedArrayBuffer,
    proto_entries: js_shared_array_buffer_proto_funcs,
    ctor_entries: js_shared_array_buffer_funcs,
    call: None,
    exotic: None,
    initialize: Some(ArrayBuffer::class_init),
    finalize: Some(ArrayBuffer::class_finalize),
    finalize_opaque: None,
    unregister: None,
};
