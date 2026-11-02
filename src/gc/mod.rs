// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

pub mod audit;
pub(crate) mod gc;
pub(crate) mod guard;

pub(crate) use gc::GcHeapExt;
pub use guard::{GcBind, WriteBarrier, WriteBarrierVec};
