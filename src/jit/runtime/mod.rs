// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Runtime coordination for the JIT.
//!
//! - [`hotness`] decides when a function is worth compiling.
//! - [`coordinator`] owns the request queue and each function's lifecycle.
//! - [`install`] publishes finished code into executable memory and the slot.
//! - [`background`] runs verification and lowering on worker threads.
//!
//! The runtime thread stays the single writer of every shared slot and the
//! code cache: it dispatches jobs, drains completions and installs the result.

mod background;
mod coordinator;
mod hotness;
mod install;

pub(crate) use {
    background::{BackgroundCompiler, CompileCompletion, JobResult},
    coordinator::{CompileOutcome, Coordinator, Request, Tiering},
    hotness::Tier,
    install::{Published, publish},
};

// Only the runtime-thread tests assert against the prune threshold.
#[cfg(test)]
pub(crate) use coordinator::MIN_RECORDS;
