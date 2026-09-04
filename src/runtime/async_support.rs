// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    pin::Pin,
    task::{Context as TaskContext, Poll},
};

use crate::{ExitStatus, JSValue};

use super::{Runtime, RuntimeImpl, handle::RUNTIMES, rt::JoinState, tick::TickStatus};

// ── RunFuture ──────────────────────────────────────────────────────────

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
                std::thread::yield_now();
                cx.waker().wake_by_ref();
                Poll::Pending
            }
            TickStatus::Park(hint) => {
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

// ── join_async ─────────────────────────────────────────────────────────

fn tick_all_to_idle() {
    loop {
        let mut had_progress = false;
        RUNTIMES.with(|rts| {
            let rts = rts.borrow();
            for w in rts.iter() {
                let Some(rt) = w.upgrade() else { continue };
                let rt = unsafe { &mut *rt.get() };
                if matches!(rt.tick(), TickStatus::Progress) {
                    had_progress = true;
                }
            }
        });

        if !had_progress {
            break;
        }
    }
}

const fn exit_status_of(rt: &RuntimeImpl) -> ExitStatus {
    if let Some(code) = rt.interrupt_code {
        ExitStatus::Interrupted(code)
    } else {
        ExitStatus::Value(JSValue::UNDEFINED)
    }
}

// ── JoinNextFuture ─────────────────────────────────────────────────────

/// A stream that yields `(Runtime, ExitStatus)` as each runtime completes.
///
/// Created by [`Runtime::join_each`]. On each `poll_next`, reads the
/// current live runtimes from the thread-local scheduler (picks up
/// runtimes created after the stream was created). Yields one result
/// per completed runtime. Returns `None` when every runtime is idle.
///
/// ```ignore
/// use futures_util::StreamExt;
///
/// let mut stream = Runtime::join_each();
/// while let Some((rt, status)) = stream.next().await {
///     println!("runtime done: {status:?}");
/// }
/// ```
pub struct JoinEachFuture;

impl futures_util::Stream for JoinEachFuture {
    type Item = (Runtime, ExitStatus);

    fn poll_next(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        // Phase 1: tick all live runtimes until no progress.
        loop {
            let mut had_progress = false;

            let parks: Vec<std::sync::Arc<super::ParkState>> = RUNTIMES.with(|rts| {
                let rts = rts.borrow();
                let mut parks = Vec::new();
                for w in rts.iter() {
                    let Some(rc) = w.upgrade() else { continue };
                    let rt = unsafe { &mut *rc.get() };

                    if rt.join_state == JoinState::Yielded {
                        continue;
                    }

                    match rt.tick() {
                        TickStatus::Progress => had_progress = true,
                        TickStatus::Park(_) => parks.push(rt.park.clone()),
                        TickStatus::Idle => rt.join_state = JoinState::Idle,
                    }
                }

                if had_progress {
                    cx.waker().wake_by_ref();
                }
                parks
            });

            if !had_progress {
                if let Some(park) = parks.first() {
                    (**park).park_for(cx.waker(), None);
                }
                break;
            }
        }

        // Phase 2: yield the first Idle runtime.
        let result = RUNTIMES.with(|rts| {
            let rts = rts.borrow();
            for w in rts.iter() {
                let Some(rc) = w.upgrade() else { continue };
                let rt = unsafe { &mut *rc.get() };
                if rt.join_state == JoinState::Idle {
                    rt.join_state = JoinState::Yielded;
                    let es = exit_status_of(rt);
                    return Some((Runtime::from_inner(rc), es));
                }
            }
            None
        });

        if let Some(item) = result {
            return Poll::Ready(Some(item));
        }

        // Check if all live runtimes are yielded.
        let all_done = RUNTIMES.with(|rts| {
            rts.borrow().iter().all(|w| {
                w.upgrade()
                    .is_none_or(|rc| unsafe { (*rc.get()).join_state == JoinState::Yielded })
            })
        });

        if all_done {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

// ── Runtime impl ───────────────────────────────────────────────────────

impl Runtime {
    /// For `self` the single runtime, await running till all background jobs are executed.
    #[inline(always)]
    pub const fn run_async(&mut self) -> RunFuture<'_> {
        RunFuture::new(self)
    }

    /// Drive to run all runtimes on the current thread to idle.
    pub async fn join_async() {
        tick_all_to_idle();
    }

    /// Create a stream that yields `(Runtime, ExitStatus)` as each runtime
    /// completes. Always reads from the current thread-local scheduler.
    /// Resets join state on all live runtimes.
    pub fn join_each() -> JoinEachFuture {
        RUNTIMES.with(|rts| {
            for w in rts.borrow().iter() {
                if let Some(rc) = w.upgrade() {
                    let rt = unsafe { &mut *rc.get() };
                    rt.join_state = JoinState::Active;
                }
            }
        });
        JoinEachFuture
    }
}
