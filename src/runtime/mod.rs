// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#[cfg(feature = "async_support")]
mod async_support;
#[cfg(all(test, feature = "child_runtime"))]
mod child_lifetime;
mod handle;
mod park;
mod rt;
mod tick;

pub use {
    handle::Runtime,
    park::{DeadlineWaker, WakeHandle},
    rt::{RuntimeImpl, RuntimeUserState},
    tick::TickStatus,
};

pub(crate) use {handle::RuntimePtr, park::ParkState, rt::*};

#[cfg(feature = "async_support")]
pub use async_support::{JoinEachFuture, RunFuture};

#[cfg(not(feature = "async_support"))]
pub(super) use tick::tick_tree;
