// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod ctx;
mod global;
mod interrupt;
mod intrinsic;
mod scope;

pub use {
    ctx::{Context, ContextImpl},
    scope::ContextScope,
};

pub(crate) use ctx::ContextPtr;
