// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;

use crate::{
    Exception,
    context::{ContextPtr, ScopedContext},
    microtask::JobState,
};

use super::RuntimeImpl;

impl<'c> ScopedContext<'c> {
    pub fn add_tick_handler(&mut self, f: fn(&mut ScopedContext) -> bool) {
        self.tick_handlers.push(f);
    }
}

pub(super) enum TickStatus {
    /// More jobs to process — call tick() again.
    Continue,
    /// Event loop stopped normally (or interrupted — check interrupt_code).
    Stop,
}

impl RuntimeImpl {
    /// Run background jobs for one round.
    pub(super) fn tick(&mut self) -> TickStatus {
        let mut more_ticks = false;

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
                            Ok(JobState::Again) => {
                                job.gc_trace = gc_trace;
                                cx.polling_jobs.push_back(job);
                                more_ticks = true;
                            }
                            Ok(JobState::Done) => {}
                            Err(_) => {
                                log::warn!("[task error] {:?}", self.cur_exception);
                                self.cur_exception = Exception::No;
                                // Don't re-queue — treat as Done.
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
                                more_ticks = true;
                            }

                            // Interrupt detected — stop now
                            if self.interrupt_code.is_some() {
                                break;
                            }
                        }

                        // Oneshot jobs may also be queued by polling handlers; keep ticking if any remain.
                        if !cx.oneshot_jobs.is_empty() {
                            more_ticks = true;
                        }
                    }
                }

                ()
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
            return TickStatus::Stop;
        }

        if self.gc_req || self.gc_heap().should_gc() {
            self.gc_collect();
        }

        // Process FinalizationRegistry cleanups collected during GC
        #[cfg(feature = "builtin_weakref")]
        self.process_finrec_cleanups();

        if more_ticks {
            TickStatus::Continue
        } else {
            TickStatus::Stop
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TickStatus;

    use crate::{
        JSValue,
        builtins::promise::{Promise, PromiseResultSlot, PromiseState},
        runtime::Runtime,
        ScopedContext,
    };

    /// Drive the event loop until it stops (no more jobs to run).
    fn drain(rt: &mut Runtime) {
        while matches!(rt.tick(), TickStatus::Continue) {}
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
        let mut ctx = rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP).unwrap();

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
