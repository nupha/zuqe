// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Instant,
};

use crate::{ExitStatus, JSValue};

use super::{Runtime, park::PARK_QUANTUM, tick::TickStatus};

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
            TickStatus::Progress => {
                // Real work was produced this round — keep draining. We yield
                // the current time slice so sibling tokio tasks (fs/fetch IO)
                // get a chance to push values into their channels, turning a
                // later `tick()` into `Parked`/`Idle`.
                std::thread::yield_now();
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            TickStatus::Parked(hint) => {
                // A polling job is waiting on IO or a timer deadline —
                // suspend for real instead of hot-spinning. Two wake sources:
                //   * the parker thread wakes us at `min(hint, quantum)`
                //   * external producers (`WakeHandle::wake` after a channel
                //     send / slot commit) wake us via the registered waker
                // The quantum cap is a safety net: a producer that misses its
                // `wake()` call degrades to a ~5 ms poll instead of a lost
                // wakeup (the loop sleeping forever). Once genuinely idle
                // (no Progress, no Parked) `tick()` returns `Idle` and we exit.
                let park = this.rt.park.clone();
                let now = Instant::now();
                let deadline = hint.map_or(now + PARK_QUANTUM, |d| d.min(now + PARK_QUANTUM));
                park.park_async(deadline);
                if park.register(cx.waker()) {
                    // A wake raced with registration — re-tick immediately.
                    cx.waker().wake_by_ref();
                }
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
