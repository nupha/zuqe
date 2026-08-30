// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod handle;
mod park;
mod rt;
mod tick;

pub use {handle::Runtime, park::WakeHandle, rt::RuntimeImpl};

pub(crate) use {handle::RuntimePtr, park::ParkState, rt::*};

// Consumed only by the sync `eval_wait` (eval.rs); the async driver reaches
// the const through the sibling module path directly.
#[cfg(not(feature = "async_support"))]
pub(crate) use park::PARK_QUANTUM;

#[cfg(feature = "async_support")]
mod async_support;
#[cfg(feature = "async_support")]
pub use async_support::RunFuture;
