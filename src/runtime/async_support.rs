// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use crate::{ExitStatus, JSValue};

use super::{Runtime, tick::TickStatus};

pub struct RunFuture<'rt> {
    rt: &'rt mut Runtime,
}

impl<'rt> RunFuture<'rt> {
    pub const fn new(rt: &'rt mut Runtime) -> Self {
        Self { rt }
    }
}

impl<'rt> std::future::Future for RunFuture<'rt> {
    type Output = ExitStatus;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this.rt.tick() {
            TickStatus::Progress | TickStatus::Parked => {
                // Progress: real work was produced this round — keep draining.
                // Parked: a polling job is still waiting on IO (re-queued, not
                // lost) but produced no progress. Unlike `drain_jobs` (which runs
                // on a bare OS thread with no waker mechanism and must terminate
                // on `Parked`), `RunFuture` lives under a tokio executor: we
                // re-arm our waker and return `Pending` so the executor will
                // poll us again. We also `yield_now()` to give other tokio tasks
                // (the fs/fetch IO tasks) a chance to push a value into the
                // channel, turning the next `tick()` into `Progress` or `Idle`.
                // This never dead-loops: once the event loop is genuinely idle
                // (no Progress, no Parked) `tick()` returns `Idle` and we exit.
                std::thread::yield_now();
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            TickStatus::Idle => {
                debug_assert!(matches!(this.rt.exception(), crate::Exception::No));

                Poll::Ready(if let Some(code) = this.rt.take_interrupt() {
                    ExitStatus::Interrupted(code)
                } else {
                    ExitStatus::Value(JSValue::UNDEFINED)
                })
            }
        }
    }
}

impl Runtime {
    /// Await till all background jobs are executed
    pub const fn run_async(&mut self) -> RunFuture<'_> {
        RunFuture::new(self)
    }
}
