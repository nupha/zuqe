// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_promise;
mod job;
mod promise;
mod resolving;

pub use {class_promise::*, promise::*, resolving::*};

pub(crate) use job::*;

use crate::{Atom, class::ClassDef};

pub(crate) const CLASS_DEF_PROMISE: ClassDef = ClassDef::builtin(
    Atom::CONST_Promise,
    js_promise_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(Promise::class_constructor, 1, false)
            .entries(js_promise_funcs)
            .global(),
    ),
);

pub(crate) const CLASS_DEF_PROMISE_RESOLVE_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_PromiseResolveFunction, &[], None)
        .on_call(PromiseResolveFunc::class_call);

pub(crate) const CLASS_DEF_PROMISE_REJECT_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_PromiseRejectFunction, &[], None)
        .on_call(PromiseResolveFunc::class_call);
