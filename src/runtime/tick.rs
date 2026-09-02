// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::time::Instant;

use gc_lite::GcNode;

use crate::{
    Exception, WakeReceiver, WakeSender,
    context::{ContextPtr, ScopedContext},
    microtask::JobState,
};

use super::RuntimeImpl;

impl<'c> ScopedContext<'c> {
    #[inline(always)]
    pub fn add_tick_handler(&mut self, f: fn(&mut ScopedContext) -> bool) {
        self.tick_handlers.push(f);
    }

    /// Clonable handle to this context's runtime event loop. Capture it on
    /// the JS thread and move it into producer tasks/threads (channel senders
    /// etc.); calling `wake()` after pushing work re-ticks the parked loop.
    /// Per-runtime — holding handles of many runtimes at once is fine.
    #[inline(always)]
    pub fn wake_handle(&self) -> crate::runtime::WakeHandle {
        self.rt().wake_handle()
    }

    /// Create a bounded channel whose sender wakes this event loop
    /// automatically on every send and on drop. The `n` parameter is the
    /// channel capacity. Pair it with a channel poller job so a `Parked`
    /// round is woken by the producer.
    pub fn make_channel<D: Send + 'static>(
        &self,
        n: usize,
    ) -> (WakeSender<D>, crate::WakeReceiver<D>) {
        let (tx, rx) = crate::channel::make_bounded_channel_raw(n);
        (
            WakeSender::from(tx, self.wake_handle()),
            WakeReceiver::from(rx),
        )
    }

    /// Create an unbounded channel whose sender wakes this event loop
    /// automatically on every send and on drop (all senders gone). Pair it
    /// with a channel poller job so a `Parked` round is woken by the
    /// producer — the infrastructure guarantee behind "every `Again` wait
    /// has a wake source"; producers cannot forget.
    ///
    /// `D` need not be `Send`: JS-thread-only channels (e.g. carrying
    /// `Object` handles) are supported; `send_async` (for cross-thread
    /// producers) does require `Send`.
    pub fn make_local_channel<D: 'static>(&self) -> (WakeSender<D>, crate::WakeReceiver<D>) {
        let (tx, rx) = crate::channel::make_local_unbounded_channel_raw();
        (
            WakeSender::from(tx, self.wake_handle()),
            WakeReceiver::from(rx),
        )
    }

    /// Advise the event-loop driver of the earliest time a polling job needs
    /// to run again (e.g. a timer's next deadline). A `Parked` round then
    /// suspends until this instant (or an external wake) instead of spinning.
    /// Accumulates the minimum across jobs of one tick; consumed by `tick()`.
    pub fn set_poll_hint(&mut self, at: Instant) {
        let prev = self.poll_hint.replace(Some(at));
        if let Some(d) = prev
            && d < at
        {
            self.poll_hint.set(Some(d));
        }
    }
}

#[derive(PartialEq)]
pub(super) enum TickStatus {
    /// This round made real progress: a job completed (Done/Err), a tick
    /// handler reported new work, or a oneshot job is still queued. The
    /// caller should call `tick()` again immediately to advance.
    Progress,
    /// No real progress this round, but a polling job is still spinning on
    /// `Block` (waiting on external IO or a timer deadline). Those jobs are
    /// re-queued (never lost) but produced no new work. The `Option<Instant>`
    /// is the earliest deadline any such job reported via
    /// `set_poll_hint` (`None` = wait for external wake only). The caller
    /// decides: the sync loop suspends on a condvar, the async path parks via
    /// the parker thread (Pending), and the agent `drain_jobs` terminates —
    /// so a pure-`Block` wait can never dead-loop or hot-spin.
    Park(Option<Instant>),
    /// The event loop is genuinely idle (no Progress, no Parked) or was
    /// interrupted. Every caller terminates its drive loop.
    Idle,
}

