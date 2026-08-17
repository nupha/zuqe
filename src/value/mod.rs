// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod ops;
mod primitives;
mod repr;
mod tag;
mod val;

pub use val::JSValue;

pub(crate) use tag::JSValueTag;

pub(crate) const HINT_STRING: i32 = 0;
pub(crate) const HINT_NUMBER: i32 = 1;
pub(crate) const HINT_NONE: i32 = 2;
pub(crate) const HINT_FORCE_ORDINARY: i32 = 0x01 << 4;
