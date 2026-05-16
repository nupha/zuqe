// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use crate::Exception;

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
    type Output = Result<(), Exception>;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this.rt.tick() {
            Ok(TickStatus::Continue) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Ok(TickStatus::Stop) => Poll::Ready(Ok(())),
            Err(_) => Poll::Ready(Err(this.rt.take_exception())),
        }
    }
}

impl Runtime {
    /// Await till all background jobs are executed
    pub const fn run_async(&mut self) -> RunFuture<'_> {
        RunFuture::new(self)
    }
}
