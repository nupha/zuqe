// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_promise;
mod microtask;
mod promise;
mod settle;

pub use {class_promise::*, microtask::*, promise::*, settle::*};

#[cfg(feature = "async_support")]
mod async_support;
#[cfg(feature = "async_support")]
pub use async_support::*;

use crate::{Atom, class::ClassDef};

pub(crate) static CLASS_DEF_PROMISE: ClassDef = ClassDef::builtin(
    Atom::CONST_Promise,
    js_promise_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_promise_constructor, 1, false)
            .entries(js_promise_funcs)
            .expose_to_global(),
    ),
);

pub(crate) static CLASS_DEF_PROMISE_RESOLVE_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_PromiseResolveFunction, &[], None)
        .on_call(PromiseResolveFunc::class_call);

pub(crate) static CLASS_DEF_PROMISE_REJECT_FUNCTION: ClassDef =
    ClassDef::builtin(Atom::CONST_PromiseRejectFunction, &[], None)
        .on_call(PromiseResolveFunc::class_call);
