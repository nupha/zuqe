// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Runtime coordination for the JIT (epic D).
//!
//! - [`hotness`] decides when a function is worth compiling.
//! - [`coordinator`] owns the request queue and each function's lifecycle.
//! - [`install`] publishes finished code into executable memory and the slot.
//! - [`background`] runs the compile off-thread (feature `jit_background`).
//!
//! Without `jit_background`, compilation runs inline in `JitState::poll`, which
//! the host already calls outside `with_new_scope`.

mod coordinator;
mod hotness;
mod install;

#[cfg(feature = "jit_background")]
mod background;

pub(crate) use {
    coordinator::{CompileOutcome, Coordinator, Request},
    install::publish,
};

#[cfg(feature = "jit_background")]
pub(crate) use background::{BackgroundCompiler, CompileCompletion, JobResult};
