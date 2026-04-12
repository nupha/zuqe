// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_promise;
mod job;
mod promise;
mod resolving;

pub use {class_promise::*, promise::*, resolving::*};

pub(crate) use job::*;

use crate::class::ClassInitType;
use crate::{Atom, ClassDef};

pub const CLASS_DEF_PROMISE: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Promise,
    proto_entries: js_promise_proto_funcs,
    ctor_entries: js_promise_funcs,
    call: None,
    exotic: None,
    initialize: ClassInitType::InitFn(Promise::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_PROMISE_RESOLVE_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_PromiseResolveFunction,
    proto_entries: &[],
    ctor_entries: &[],
    call: Some(PromiseResolveFunc::class_call),
    exotic: None,
    initialize: ClassInitType::None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_PROMISE_REJECT_FUNCTION: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_PromiseRejectFunction,
    proto_entries: &[],
    ctor_entries: &[],
    call: Some(PromiseResolveFunc::class_call),
    exotic: None,
    initialize: ClassInitType::None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
