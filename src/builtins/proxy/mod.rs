// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_proxy;
mod proxy;

pub use {class_proxy::*, proxy::*};

use crate::class::ClassInitType;
use crate::{Atom, ClassDef};

pub const CLASS_DEF_PROXY: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Proxy,
    proto_entries: &[],
    ctor_entries: js_proxy_funcs,
    call: Some(Proxy::class_call),
    exotic: Some(&js_proxy_exotic_methods),
    initialize: ClassInitType::InitFn(Proxy::class_init),
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
