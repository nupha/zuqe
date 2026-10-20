// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Background compiler workers (feature `jit_background`; plan §12).
//!
//! Verify + Cranelift lowering move off the runtime thread; the runtime thread
//! stays the **single writer** of everything shared (the `jit` slots, the code
//! cache, metrics, the coordinator). A worker receives only `Send` data — a
//! [`CompileJob`] holding a [`CompileSnapshot`] and [`VerifyLimits`] — and sends
//! back owned code bytes. It never touches a runtime pointer.
//!
//! Backpressure is one job per worker: `dispatch` only sends while
//! `in_flight < workers`, and the job channel is bounded to `workers`, so the
//! backlog stays in the coordinator queue (where it stays reorderable) instead
//! of blocking the runtime thread.

use std::{
    cell::Cell,
    collections::HashMap,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use cranelift_codegen::isa::TargetIsa;

use crate::jit::{
    Lowered, bytecode::VerifyLimits, error::JitError, lower, snapshot::CompileSnapshot,
};

/// One attempt handed to a worker. Everything here is `Send` and holds no GC
/// pointer, so the worker runs without the runtime.
pub(crate) struct CompileJob {
    pub(crate) attempt_id: u64,
    pub(crate) snapshot: CompileSnapshot,
    pub(crate) limits: VerifyLimits,
}

/// What a worker produced. `Compiled` carries owned code bytes, never a slot.
pub(crate) enum JobResult {
    Compiled { bytes: Box<[u8]>, specialized: u64 },
    Rejected,
    Failed,
}

/// A finished attempt, matched to its dispatch by `attempt_id`.
pub(crate) struct CompileCompletion {
    pub(crate) attempt_id: u64,
    pub(crate) result: JobResult,
    /// How long the attempt took (verification + codegen), in nanoseconds.
    /// The worker measures it; the runtime thread folds it into the metrics
    /// (it stays the single writer of the counters).
    pub(crate) nanos: u64,
}

/// Per-attempt bookkeeping the runtime thread keeps until the worker finishes.
pub(crate) struct InFlight {
    pub(crate) function_id: u64,
    dispatched_at: Instant,
}

/// Bounded compiler workers plus the in-flight bookkeeping of the runtime side.
pub(crate) struct BackgroundCompiler {
    jobs: Option<mpsc::SyncSender<CompileJob>>,
    completions: mpsc::Receiver<CompileCompletion>,
    workers: Vec<JoinHandle<()>>,
    in_flight: HashMap<u64, InFlight>,
    next_attempt_id: u64,
    capacity: usize,
}

impl BackgroundCompiler {
    /// Spawn `workers` threads. Fails closed if a thread cannot be spawned (the
    /// caller then refuses to attach a JIT rather than run half-initialized).
    pub(crate) fn new(workers: usize, isa: Arc<dyn TargetIsa>) -> Result<Self, JitError> {
        let (jobs, job_rx) = mpsc::sync_channel::<CompileJob>(workers);
        let job_rx = Arc::new(Mutex::new(job_rx));
        let (completion_tx, completions) = mpsc::channel::<CompileCompletion>();

        let mut handles = Vec::with_capacity(workers);
        for index in 0..workers {
            let rx = Arc::clone(&job_rx);
            let tx = completion_tx.clone();
            let isa = Arc::clone(&isa);
            let spawned = thread::Builder::new()
                .name(format!("zhuque-jit-{index}"))
                .spawn(move || worker_loop(rx, tx, isa));
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(e) => {
                    // Drop every sender so the workers already running see
                    // `recv` fail and exit; then join them. No half-started
                    // compiler is left behind.
                    drop(jobs);
                    drop(job_rx);
                    for handle in handles {
                        let _ = handle.join();
                    }
                    return Err(JitError::Platform(format!("jit worker spawn: {e}")));
                }
            }
        }
        // The workers are the only senders from here on; dropping the original
        // makes channel disconnection mean "shut down".
        drop(completion_tx);
        Ok(Self {
            jobs: Some(jobs),
            completions,
            workers: handles,
            in_flight: HashMap::new(),
            next_attempt_id: 0,
            capacity: workers,
        })
    }

    /// Whether another attempt may be handed to a worker.
    pub(crate) fn has_capacity(&self) -> bool {
        self.in_flight.len() < self.capacity
    }

    /// Attempts in flight (also the `functions_compiling` metric).
    pub(crate) fn in_flight_len(&self) -> usize {
        self.in_flight.len()
    }

    /// Send one snapshot to a worker. Returns false (and leaves the caller to
    /// roll the request back) when there is no capacity or the channel is full,
    /// so the runtime thread never blocks.
    pub(crate) fn submit(
        &mut self,
        function_id: u64,
        snapshot: CompileSnapshot,
        limits: VerifyLimits,
    ) -> bool {
        if !self.has_capacity() {
            return false;
        }
        let Some(sender) = self.jobs.as_ref() else {
            return false;
        };
        let attempt_id = self.next_attempt_id;
        if sender
            .try_send(CompileJob {
                attempt_id,
                snapshot,
                limits,
            })
            .is_err()
        {
            return false;
        }
        self.next_attempt_id = self.next_attempt_id.wrapping_add(1);
        self.in_flight.insert(
            attempt_id,
            InFlight {
                function_id,
                dispatched_at: Instant::now(),
            },
        );
        true
    }

    /// A finished attempt, if one arrived since the last call. Never blocks.
    pub(crate) fn try_recv(&mut self) -> Option<CompileCompletion> {
        self.completions.try_recv().ok()
    }

    /// Take the bookkeeping of `attempt_id` off the in-flight map. `None` means
    /// the completion is stale (already abandoned or invalidated) and must be
    /// dropped.
    pub(crate) fn resolve(&mut self, attempt_id: u64) -> Option<InFlight> {
        self.in_flight.remove(&attempt_id)
    }

    /// Abandon every attempt past `timeout` and return their function ids.
    ///
    /// The abandoned entry is **removed**, not just flagged: `in_flight` is both
    /// the capacity gate (`has_capacity`) and the `functions_compiling` metric,
    /// so leaving it would keep the worker slot reserved until the abandoned
    /// attempt happens to finish — and the retry this abandonment triggers could
    /// not even be dispatched until then, which defeats the budget. Removing it
    /// hands the slot back at once; the worker keeps running (Cranelift lowering
    /// cannot be preempted) and its late completion no longer matches an in-flight
    /// attempt, so `resolve` reports it stale and the caller drops it.
    pub(crate) fn expire(&mut self, timeout: Duration) -> Vec<u64> {
        let mut expired = Vec::new();
        self.in_flight.retain(|_, job| {
            if job.dispatched_at.elapsed() > timeout {
                expired.push(job.function_id);
                false
            } else {
                true
            }
        });
        expired
    }
}

