// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Errors from attaching the JIT to a runtime.

use std::fmt;

/// Why `Jit::attach` could not install a JIT on a runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitError {
    /// The runtime already has a JIT attached.
    AlreadyAttached,
    /// The host CPU/OS has no Cranelift backend in this build.
    UnsupportedTarget(&'static str),
    /// Cranelift rejected the target configuration.
    Cranelift(String),
    /// The host platform refused an operating-system call (mmap, mprotect, …).
    Platform(String),
}

impl fmt::Display for JitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyAttached => write!(f, "runtime already has a JIT attached"),
            Self::UnsupportedTarget(e) => write!(f, "no Cranelift backend for this target: {e}"),
            Self::Cranelift(e) => write!(f, "cranelift: {e}"),
            Self::Platform(e) => write!(f, "platform: {e}"),
        }
    }
}

impl std::error::Error for JitError {}
