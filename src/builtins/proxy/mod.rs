// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod class_proxy;
mod proxy;

pub use {class_proxy::*, proxy::*};

use crate::object::Proxy;

crate::def_native_object!(
    builtin,
    "Proxy",
    CLASS_DEF_PROXY,
    class_id: (crate::JS_CLASS_PROXY, crate::Atom::CONST_Proxy),
    crate::builtins::proxy::ProxyPayload, (), None,
    init_fn: Proxy::class_init,
    exotic: &js_proxy_exotic_methods,
    on_call: Proxy::class_call
);