impl Drop for BackgroundCompiler {
    fn drop(&mut self) {
        // Dropping the sender disconnects the channel; every worker's `recv`
        // then fails and the loop breaks, so the joins terminate.
        self.jobs.take();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

/// One worker: pull a job, lower it, send the result. No `unwrap`/`panic!` on
/// this path: under `panic = "abort"` a panic here would kill the process, so
/// every fallible step returns a value.
fn worker_loop(
    rx: Arc<Mutex<mpsc::Receiver<CompileJob>>>,
    tx: mpsc::Sender<CompileCompletion>,
    isa: Arc<dyn TargetIsa>,
) {
    loop {
        let job = {
            let guard = match rx.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.recv()
        };
        let Ok(CompileJob {
            attempt_id,
            snapshot,
            limits,
        }) = job
        else {
            break;
        };
        // `verify`/`lower` are Result-based, but a Cranelift bug could still
        // panic; under `panic = "unwind"` this contains it. Under
        // `panic = "abort"` it is a no-op and the process aborts, which is why
        // the worker path itself must not panic.
        let elapsed = Cell::new(0);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lower(snapshot, limits, isa.as_ref(), &elapsed)
        }))
        .map_or(JobResult::Failed, |lowered| match lowered {
            Lowered::Compiled { bytes, specialized } => JobResult::Compiled { bytes, specialized },
            Lowered::Rejected => JobResult::Rejected,
            Lowered::Failed => JobResult::Failed,
        });
        if tx
            .send(CompileCompletion {
                attempt_id,
                result,
                nanos: elapsed.get(),
            })
            .is_err()
        {
            break;
        }
    }
}

/// The worker contract: a job and a completion cross the thread boundary.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<CompileJob>();
    assert_send::<CompileCompletion>();
    assert_send::<JobResult>();
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jit::{build_host_isa, snapshot::CompileSnapshot};

    fn isa() -> Arc<dyn TargetIsa> {
        build_host_isa().expect("host isa")
    }

    /// The worker lowers on its own thread and the completion comes back with
    /// the dispatch's attempt id.
    #[test]
    fn a_job_is_lowered_off_thread_and_matched_by_attempt_id() {
        let mut bg = BackgroundCompiler::new(1, isa()).expect("spawn");
        let snapshot = CompileSnapshot::from_raw(vec![0u8; 4], 0, 0, 0, 8);
        assert!(bg.submit(7, snapshot, VerifyLimits::default()));
        assert_eq!(bg.in_flight_len(), 1);

        let completion = loop {
            if let Some(completion) = bg.try_recv() {
                break completion;
            }
            std::thread::yield_now();
        };
        assert_eq!(completion.attempt_id, 0);
        // The hand-built snapshot is not a valid function, so the result is a
        // fail-closed rejection rather than a compile.
        assert!(matches!(completion.result, JobResult::Rejected));
    }

    /// `Drop` joins every worker; if it did not, a blocked worker would hang
    /// the test.
    #[test]
    fn drop_without_pending_jobs_joins_the_workers() {
        let bg = BackgroundCompiler::new(3, isa()).expect("spawn");
        drop(bg);
    }

    /// An expired attempt must release its worker slot immediately, so the retry
    /// it triggers can be dispatched without waiting for the abandoned lowering
    /// to finish. `in_flight` is both the capacity gate and the
    /// `functions_compiling` metric, so a leftover entry there parks the retry
    /// behind a zombie.
    #[test]
    fn expiring_an_attempt_frees_its_slot_without_waiting_for_the_worker() {
        let mut bg = BackgroundCompiler::new(1, isa()).expect("spawn");
        let snapshot = CompileSnapshot::from_raw(vec![0u8; 4], 0, 0, 0, 8);
        assert!(bg.submit(7, snapshot.clone(), VerifyLimits::default()));
        assert_eq!(bg.in_flight_len(), 1);
        assert!(!bg.has_capacity(), "the single worker is busy");

        // Zero budget: the just-dispatched attempt is instantly over it.
        let expired = bg.expire(Duration::ZERO);
        assert_eq!(expired, vec![7], "its function id is reported for retry");
        assert_eq!(
            bg.in_flight_len(),
            0,
            "the abandoned attempt is not in flight"
        );
        assert!(bg.has_capacity(), "the slot is free for the retry");

        // The late completion of the abandoned attempt must not resolve the
        // retry: attempt ids do not collide.
        let retry = loop {
            if let Some(completion) = bg.try_recv() {
                break completion;
            }
            std::thread::yield_now();
        };
        let still_in_flight = bg.resolve(retry.attempt_id);
        assert!(
            still_in_flight.is_none(),
            "a drained attempt no longer matches anything in flight"
        );
    }
}
