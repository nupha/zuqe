// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Event-loop park state: lets a `Parked` round genuinely suspend the driving
//! thread instead of hot-spinning (`yield_now` + immediate self-wake).
//!
//! Wake sources drive a parked event loop:
//!
//! 1. **Deadline** — a polling job reports its earliest re-poll time via
//!    [`crate::ScopedContext::set_poll_hint`] (e.g. a timer's next deadline);
//!    `tick()` returns it as `TickStatus::Parked(Some(at))` and the engine
//!    hands it to the host-injected deadline waker
//!    ([`RuntimeImpl::set_deadline_waker`]). No threads are spawned by the
//!    engine: the host wakes the loop with its own timer facilities.
//! 2. **External wake** — IO producers (fs/fetch/browser/mcp tasks, channel
//!    senders via [`crate::WakeSender`], `PromiseResultSlot` commit/drop)
//!    call [`WakeHandle::wake`].
//! 3. **POSIX signals** — a signal handler may only call
//!    [`WakeHandle::wake_from_signal`], which does a single async-signal-safe
//!    `write()` into a self-pipe; the host watches the read end (see
//!    [`WakeHandle::signal_read_fd`]) and forwards a regular wake.
//!
//! By default the loop is purely event driven: every `Again` waiting point
//! must have a wake source, or the loop sleeps forever. With the
//! `quantum_poll` feature enabled, a host that cannot guarantee full wake
//! wiring can additionally enable a fallback quantum via
//! [`RuntimeImpl::use_quantum_poll`] (ms; 0 = off): every park is then
//! capped at that interval, so a missed wake degrades to a cheap periodic
//! poll instead of a lost wakeup.
//!
//! # Locking model
//!
//! ONE mutex ([`ParkState::inner`]) covers the mutable wake state — the
//! registered waker and the pending-wake flag. Invariants:
//!
//! * `wake()` sets `pending` **before** any wakeup side effect; consumers
//!   re-check `pending` after (re-)registering, which closes the
//!   check-then-park race window.
//! * Only `wake_from_signal` writes to the self-pipe; the host's pipe
//!   watcher forwards wakes without writing, so the pipe can never
//!   self-oscillate.

use std::{
    sync::{Arc, Condvar, Mutex},
    task::Waker,
    time::Instant,
};

/// Host-provided deadline waker: called on the JS thread every time the
/// event loop parks with a deadline; the implementation must arrange for
/// [`WakeHandle::wake`] (or an equivalent re-poll) at (or after) `at`.
pub type DeadlineWaker = dyn Fn(Instant) + Send + Sync;

#[derive(Default)]
struct ParkInner {
    /// Waker registered by the async driver (`RunFuture`) while parked.
    waker: Option<Waker>,
    /// Set by `wake()` before any wakeup side effect; consumed when the
    /// driver re-arms. Closes the check-then-park race window.
    pending: bool,
}

pub(crate) struct ParkState {
    inner: Mutex<ParkInner>,
    /// Signals the sync driver's condvar wait.
    parker_cond: Condvar,
    /// Host-injected deadline waker (see [`DeadlineWaker`]).
    pub(crate) deadline_waker: Mutex<Option<Arc<DeadlineWaker>>>,

    /// Self-pipe for signal-originated wakes (Unix). Only
    /// `wake_from_signal` writes; the host's pipe watcher consumes.
    #[cfg(unix)]
    pipe_r: AtomicI32,
    #[cfg(unix)]
    pipe_w: AtomicI32,
    /// Fallback quantum poll interval in ms (0 = off, the default). When
    /// set, every park is capped at this interval so a missed wake degrades
    /// to a cheap periodic poll. Set via `RuntimeImpl::use_quantum_poll`.
    /// Requires the `quantum_poll` feature.
    #[cfg(feature = "quantum_poll")]
    quantum_ms: AtomicU32,
    /// Warn-once flag: a deadline was due but no deadline waker is set.
    #[cfg(debug_assertions)]
    deadline_waker_warned: std::sync::atomic::AtomicBool,
}

