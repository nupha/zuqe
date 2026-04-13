// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod agen;
mod r#gen;

pub use {agen::*, r#gen::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub const CLASS_DEF_GENERATOR_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Function,
    proto_entries: js_generator_function_proto_funcs,
    ctor_entries: &[],
    call: Some(js_generator_function_call),
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_generator_function_proto_funcs,
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_GENERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Generator,
    proto_entries: js_generator_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::Entries {
        proto_entries: js_generator_proto_funcs,
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_ASYNC_GENERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_AsyncGenerator,
    proto_entries: js_async_generator_proto_funcs,
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(AsyncGeneratorFunction::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_ASYNC_GENERATOR_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_AsyncGeneratorFunction,
    proto_entries: js_async_generator_function_proto_funcs,
    ctor_entries: &[],
    call: Some(AsyncGeneratorFunction::class_call),
    exotic: None,
    initialize: ClassInitType::InitFn(AsyncGeneratorFunction::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
