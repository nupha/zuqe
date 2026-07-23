// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod ops;
mod primitives;
mod repr;
mod val;

pub(crate) use primitives::*;

#[cfg(feature = "nan_boxing")]
mod nb_value;
#[cfg(feature = "nan_boxing")]
mod tag;
#[cfg(feature = "nan_boxing")]
pub use nb_value::JSValue;
#[cfg(feature = "nan_boxing")]
pub(crate) use tag::*;

#[cfg(not(feature = "nan_boxing"))]
mod nnb_value;
#[cfg(not(feature = "nan_boxing"))]
pub use nnb_value::JSValue;

pub(crate) const HINT_STRING: i32 = 0;
pub(crate) const HINT_NUMBER: i32 = 1;
pub(crate) const HINT_NONE: i32 = 2;
pub(crate) const HINT_FORCE_ORDINARY: i32 = 0x01 << 4;
