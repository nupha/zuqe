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
            TickStatus::Continue => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            TickStatus::Stop => {
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
