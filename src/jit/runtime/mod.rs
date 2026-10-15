// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Runtime coordination for the JIT (epic D).
//!
//! - [`hotness`] decides when a function is worth compiling.
//! - [`coordinator`] owns the request queue and each function's lifecycle.
//! - [`install`] publishes finished code into executable memory and the slot.
//!
//! The background worker (D3) is not built yet: compilation runs inline in
//! `JitState::poll`, which the host already calls outside `with_new_scope`.

mod coordinator;
mod hotness;
mod install;

pub(crate) use coordinator::{CompileOutcome, Coordinator, Request};
pub(crate) use install::publish;
