// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod handle;
mod park;
mod rt;
mod tick;

pub use {handle::Runtime, park::{DeadlineWaker, WakeHandle}, rt::RuntimeImpl};

pub(crate) use {handle::RuntimePtr, park::ParkState, rt::*};

#[cfg(feature = "async_support")]
mod async_support;
#[cfg(feature = "async_support")]
pub use async_support::RunFuture;