impl RuntimeImpl {
    /// Run background jobs for one round.
    pub(super) fn tick(&mut self) -> TickStatus {
        let mut had_progress = false;
        let mut had_blocked = false;
        let mut hint: Option<Instant> = None;

        for mut ctx in self.context_list.iter().copied() {
            ctx.with_new_scope(|cx| {
                // 1. Run all one-shot jobs first (always complete, never retry).
                //    New oneshot jobs may be queued during execution, so keep
                //    draining until the queue is empty.
                if !cx.oneshot_jobs.is_empty() {
                    while let Some(mut job) = cx.oneshot_jobs.pop_front() {
                        self.gc_job = job.gc_trace.take(); // hold job gc tracer so GC can trace it during run()
                        let res = job.run(cx); // run the job
                        drop(self.gc_job.take()); // release gc tracer

                        if res.is_err() {
                            log::warn!("[task error] {:?}", self.cur_exception);
                            self.cur_exception = Exception::No;
                            continue;
                        } else if self.interrupt_code.is_some() {
                            // Interrupt detected — stop now
                            break;
                        }
                    }
                }

                if self.interrupt_code.is_none() {
                    // 2. Run polling jobs (may return `Again`) — one round only.
                    //    Skip if interrupt is pending.
                    let num = cx.polling_jobs.len();
                    for _ in 0..num {
                        let Some(mut job) = cx.polling_jobs.pop_front() else {
                            break;
                        };

                        // put job trace to temp holder and run the job
                        self.gc_job = job.gc_trace.take();
                        let res = job.run(cx);
                        let gc_trace = self.gc_job.take();

                        match res {
                            Ok(JobState::Block) => {
                                // Re-queue for the next round. A pending-channel
                                // promise (fetch/fs/...) is waiting on IO; it only
                                // resolves once the IO thread pushes a value into the
                                // channel. We do NOT set `may_have_new_job` here — this
                                // round made no real progress, so the sync `eval_wait`
                                // loop must not busy-exit and the agent `drain_jobs`
                                // must be able to terminate. The job is safely
                                // re-queued and will be polled again next round (or by
                                // the next `tick()` caller), so it is never lost.
                                had_blocked = true;
                                job.gc_trace = gc_trace;
                                cx.polling_jobs.push_back(job);
                            }
                            Ok(JobState::Continue) => {
                                // Same re-queue safety as `Again`, but does NOT count
                                // as `Parked`: an unobserved (fire-and-forget) promise
                                // must not keep the event loop — and thus the process —
                                // alive. The job is never lost, so it keeps polling on
                                // any later round driven by other work.
                                job.gc_trace = gc_trace;
                                cx.polling_jobs.push_back(job);
                            }
                            Ok(JobState::Done) => {
                                had_progress = true;
                            }
                            Err(_) => {
                                log::warn!("[task error] {:?}", self.cur_exception);
                                self.cur_exception = Exception::No;
                                // Don't re-queue — treat as Done.
                                had_progress = true;
                            }
                        }

                        // Interrupt detected — stop now
                        if self.interrupt_code.is_some() {
                            break;
                        }
                    }

                    if self.interrupt_code.is_none() {
                        // 3. customized tick handlers
                        let ctx_ptr = ContextPtr::from(cx.gc_ref());
                        for f in ctx_ptr.tick_handlers.iter() {
                            if f(cx) {
                                had_progress = true;
                            }

                            // Interrupt detected — stop now
                            if self.interrupt_code.is_some() {
                                break;
                            }
                        }

                        // Oneshot jobs may also be queued by polling handlers; keep ticking if any remain.
                        if !cx.oneshot_jobs.is_empty() {
                            had_progress = true;
                        }
                    }

                    // Collect the earliest deadline any polling job reported
                    // this round (jobs ran above; take clears the cell).
                    if let Some(d) = cx.poll_hint.take() {
                        hint = Some(match hint {
                            Some(h) => h.min(d),
                            None => d,
                        });
                    }
                }
            });

            // Skip remaining contexts if interrupted
            if self.interrupt_code.is_some() {
                break;
            }
        }

        // ES §9.5: any exception left behind by a job is a bug in debug builds,
        // and must be isolated (cleared) in release builds.
        debug_assert_eq!(
            self.cur_exception,
            Exception::No,
            "[task bug] a job or tick handler left exception behind: {:?}",
            self.cur_exception
        );
        if !matches!(self.cur_exception, Exception::No) {
            log::warn!("[task leaked an exception] {:?}", self.cur_exception);
            self.cur_exception = Exception::No;
        }

        // Interrupt check — independent from exception.
        if self.interrupt_code.is_some() {
            return TickStatus::Idle;
        }

        if self.gc_req || self.gc_heap().should_gc() {
            self.gc_collect();
        }

        // Process FinalizationRegistry cleanups collected during GC
        #[cfg(feature = "builtin_weakref")]
        self.process_finrec_cleanups();

        // Progress takes priority: if any real work was produced this round,
        // advance immediately (a co-existing `Again` job is already re-queued
        // and will be polled next round). Only when there is no real progress
        // but a polling job is still pending do we report `Parked`, carrying
        // the earliest reported re-poll deadline so the driver can suspend.
        if had_progress {
            TickStatus::Progress
        } else if had_blocked {
            TickStatus::Park(hint)
        } else {
            TickStatus::Idle
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TickStatus;

    use crate::{
        JSValue, ScopedContext,
        builtins::promise::{Promise, PromiseResultSlot, PromiseState},
        runtime::Runtime,
    };

    /// Drive the event loop until it stops (no more jobs to run).
    fn drain(rt: &mut Runtime) {
        while matches!(rt.tick(), TickStatus::Progress) {}
    }

    /// 回归：`commit` 后立刻 drop 全部外部强引用、job 尚未轮询时，已提交的值必须仍可取到。
    ///
    /// job 持有强 `Arc`，轮询用 `strong_count == 1` 判断提交者消失；
    /// 数据保活与提交者存活检测不再耦合在同一个 `Weak` 上。
    #[test]
    fn result_slot_commit_then_drop_is_not_lost() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        let promise = ctx.with_root_scope(|mut cx| {
            let slot = PromiseResultSlot::new();
            let p = Promise::create_with_result_slot(&mut cx, slot.clone(), |_, v: i32| {
                Ok(JSValue::from(v))
            })?;
            // 复现竞态：commit 后立刻 drop，job 尚未开始轮询。
            slot.commit(42).unwrap();
            drop(slot);
            Ok::<Promise, i32>(p)
        })?;

        drain(&mut rt);

        assert_eq!(promise.state(), PromiseState::Fulfilled);
        assert_eq!(promise.result(), JSValue::from(42));
        Ok(())
    }

    /// 回归：全部提交者 drop 且未 commit 时，job 确定性 settle 而非永久挂住。
    ///
    /// debug 构建走 `debug_assert` 立刻炸（开发期安全网）；release 构建以 internal error 拒绝。
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "dropped without commit")]
    fn result_slot_drop_without_commit_detected() {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt
            .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
            .unwrap();

        let _ = ctx.with_root_scope(|mut cx| {
            let slot = PromiseResultSlot::new();
            let p = Promise::create_with_result_slot(&mut cx, slot.clone(), |_, v: i32| {
                Ok(JSValue::from(v))
            })
            .unwrap();
            drop(slot);
            p
        });

        drain(&mut rt);
    }

    /// release 构建：不 commit 直接 drop → promise 以 internal error 拒绝，事件循环正常退出。
    #[cfg(not(debug_assertions))]
    #[test]
    fn result_slot_drop_without_commit_rejects() -> Result<(), i32> {
        let mut rt = Runtime::create()?;
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

        let promise = ctx.with_root_scope(|mut cx| {
            let slot = PromiseResultSlot::new();
            let p = Promise::create_with_result_slot(&mut cx, slot.clone(), |_, v: i32| {
                Ok(JSValue::from(v))
            })?;
            drop(slot);
            Ok::<Promise, i32>(p)
        })?;

        drain(&mut rt);

        assert_eq!(promise.state(), PromiseState::Rejected);
        Ok(())
    }
}
