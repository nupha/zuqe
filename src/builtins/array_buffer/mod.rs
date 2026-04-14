// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod abuf;
mod class_arraybuffer;

pub use {abuf::*, class_arraybuffer::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub(crate) const CLASS_DEF_ARRAY_BUFFER: ClassDef = ClassDef::builtin(
    Atom::CONST_ArrayBuffer,
    ClassInitType::Normal {
        proto_entries: js_array_buffer_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_array_buffer_constructor, 1, false)
                .entries(js_array_buffer_funcs)
                .export_to_global(),
        ),
    },
)
.on_finalize(ArrayBuffer::class_finalize);

pub(crate) const CLASS_DEF_SHARED_ARRAY_BUFFER: ClassDef = ClassDef::builtin(
    Atom::CONST_SharedArrayBuffer,
    ClassInitType::Normal {
        proto_entries: js_shared_array_buffer_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(js_shared_array_buffer_constructor, 1, false)
                .entries(js_shared_array_buffer_funcs)
                .export_to_global(),
        ),
    },
)
.on_finalize(ArrayBuffer::class_finalize);
