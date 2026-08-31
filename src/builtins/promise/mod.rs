// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_promise;
mod job;
mod promise;
mod settle;

pub(crate) use job::*;
pub use {class_promise::*, promise::*, settle::*};

#[cfg(feature = "async_support")]
mod async_support;
#[cfg(feature = "async_support")]
pub use async_support::*;

use crate::object::{Promise, PromiseResolveFunc};

crate::def_native_object!(
    builtin,
    "Promise",
    CLASS_DEF_PROMISE,
    class_id: (crate::JS_CLASS_PROMISE, crate::Atom::CONST_Promise),
    crate::builtins::promise::PromisePayload,
    js_promise_proto_funcs,
    Some(
        crate::class::ClassCtorDef::new(js_promise_constructor, 1, false)
            .entries(js_promise_funcs)
            .expose_to_global()
    )
);

crate::def_native_object!(
    builtin,
    "PromiseResolveFunction",
    CLASS_DEF_PROMISE_RESOLVE_FUNCTION,
    class_id: (
        crate::JS_CLASS_PROMISE_RESOLVE_FUNCTION,
        crate::Atom::CONST_PromiseResolveFunction,
    ),
    crate::builtins::promise::PromiseResolvePayload,
    &[],
    None,
    on_call: PromiseResolveFunc::class_call
);

crate::def_native_object!(
    builtin,
    "PromiseRejectFunction",
    CLASS_DEF_PROMISE_REJECT_FUNCTION,
    class_id: (
        crate::JS_CLASS_PROMISE_REJECT_FUNCTION,
        crate::Atom::CONST_PromiseRejectFunction,
    ),
    crate::builtins::promise::PromiseResolvePayload,
    &[],
    None,
    on_call: PromiseResolveFunc::class_call
);
