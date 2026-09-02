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
            TickStatus::Progress => {
                // Real work was produced this round — keep draining. We yield
                // the current time slice so sibling tokio tasks (fs/fetch IO)
                // get a chance to push values into their channels, turning a
                // later `tick()` into `Parked`/`Idle`.
                std::thread::yield_now();
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            TickStatus::Park(hint) => {
                // A polling job is waiting on IO or a timer deadline —
                // suspend for real instead of hot-spinning. `park_for` arms
                // the parker thread for the deadline and registers our waker
                // in one critical section; wake sources afterwards are the
                // deadline itself, external producers (`WakeSender` send/drop,
                // slot commit/drop, signals), or the fallback quantum when
                // enabled via `use_quantum_poll`. With quantum off (the
                // default) the loop is purely event driven: every `Again`
                // waiting point must have a wake source.
                this.rt.park.park_for(cx.waker(), hint);
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
