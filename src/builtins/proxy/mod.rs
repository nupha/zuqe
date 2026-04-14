// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_proxy;
mod proxy;

pub use {class_proxy::*, proxy::*};

use crate::{
    Atom,
    class::{ClassDef},
};

pub(crate) const CLASS_DEF_PROXY: ClassDef =
    ClassDef::builtin_with_init_func(Atom::CONST_Proxy, Proxy::class_init)
        .exotic(&js_proxy_exotic_methods)
        .on_call(Proxy::class_call);