impl ParkState {
    pub(crate) fn new() -> Self {
        #[cfg(unix)]
        let (pipe_r, pipe_w) = Self::create_pipe();
        Self {
            inner: Mutex::new(ParkInner::default()),
            parker_cond: Condvar::new(),
            deadline_waker: Mutex::new(None),
            #[cfg(unix)]
            pipe_r: AtomicI32::new(pipe_r),
            #[cfg(unix)]
            pipe_w: AtomicI32::new(pipe_w),
            #[cfg(feature = "quantum_poll")]
            quantum_ms: AtomicU32::new(0),
            #[cfg(debug_assertions)]
            deadline_waker_warned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Non-blocking pipe with both ends `O_NONBLOCK`; `(-1, -1)` on failure
    /// (signal wakes then degrade to no-ops).
    #[cfg(unix)]
    fn create_pipe() -> (i32, i32) {
        unsafe {
            let mut fds = [0 as libc::c_int; 2];
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return (-1, -1);
            }
            for fd in fds.iter() {
                let flags = libc::fcntl(*fd, libc::F_GETFL, 0);
                if flags >= 0 {
                    libc::fcntl(*fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
                }
            }
            (fds[0], fds[1])
        }
    }

    /// Async-signal-safe wakeup: write one byte into the self-pipe and
    /// nothing else (no locks, no allocation). The host's pipe watcher
    /// forwards a regular wake.
    #[cfg(unix)]
    fn write_pipe_byte(&self) {
        let fd = self.pipe_w.load(Ordering::Relaxed);
        if fd < 0 {
            return;
        }
        let byte = 1u8;
        loop {
            let n = unsafe { libc::write(fd, &byte as *const u8 as *const libc::c_void, 1) };
            if n < 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            // EAGAIN (pipe full) is fine: a byte is already queued, which is
            // all a coalesced wake needs.
            break;
        }
    }

    /// Async driver: register `waker` for later wakes and hand the deadline
    /// (already quantum-capped) to the host's deadline waker. One critical
    /// section, so a `wake()` racing this either has its `pending` flag
    /// visible to the check below or finds the waker registered and wakes
    /// it. No ordering rules for the caller.
    pub(crate) fn park_for(&self, waker: &Waker, deadline: Option<Instant>) {
        let stale = {
            let mut g = self.inner.lock().unwrap();
            g.waker.replace(waker.clone())
        };
        drop(stale); // stale registration from the previous park round

        // Hand the deadline to the host's deadline waker. With the
        // `quantum_poll` feature enabled, the deadline is additionally
        // capped by the fallback quantum.
        #[cfg(feature = "quantum_poll")]
        let at = match (deadline, self.quantum()) {
            (Some(d), Some(q)) => Some(d.min(Instant::now() + q)),
            (Some(d), None) => Some(d),
            (None, Some(q)) => Some(Instant::now() + q),
            (None, None) => None,
        };
        #[cfg(not(feature = "quantum_poll"))]
        let at = deadline;
        if let Some(at) = at {
            let g = self.deadline_waker.lock().unwrap();
            if let Some(f) = g.as_ref() {
                f(at);
            } else {
                // Nothing will ever deliver this deadline: timers cannot fire.
                #[cfg(debug_assertions)]
                self.warn_missing_deadline_waker();
            }
        }

        if self.take_pending() {
            // A wake raced with (re-)registration — re-tick immediately.
            waker.wake_by_ref();
        }
    }

    /// Sync driver: block the calling thread until `wake()` (condvar
    /// notification) or `deadline` (`None` = indefinitely).
    #[cfg(not(feature = "async_support"))]
    pub(crate) fn park_blocking(&self, deadline: Option<Instant>) {
        let mut g = self.inner.lock().unwrap();
        loop {
            if g.pending {
                g.pending = false;
                return;
            }

            let now = Instant::now();
            if deadline.is_some_and(|d| now >= d) {
                return;
            }

            // Sync mode needs no quantum for correctness of the deadline
            // itself (the condvar timeout returns at `deadline`); the
            // quantum only adds the missed-wake fallback for an otherwise
            // indefinite wait.
            #[cfg(feature = "quantum_poll")]
            let timeout = match (deadline, self.quantum()) {
                (Some(d), _) => Some(d.saturating_duration_since(now)),
                (None, Some(q)) => Some(q),
                (None, None) => None,
            };
            #[cfg(not(feature = "quantum_poll"))]
            let timeout = deadline.map(|d| d.saturating_duration_since(now));
            g = match timeout {
                Some(t) => self.parker_cond.wait_timeout(g, t).unwrap().0,
                None => self.parker_cond.wait(g).unwrap(),
            };
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
        // Rouse the sync driver's condvar wait (no-op cost otherwise).
        self.parker_cond.notify_all();
    }

    /// Take and clear the pending-wake flag.
    fn take_pending(&self) -> bool {
        let mut g = self.inner.lock().unwrap();
        std::mem::replace(&mut g.pending, false)
    }

    /// Debug-only, warn-once diagnostic: a deadline was due but the host
    /// never injected a deadline waker, so timer-based scripts will hang.
    #[cfg(debug_assertions)]
    fn warn_missing_deadline_waker(&self) {
        if !self
            .deadline_waker_warned
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            log::warn!(
                "[park] deadline wake requested but no deadline waker is set —
                 timers (setTimeout / Aike.sleep / setInterval) will never fire.
                 Call Runtime::set_deadline_waker() at startup."
            );
        }
    }

    /// Wake from a POSIX signal handler: only an async-signal-safe
    /// `write()` into the self-pipe — no locks, no allocation. The host's
    /// pipe watcher forwards a regular wake. On Windows console control
    /// handlers run as ordinary threads; call [`WakeHandle::wake`] there.
    pub fn wake_from_signal(&self) {
        #[cfg(unix)]
        self.write_pipe_byte();
        #[cfg(not(unix))]
        self.wake();
    }

    /// Write-end fd of the self-pipe for signal handlers (Unix; `-1` when
    /// unavailable). Store it before installing the handler; the handler
    /// writes one byte (what `wake_from_signal` does) without locks.
    #[cfg(unix)]
    pub fn signal_write_fd(&self) -> i32 {
        self.pipe_w.load(Ordering::Relaxed)
    }

    /// Read-end fd of the self-pipe (Unix). The host should watch it for
    /// readability and forward a regular [`WakeHandle::wake`] — that is what
    /// turns a signal-handler `write` into an event-loop wake.
    #[cfg(unix)]
    pub fn signal_read_fd(&self) -> i32 {
        self.pipe_r.load(Ordering::Relaxed)
    }

    /// Set the fallback quantum poll interval (ms; 0 = off).
    #[cfg(feature = "quantum_poll")]
    pub(crate) fn set_quantum_ms(&self, ms: u32) {
        self.quantum_ms.store(ms, Ordering::Relaxed);
    }

    /// Fallback quantum poll interval (0 = off).
    #[cfg(feature = "quantum_poll")]
    fn quantum(&self) -> Option<Duration> {
        let ms = self.quantum_ms.load(Ordering::Relaxed);
        (ms > 0).then(|| Duration::from_millis(ms as u64))
    }
}

impl Drop for ParkState {
    fn drop(&mut self) {
        #[cfg(unix)]
        for fd in [&self.pipe_r, &self.pipe_w] {
            let fd = fd.load(Ordering::Relaxed);
            if fd >= 0 {
                unsafe {
                    libc::close(fd);
                }
            }
        }
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

    /// Wake from a POSIX signal handler: async-signal-safe (`write()` into
    /// the self-pipe only). On Windows, call [`WakeHandle::wake`] instead —
    /// console control handlers run as ordinary threads.
    pub fn wake_from_signal(&self) {
        self.0.wake_from_signal();
    }

    /// Write-end fd of the self-pipe for signal handlers (Unix; `-1` when
    /// unavailable). Store it before installing the handler; the handler
    /// writes one byte (what `wake_from_signal` does) without locks.
    #[cfg(unix)]
    pub fn signal_write_fd(&self) -> i32 {
        self.0.signal_write_fd()
    }

    /// Read-end fd of the self-pipe (Unix). Watch it for readability and
    /// forward [`WakeHandle::wake`] to complete the signal wake path.
    #[cfg(unix)]
    pub fn signal_read_fd(&self) -> i32 {
        self.0.signal_read_fd()
    }
}
