// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_gen;
mod r#gen;

pub(crate) use {a_gen::*, r#gen::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub const CLASS_DEF_GENERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Generator,
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(GeneratorFunction::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_GENERATOR_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Function,
    call: Some(js_generator_function_call),
    exotic: None,
    initialize: ClassInitType::InitFn(GeneratorFunction::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_ASYNC_GENERATOR: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_AsyncGenerator,
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
    call: Some(AsyncGeneratorFunction::class_call),
    exotic: None,
    initialize: ClassInitType::InitFn(AsyncGeneratorFunction::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
