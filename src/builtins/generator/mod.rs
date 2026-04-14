// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_gen;
mod r#gen;

pub(crate) use {a_gen::*, r#gen::*};

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub(crate) const CLASS_DEF_GENERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_Generator,
    ClassInitType::InitFn(GeneratorFunction::class_init),
);

pub(crate) const CLASS_DEF_GENERATOR_FUNCTION: ClassDef = ClassDef::builtin(
    Atom::CONST_Function,
    ClassInitType::InitFn(GeneratorFunction::class_init),
)
.on_call(js_generator_function_call);

pub(crate) const CLASS_DEF_ASYNC_GENERATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_AsyncGenerator,
    ClassInitType::InitFn(AsyncGeneratorFunction::class_init),
);

pub(crate) const CLASS_DEF_ASYNC_GENERATOR_FUNCTION: ClassDef = ClassDef::builtin(
    Atom::CONST_AsyncGeneratorFunction,
    ClassInitType::InitFn(AsyncGeneratorFunction::class_init),
)
.on_call(AsyncGeneratorFunction::class_call);
