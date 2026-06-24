// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod core;
mod ctx;
mod global;
mod interrupt;
mod intrinsic;
mod scope;

pub use {
    ctx::{Context, ContextImpl},
    scope::{ScopeResult, ScopedContext},
};

pub(crate) use ctx::{ContextPtr, ProtoCtorPair};
