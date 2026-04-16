// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use super::{Runtime, TickStatus};

pub struct RuntimeFuture<'rt> {
    rt: &'rt mut Runtime,
}

impl<'rt> RuntimeFuture<'rt> {
    pub fn new(rt: &'rt mut Runtime) -> Self {
        Self { rt }
    }
}

impl<'rt> std::future::Future for RuntimeFuture<'rt> {
    type Output = Result<i32, i32>;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();

        match this.rt.tick() {
            Ok(TickStatus::Continue) => {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            Ok(TickStatus::Stop(code)) => Poll::Ready(Ok(code)),
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl Runtime {
    pub fn run_async(&mut self) -> RuntimeFuture<'_> {
        RuntimeFuture::new(self)
    }
}

