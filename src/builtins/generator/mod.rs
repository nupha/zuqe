// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod a_gen;
mod r#gen;

pub(crate) use {a_gen::*, r#gen::*};

pub(crate) use crate::object::{AsyncGeneratorFunction, GeneratorFunction};

crate::def_native_object!(
    builtin,
    "Generator",
    CLASS_DEF_GENERATOR,
    class_id: (crate::JS_CLASS_GENERATOR, crate::Atom::CONST_Generator),
    crate::builtins::generator::GeneratorPayload, (), None,
    init_fn: GeneratorFunction::class_init
);

crate::def_native_object!(
    builtin,
    "GeneratorFunction",
    CLASS_DEF_GENERATOR_FUNCTION,
    class_id: (crate::JS_CLASS_GENERATOR_FUNCTION, crate::Atom::CONST_Function),
    crate::function::BFuncPayload, (), None,
    init_fn: GeneratorFunction::class_init,
    on_call: js_generator_function_call
);

crate::def_native_object!(
    builtin,
    "AsyncGenerator",
    CLASS_DEF_ASYNC_GENERATOR,
    class_id: (crate::JS_CLASS_ASYNC_GENERATOR, crate::Atom::CONST_AsyncGenerator),
    crate::builtins::generator::AGenPayload, (), None,
    init_fn: AsyncGeneratorFunction::class_init
);

crate::def_native_object!(
    builtin,
    "AsyncGeneratorFunction",
    CLASS_DEF_ASYNC_GENERATOR_FUNCTION,
    class_id: (crate::JS_CLASS_ASYNC_GENERATOR_FUNCTION, crate::Atom::CONST_AsyncGeneratorFunction),
    crate::function::BFuncPayload, (), None,
    init_fn: AsyncGeneratorFunction::class_init,
    on_call: AsyncGeneratorFunction::class_call
);
