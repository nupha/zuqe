// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_promise;
mod job;
mod promise;
mod resolving;

pub use {class_promise::*, promise::*, resolving::*};

pub(crate) use job::*;

use crate::{
    Atom,
    class::{ClassDef, ClassInitType},
};

pub(crate) const CLASS_DEF_PROMISE: ClassDef = ClassDef::builtin(
    Atom::CONST_Promise,
    ClassInitType::Normal {
        proto_entries: js_promise_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(Promise::class_constructor, 1, false)
                .entries(js_promise_funcs)
                .export_to_global(),
        ),
    },
);

pub(crate) const CLASS_DEF_PROMISE_RESOLVE_FUNCTION: ClassDef = ClassDef::builtin(
    Atom::CONST_PromiseResolveFunction,
    ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
)
.on_call(PromiseResolveFunc::class_call);

pub(crate) const CLASS_DEF_PROMISE_REJECT_FUNCTION: ClassDef = ClassDef::builtin(
    Atom::CONST_PromiseRejectFunction,
    ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
)
.on_call(PromiseResolveFunc::class_call);
