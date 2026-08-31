// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod abuf;
mod class_arraybuffer;

pub use {abuf::*, class_arraybuffer::*};

pub use crate::object::ArrayBuffer;

crate::def_native_object!(
    builtin,
    "ArrayBuffer",
    CLASS_DEF_ARRAY_BUFFER,
    class_id: (crate::JS_CLASS_ARRAY_BUFFER, crate::Atom::CONST_ArrayBuffer),
    crate::builtins::array_buffer::ArrayBufferPayload,
    js_array_buffer_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_array_buffer_constructor, 1, false)
            .entries(js_array_buffer_funcs)
            .expose_to_global(),
    ),
    on_finalize: ArrayBuffer::class_finalize
);

crate::def_native_object!(
    builtin,
    "SharedArrayBuffer",
    CLASS_DEF_SHARED_ARRAY_BUFFER,
    class_id: (crate::JS_CLASS_SHARED_ARRAY_BUFFER, crate::Atom::CONST_SharedArrayBuffer),
    crate::builtins::array_buffer::ArrayBufferPayload,
    js_shared_array_buffer_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_shared_array_buffer_constructor, 1, false)
            .entries(js_shared_array_buffer_funcs)
            .expose_to_global(),
    ),
    on_finalize: ArrayBuffer::class_finalize
);
