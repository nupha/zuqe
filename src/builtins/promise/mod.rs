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

pub const CLASS_DEF_PROMISE: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Promise,
    call: None,
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: js_promise_proto_funcs,
        ctor: Some(
            crate::class::ClassCtorDef::new(Promise::class_constructor, 1, false)
                .entries(js_promise_funcs)
                .export_to_global(),
        ),
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_PROMISE_RESOLVE_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_PromiseResolveFunction,
    call: Some(PromiseResolveFunc::class_call),
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_PROMISE_REJECT_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_PromiseRejectFunction,
    call: Some(PromiseResolveFunc::class_call),
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
