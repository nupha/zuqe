// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Event-loop park state: lets a `Parked` round genuinely suspend the driving
//! thread instead of hot-spinning (`yield_now` + immediate self-wake).
//!
//! Two wake sources drive a parked event loop:
//!
//! 1. **Deadline** — a polling job reports its earliest re-poll time via
//!    [`crate::ScopedContext::set_poll_hint`] (e.g. a timer's next deadline);
//!    `tick()` returns it as `TickStatus::Parked(Some(at))`. The async driver
//!    hands it to a dedicated parker thread which sleeps until the deadline
//!    and then wakes the loop. The sync driver simply blocks on a condvar.
//! 2. **External wake** — IO producers (fs/fetch/browser/mcp tasks, channel
//!    senders, `PromiseResultSlot::commit`) call [`WakeHandle::wake`] when new
//!    work arrives, turning the next `tick()` into `Progress`.
//!
//! Every park is additionally capped at [`PARK_QUANTUM`]: a producer that
//! misses its `wake()` call degrades to a cheap ~5 ms poll instead of a lost
//! wakeup (the loop sleeping forever). Waking ~200 times/s costs a fraction
//! of a percent of a core, versus the 95%+ hot spin this replaces.

use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::Waker,
    thread,
    time::{Duration, Instant},
};

/// Upper bound of a single park. Applied to every park, with or without a
/// deadline hint (see module docs — missed-`wake()` safety net).
pub(crate) const PARK_QUANTUM: Duration = Duration::from_millis(5);

/// Idle wait of the parker thread between liveness checks.
const PARKER_IDLE: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Inner {
    /// Waker registered by the async driver (`RunFuture`) while parked.
    waker: Option<Waker>,
    /// Set by `wake()` before any wakeup side effect; consumed when the
    /// driver re-arms. Closes the check-then-park race window.
    pending: bool,
}

pub(crate) struct ParkState {
    inner: Mutex<Inner>,
    /// Signals `park_blocking` (sync driver) waiters.
    cond: Condvar,
    /// Next deadline requested from the parker thread (async driver).
    parker_req: Mutex<Option<Instant>>,
    parker_cond: Condvar,
    parker_started: AtomicBool,
    /// Set at runtime drop so the parker thread can exit even though it still
    /// holds a strong `Arc<ParkState>` while waiting.
    retired: AtomicBool,
}

impl ParkState {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            cond: Condvar::new(),
            parker_req: Mutex::new(None),
            parker_cond: Condvar::new(),
            parker_started: AtomicBool::new(false),
            retired: AtomicBool::new(false),
        }
    }

    /// Wake the parked event loop. Safe from any thread; cheap when the loop
    /// is not parked (leaves a pending flag that the next park consumes).
    pub(crate) fn wake(&self) {
        let waker = {
            let mut g = self.inner.lock().unwrap();
            g.pending = true;
            g.waker.take()
        };
        if let Some(w) = waker {
            w.wake();
        }
        self.cond.notify_all();
    }

    /// Take and clear the pending-wake flag.
    fn take_pending(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        std::mem::replace(&mut g.pending, false)
    }

    /// Async driver: register the task waker for a later `wake()`. Returns
    /// true if a wake already arrived — the caller must re-tick immediately
    /// instead of parking. Arm the deadline timer *before* calling this.
    pub(crate) fn register(&self, waker: &Waker) -> bool {
        // Drop the stale registration from the previous park round silently.
        let old = self.inner.lock().unwrap().waker.replace(waker.clone());
        drop(old);
        self.take_pending()
    }

    /// Sync driver: block the calling thread until `wake()` or `deadline`.
    pub(crate) fn park_blocking(&self, deadline: Instant) {
        let mut g = self.inner.lock().unwrap();
        loop {
            if g.pending {
                g.pending = false;
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let (g2, _) = self.cond.wait_timeout(g, deadline - now).unwrap();
            g = g2;
        }
    }

    /// Async driver: arm the parker thread to `wake()` us at `deadline`.
    /// Must be called before `register` (a wake racing either way is caught
    /// by the pending flag).
    #[cfg(feature = "async_support")]
    pub(crate) fn park_async(self: &Arc<Self>, deadline: Instant) {
        self.spawn_parker();
        *self.parker_req.lock().unwrap() = Some(deadline);
        self.parker_cond.notify_all();
    }

    /// Spawn the one-shot parker thread on first async park. It sleeps until
    /// the requested deadline and wakes the event loop. Parks are capped at
    /// [`PARK_QUANTUM`], so it handles ~200 requests/s while the loop is idle.
    #[cfg(feature = "async_support")]
    fn spawn_parker(self: &Arc<Self>) {
        if self.parker_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let weak = Arc::downgrade(self);
        let _ = thread::Builder::new()
            .name("zhuque-parker".into())
            .spawn(move || {
                loop {
                    // Hold the strong ref only while waiting for a request so the
                    // runtime stays droppable; `retired` flags the owner is gone.
                    let Some(st) = weak.upgrade() else { return };
                    let deadline = {
                        let mut req = st.parker_req.lock().unwrap();
                        loop {
                            if let Some(d) = *req {
                                *req = None;
                                break d;
                            }
                            let (r, _) = st.parker_cond.wait_timeout(req, PARKER_IDLE).unwrap();
                            req = r;
                            if st.retired.load(Ordering::Acquire) {
                                return;
                            }
                        }
                    };
                    drop(st);

                    let now = Instant::now();
                    if deadline > now {
                        thread::sleep(deadline - now);
                    }

                    let Some(st) = weak.upgrade() else { return };
                    st.wake();
                }
            });
    }

    /// Signal the parker thread that the runtime is going away.
    pub(crate) fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        self.parker_cond.notify_all();
    }
}

/// Clonable handle for external producers (IO threads / tokio tasks) to wake
/// a parked event loop. Cheap to call when the loop is not parked.
#[derive(Clone)]
pub struct WakeHandle(pub(crate) Arc<ParkState>);

impl WakeHandle {
    /// Wake the event loop so it re-runs `tick()`. Call after pushing new
    /// work (channel send, slot commit, ...). No-op cost when not parked.
    pub fn wake(&self) {
        self.0.wake();
    }
}
