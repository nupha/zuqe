// test262 $262.agent harness
//
// Implements the multi-agent (concurrent thread) API used by Atomics /
// SharedArrayBuffer tests. The parent side (running inside the main test
// Runtime) coordinates a set of child agents, each of which runs its own
// Runtime + Context in a dedicated OS thread.
//
// Design notes:
//   * zuqe's Runtime is NOT Send (it is Rc<UnsafeCell<..>>). We therefore
//     create each agent's Runtime *inside* the spawned thread, so the
//     Runtime lives and dies on a single thread. This avoids any cross-thread
//     transfer of the non-Send handle.
//   * SharedArrayBuffer memory is `Arc<RwLock<Box<[u8]>>>` (`ABufData::Shared`),
//     which *is* Send+Sync, so it can be cloned and handed to the child thread
//     safely. The child rebuilds an ArrayBuffer view over the same backing.
//   * Agent state is reached from native callbacks via the process-global hub
//     singleton (agents_hub) and the agent id carried in the native function's
//     `magic` slot, because native callbacks are plain `fn` pointers and cannot
//     capture environment.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};

use zuqe::builtins::array_buffer::ArrayBuffer;
use zuqe::builtins::promise::Promise;
use zuqe::{
    CallContext, Callback, EvalFlags, EvalName, EvalType, JSValue, Object, Runtime, ScopedContext,
};

#[derive(Clone)]
struct Broadcast {
    // shared memory backing, if the broadcast message is (or includes) a SAB
    sab: Option<Arc<RwLock<Box<[u8]>>>>,
    // textual message, if the broadcast carries a string
    msg: Option<String>,
    // Signalled once the agent has consumed this broadcast and run its
    // `receiveBroadcast` callback. The parent's `$262.agent.broadcast` waits on
    // this so that operations performed inside the callback (e.g.
    // `Atomics.waitAsync` registering a waiter) are guaranteed to have
    // completed before the parent's subsequent `Atomics.notify` runs.
    consumed: Option<Arc<(Mutex<bool>, Condvar)>>,
}

struct HubInner {
    broadcast: Option<Broadcast>,
    reports: VecDeque<String>,
    leaving: bool,
    shutdown: bool,
}

/// A per-test hub instance. Each agent test gets a *fresh* `Hub` wrapped in an
/// `Arc`; agent threads hold a clone of that `Arc` for the whole test. At the
/// end of the test the hub is torn down (waking every blocked agent) and the
/// *next* test allocates a brand-new `Hub`. Because agent threads reference the
/// old `Arc`, they exit cleanly without touching the new test's hub — this fully
/// isolates tests from each other.
struct Hub {
    inner: Mutex<HubInner>,
    cond: Condvar,
}

impl Hub {
    fn new() -> Self {
        Hub {
            cond: Condvar::new(),
            inner: Mutex::new(HubInner {
                broadcast: None,
                reports: VecDeque::new(),
                leaving: false,
                shutdown: false,
            }),
        }
    }

    /// Wake all agents waiting on a broadcast.
    fn broadcast(&self, b: Broadcast) {
        let mut inner = self.inner.lock().unwrap();
        inner.broadcast = Some(b);
        drop(inner);
        self.cond.notify_all();
    }

    /// Wait for the next broadcast. Blocks until one is available or the hub is
    /// torn down (in which case it returns `None` so the agent thread can exit).
    fn wait_broadcast(&self) -> Option<Broadcast> {
        let mut inner = self.inner.lock().unwrap();
        while inner.broadcast.is_none() && !inner.shutdown {
            inner = self.cond.wait(inner).unwrap();
        }
        if inner.shutdown && inner.broadcast.is_none() {
            return None;
        }
        inner.broadcast.clone()
    }

    /// Tear down: wake every agent thread still blocked. Called at the end of
    /// each agent test.
    fn teardown(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.shutdown = true;
        inner.broadcast = None;
        drop(inner);
        self.cond.notify_all();
    }

    /// Child agent reports a string to be fetched by the parent.
    fn report(&self, msg: String) {
        let mut inner = self.inner.lock().unwrap();
        inner.reports.push_back(msg);
        self.cond.notify_all();
    }

    /// Parent fetches one pending report (blocks until available).
    fn get_report(&self) -> String {
        let mut inner = self.inner.lock().unwrap();
        while inner.reports.is_empty() {
            inner = self.cond.wait(inner).unwrap();
        }
        inner.reports.pop_front().unwrap()
    }

    /// Indicate an agent is leaving; wake the parent if it is waiting.
    fn leaving(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.leaving = true;
        self.cond.notify_all();
    }

    /// Whether an agent has called `$262.agent.leaving()` (its script finished).
    fn is_leaving(&self) -> bool {
        self.inner.lock().unwrap().leaving
    }
}

pub(crate) struct AgentHub {
    // Current test's hub. Replaced wholesale at the start of each test.
    hub: Mutex<Arc<Hub>>,
    // Join handles for the currently/last spawned agent threads.
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

impl AgentHub {
    /// The hub for the currently running test.
    fn current(&self) -> Arc<Hub> {
        self.hub.lock().unwrap().clone()
    }

    /// Install a brand-new hub for the next test, dropping the previous one.
    pub(crate) fn replace(&self) {
        *self.hub.lock().unwrap() = Arc::new(Hub::new());
    }
}

pub(crate) fn agents_hub() -> &'static AgentHub {
    static HUB: OnceLock<AgentHub> = OnceLock::new();
    HUB.get_or_init(|| AgentHub {
        hub: Mutex::new(Arc::new(Hub::new())),
        threads: Mutex::new(Vec::new()),
    })
}

// Per-agent-thread hub reference. Each spawned agent thread installs the `Arc<Hub>`
// for its own test here, so child-side callbacks can reach exactly the hub they
// belong to (and never the next test's hub). This is what makes tests fully
// isolated from one another.
thread_local! {
    static CURRENT_HUB: RefCell<Option<Arc<Hub>>> = const { RefCell::new(None) };
    static READY_SIG: RefCell<Option<Arc<(Mutex<bool>, Condvar)>>> = const { RefCell::new(None) };
}

/// Access the hub belonging to the current agent thread.
fn thread_hub() -> Arc<Hub> {
    CURRENT_HUB
        .with(|h| h.borrow().clone())
        .expect("agent callback outside agent thread")
}

/// Build a JS string value from a Rust string. `JSValue: From<String>` does not
/// exist, so we allocate a StringRef via the context and wrap it.
fn to_js_string(ctx: &mut ScopedContext, s: &str) -> Result<JSValue, i32> {
    let sr = ctx
        .create_string_from_utf8(s.as_bytes().to_vec())
        .map_err(|_| -1)?;
    Ok(JSValue::make_string(sr))
}

/// Set up the parent-side `$262.agent` object, installing it onto the given
/// `$262` target object.
pub fn setup_parent_agent(ctx: &mut ScopedContext, obj262: &mut Object) -> Result<(), i32> {
    let mut agent = ctx.create_base_object().map_err(|_| -1)?;

    let f = ctx.create_native_function(Callback::Func(agent_start), "start", 1, 0)?;
    agent
        .set_property_str(ctx, "start", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    let f = ctx.create_native_function(Callback::Func(agent_broadcast), "broadcast", 1, 0)?;
    agent
        .set_property_str(ctx, "broadcast", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    let f = ctx.create_native_function(Callback::Func(agent_get_report), "getReport", 0, 0)?;
    agent
        .set_property_str(ctx, "getReport", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    let f = ctx.create_native_function(Callback::Func(agent_sleep), "sleep", 1, 0)?;
    agent
        .set_property_str(ctx, "sleep", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    let f =
        ctx.create_native_function(Callback::Func(agent_monotonic_now), "monotonicNow", 0, 0)?;
    agent
        .set_property_str(ctx, "monotonicNow", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    let f = ctx.create_native_function(
        Callback::Func(agent_safe_broadcast_async),
        "safeBroadcastAsync",
        3,
        0,
    )?;
    agent
        .set_property_str(ctx, "safeBroadcastAsync", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    let f = ctx.create_native_function(
        Callback::Func(agent_get_report_async),
        "getReportAsync",
        0,
        0,
    )?;
    agent
        .set_property_str(ctx, "getReportAsync", f.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    // Timeout constants used by $262.agent.tryYield / trySleep / etc.
    let mut timeouts = ctx.create_base_object().map_err(|_| -1)?;
    timeouts
        .set_property_str(ctx, "yield", JSValue::from(100.0))
        .map(|_| ())
        .map_err(|_| -1)?;
    timeouts
        .set_property_str(ctx, "small", JSValue::from(200.0))
        .map(|_| ())
        .map_err(|_| -1)?;
    timeouts
        .set_property_str(ctx, "long", JSValue::from(1000.0))
        .map(|_| ())
        .map_err(|_| -1)?;
    timeouts
        .set_property_str(ctx, "huge", JSValue::from(10000.0))
        .map(|_| ())
        .map_err(|_| -1)?;
    agent
        .set_property_str(ctx, "timeouts", timeouts.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    // `setTimeout`: needed by the test262 `atomicsHelper.js` harness, whose
    // `$262.agent.setTimeout` and `getReportAsync` rely on a global `setTimeout`.
    // Provide both a global binding and `$262.agent.setTimeout`.
    let st_fn = ctx.create_native_function(Callback::Func(set_timeout), "setTimeout", 2, 0)?;
    ctx.global_obj()
        .set_property_str(ctx, "setTimeout", st_fn.into())
        .map(|_| ())
        .map_err(|_| -1)?;
    let st_fn2 = ctx.create_native_function(Callback::Func(set_timeout), "setTimeout", 2, 0)?;
    agent
        .set_property_str(ctx, "setTimeout", st_fn2.into())
        .map(|_| ())
        .map_err(|_| -1)?;

    obj262
        .set_property_str(ctx, "agent", agent.into())
        .map(|_| ())
        .map_err(|_| -1)
}

// Parent-side native functions (non-capturing; use agents_hub()).

fn agent_start(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let src = cc
        .arg(0)
        .to_string(ctx)
        .map_err(|_| -1)?
        .encode_utf8(false)
        .to_string();

    let n = cc.arg(1).to_number(ctx).ok().map(|n| n.to_i32_saturating());
    let n = if let Some(n) = n {
        if n <= 0 { 1 } else { n as usize }
    } else {
        1
    };

    // Capture the current hub so the spawned threads reference exactly this
    // test's hub for their whole lifetime. The hub is (re)created once per test
    // by `agents_hub().replace()`, so multiple `start` calls within a test share it — exactly
    // matching test262's expectation that all agents in one test use one channel.
    let hub = agents_hub().current();
    let mut handles = Vec::with_capacity(n);
    let mut ready_sigs = Vec::with_capacity(n);
    for _ in 0..n {
        let src = src.clone();
        let hub = hub.clone();
        // Per-agent readiness signal so each `start` call blocks until *its own*
        // agent has reached `receiveBroadcast`, regardless of other agents.
        let ready_sig = Arc::new((Mutex::new(false), Condvar::new()));
        ready_sigs.push(ready_sig.clone());
        handles.push(std::thread::spawn(move || {
            agent_thread(hub, src, ready_sig);
        }));
    }
    agents_hub().threads.lock().unwrap().extend(handles);

    // Block until every spawned agent has finished spawning and its JS context is
    // fully set up (globals installed), so the parent may safely broadcast or
    // touch the shared buffer. The signal is raised in `agent_thread` once the
    // context is ready — before the agent source is evaluated — so `start`
    // returns as soon as the agent can run, whether or not it later calls
    // `receiveBroadcast`.
    for sig in ready_sigs {
        let (lock, cvar) = &*sig;
        let mut ready = lock.lock().unwrap();
        while !*ready {
            ready = cvar.wait(ready).unwrap();
        }
    }
    Ok(JSValue::UNDEFINED)
}

/// Release any agent threads still blocked in `receiveBroadcast` or
/// `Atomics.wait` so they can exit, preventing cross-test leakage of blocked
/// agent threads and stale hub state. Called after every agent test finishes.
pub fn teardown_agents() {
    // Release `Atomics.wait` blockers first, then `receiveBroadcast` blockers.
    zuqe::builtins::atomics::wake_all_waiters();
    agents_hub().current().teardown();
}

fn agent_broadcast(_ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    // test262 broadcast(message, sharedArrayBuffer?) — message may itself be a
    // SAB (as in $262.agent.safeBroadcast which calls broadcast(typedArray.buffer)),
    // or a string. An optional second SAB argument is also honoured.

    let mut sab: Option<Arc<RwLock<Box<[u8]>>>> = None;
    let mut msg: Option<String> = None;

    let a0 = cc.arg(0);
    if let Ok(ab) = ArrayBuffer::try_from(a0) {
        sab = ab.shared_data();
    } else {
        // Try to capture as a string message.
        if let Ok(sr) = a0.to_string(_ctx) {
            msg = Some(sr.encode_utf8(false).to_string());
        }
    }

    // Optional explicit SAB argument.
    if let Ok(ab) = ArrayBuffer::try_from(cc.arg(1))
        && let Some(data) = ab.shared_data()
    {
        sab = Some(data);
    }

    let consumed = Arc::new((Mutex::new(false), Condvar::new()));
    agents_hub().current().broadcast(Broadcast {
        sab,
        msg,
        consumed: Some(consumed.clone()),
    });
    // Wait (with a bounded fallback) until the agent has consumed the broadcast
    // and run its `receiveBroadcast` callback. This guarantees that any waiter
    // registered by `Atomics.waitAsync` inside the callback exists before the
    // parent issues `Atomics.notify` next, matching test262's agent semantics.
    // The timeout avoids deadlock for tests that never call `receiveBroadcast`.
    let (lock, cvar) = &*consumed;
    let guard = lock.lock().unwrap();
    let (_unused, res) = cvar
        .wait_timeout(guard, std::time::Duration::from_secs(10))
        .unwrap();
    let _ = res;
    Ok(JSValue::UNDEFINED)
}

/// `$262.agent.safeBroadcastAsync(sab, index, count)` — broadcasts the shared
/// buffer of `sab` to every agent (mirroring `$262.agent.broadcast`) and
/// resolves with the number of agents that received it (always 1 here, as zhuque
/// runs a single agent per `start`). The parent blocks until the agent has
/// consumed the broadcast and run its `receiveBroadcast` callback, so any waiter
/// registered by `Atomics.waitAsync` is live before the parent's subsequent
/// `Atomics.notify`.
fn agent_safe_broadcast_async(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let a0 = cc.arg(0);
    let sab = if let Ok(ab) = ArrayBuffer::try_from(a0) {
        ab.shared_data()
    } else {
        None
    };
    if sab.is_none() {
        // Fall back to a plain string message broadcast.
        let msg = a0
            .to_string(ctx)
            .ok()
            .map(|s| s.encode_utf8(false).to_string());
        let consumed = Arc::new((Mutex::new(false), Condvar::new()));
        agents_hub().current().broadcast(Broadcast {
            sab: None,
            msg,
            consumed: Some(consumed.clone()),
        });
        let (lock, cvar) = &*consumed;
        let _ = cvar
            .wait_timeout(lock.lock().unwrap(), std::time::Duration::from_secs(10))
            .unwrap();
    } else {
        let consumed = Arc::new((Mutex::new(false), Condvar::new()));
        agents_hub().current().broadcast(Broadcast {
            sab,
            msg: None,
            consumed: Some(consumed.clone()),
        });
        let (lock, cvar) = &*consumed;
        let _ = cvar
            .wait_timeout(lock.lock().unwrap(), std::time::Duration::from_secs(10))
            .unwrap();
    }
    // zhuque spawns exactly one agent per `start`; resolve with 1.
    Promise::from_resolve_value(ctx, JSValue::from(1.0)).map(JSValue::from)
}

/// `$262.agent.getReportAsync()` — resolves with the next report value sent by
/// an agent via `$262.agent.report` (preserving its primitive type). Blocks the
/// parent synchronously until the report arrives (the agent runs on a separate OS
/// thread, so this does not deadlock) and wraps the value in an already-resolved
/// Promise.
fn agent_get_report_async(ctx: &mut ScopedContext, _cc: CallContext) -> Result<JSValue, i32> {
    let r = agents_hub().current().get_report();
    let v = to_js_string(ctx, &r)?;
    Promise::from_resolve_value(ctx, v).map(JSValue::from)
}

fn agent_get_report(ctx: &mut ScopedContext, _cc: CallContext) -> Result<JSValue, i32> {
    let r = agents_hub().current().get_report();
    to_js_string(ctx, &r)
}

fn agent_sleep(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let ms = cc.arg(0).to_number(ctx).ok().map(|n| n.to_f64());
    if let Some(ms) = ms
        && ms > 0.0
    {
        std::thread::sleep(std::time::Duration::from_secs_f64(ms / 1000.0));
    }
    Ok(JSValue::UNDEFINED)
}

fn agent_monotonic_now(_ctx: &mut ScopedContext, _cc: CallContext) -> Result<JSValue, i32> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    Ok(JSValue::from(now as f64))
}

/// `setTimeout(callback, delay)` — schedules `callback` to run after roughly
/// `delay` ms. zhuque has no OS-timer backed event loop, so this registers a
/// *polling* job that yields (`Again`) until `delay` has elapsed, then invokes
/// the callback and completes.
///
/// Using a polling job (rather than a one-shot job that sleeps) is important:
/// a one-shot job runs inside the tick's one-shot drain loop, so a
/// self-rescheduling loop like test262's `wait() { ...; setTimeout(wait, 0) }`
/// would recursively execute forever within a single tick and starve the
/// polling jobs that settle `Atomics.waitAsync` promises. A polling job yields
/// each tick, letting the waitAsync polling jobs run in between.
fn set_timeout(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let cb = cc.arg(0);
    let delay = cc
        .arg(1)
        .to_number(ctx)
        .ok()
        .map(|n| n.to_f64())
        .unwrap_or(0.0);
    let delay_ns = if delay.is_finite() && delay > 0.0 {
        (delay * 1_000_000.0) as u64
    } else {
        0
    };

    let start = std::time::Instant::now();
    ctx.queue_job(
        move |cx| -> Result<zuqe::JobState, i32> {
            if start.elapsed().as_nanos() as u64 >= delay_ns {
                if let Ok(f) = Object::try_from(cb) {
                    let _ = f.call(cx, JSValue::UNDEFINED, &[]);
                }
                Ok(zuqe::JobState::Done)
            } else {
                Ok(zuqe::JobState::Block)
            }
        },
        cb,
    );
    Ok(JSValue::UNDEFINED)
}

// Child-side agent thread.

fn agent_thread(hub: Arc<Hub>, src: String, ready_sig: Arc<(Mutex<bool>, Condvar)>) {
    // Install this test's hub for the lifetime of the thread so the child-side
    // callbacks can reach exactly their own hub.
    CURRENT_HUB.with(|h| *h.borrow_mut() = Some(hub));
    READY_SIG.with(|s| *s.borrow_mut() = Some(ready_sig));

    let mut rt = match Runtime::create() {
        Ok(rt) => rt,
        Err(_) => return,
    };

    // Set up the child JS context (ECMA_CONTEXT_SETUP is required before any
    // eval) and install the `agent` / `$262.agent` globals. We do NOT eval the
    // agent source inside the setup closure — the parent runner evaluates after
    // `create_context` returns with a `Context`, so we mirror that here.
    let mut ctx = match rt.create_context(|sctx| {
        ScopedContext::ECMA_CONTEXT_SETUP(sctx)?;
        let mut agent_obj = sctx.create_base_object().map_err(|_| -1)?;

        let f = sctx
            .create_native_function(
                Callback::Func(child_receive_broadcast),
                "receiveBroadcast",
                0,
                0,
            )
            .map_err(|_| -1)?;
        agent_obj
            .set_property_str(sctx, "receiveBroadcast", f.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        let f = sctx
            .create_native_function(Callback::Func(child_report), "report", 0, 0)
            .map_err(|_| -1)?;
        agent_obj
            .set_property_str(sctx, "report", f.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        let f = sctx
            .create_native_function(Callback::Func(child_sleep), "sleep", 0, 0)
            .map_err(|_| -1)?;
        agent_obj
            .set_property_str(sctx, "sleep", f.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        let f = sctx
            .create_native_function(Callback::Func(child_leaving), "leaving", 0, 0)
            .map_err(|_| -1)?;
        agent_obj
            .set_property_str(sctx, "leaving", f.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        let f = sctx
            .create_native_function(Callback::Func(child_monotonic_now), "monotonicNow", 0, 0)
            .map_err(|_| -1)?;
        agent_obj
            .set_property_str(sctx, "monotonicNow", f.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        // Expose to the agent script both as the global `agent` and as
        // `$262.agent` (test262 agent scripts reference $262.agent.*). Both point
        // directly at `agent_obj` so that `agent.report` / `$262.agent.report`
        // both resolve to the native functions.
        let mut g = sctx.global_obj();
        g.set_property_str(sctx, "agent", agent_obj.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        let mut obj262b = sctx.create_base_object().map_err(|_| -1)?;
        obj262b
            .set_property_str(sctx, "agent", agent_obj.into())
            .map(|_| ())
            .map_err(|_| -1)?;
        g.set_property_str(sctx, "$262", obj262b.into())
            .map(|_| ())
            .map_err(|_| -1)?;

        // Keep the event loop polling while an `Atomics.waitAsync` Promise is
        // outstanding (its worker thread will eventually settle it via notify
        // or timeout). This ensures the agent's `$DONE`/report fires instead of
        // being left pending.
        sctx.add_tick_handler(|cx| cx.rt().has_pending_wait_async());

        Ok(())
    }) {
        Ok(ctx) => ctx,
        Err(_) => return,
    };

    // Signal this agent's `agent.start` call that the JS context is up and its
    // globals are installed, so the parent may safely broadcast / touch the
    // shared buffer. Raised before evaluating the source so `start` returns as
    // soon as the agent can actually run — regardless of whether the script ever
    // calls `receiveBroadcast`.
    READY_SIG.with(|s| {
        if let Some(sig) = s.borrow().clone() {
            let (lock, cvar) = &*sig;
            *lock.lock().unwrap() = true;
            cvar.notify_one();
        }
    });

    // Evaluate the agent source. The script normally calls
    // $262.agent.receiveBroadcast(...), which blocks this thread until the
    // parent broadcasts, then runs the agent's callback synchronously.
    let _ = ctx.eval(
        src.as_bytes(),
        &EvalName::Name("<agent>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    );

    // The broadcast callback may schedule asynchronous continuation microtasks
    // (e.g. an `await Atomics.waitAsync(...).value`, or `await`-ing a synchronously
    // resolved value in `null-for-timeout-agent.js`). Those run as promise
    // reactions that only execute when the event loop is driven. Since the agent
    // runs on a plain OS thread (no tokio executor), drive the event loop here.
    //
    // We keep draining until the agent signals `$262.agent.leaving()` (i.e. its
    // script finished and all `report()` calls fired) or a bounded timeout. This
    // guarantees an `await` continuation that produces further microtasks keeps
    // being serviced until the agent is truly done — a single `drain_jobs()` could
    // miss reactions scheduled by an earlier reaction. For waitAsync waiters, the
    // worker thread wakes the waiter *after* the parent's `Atomics.notify`, pushes
    // a resolve job, and the loop below services it; `leaving()` is called once the
    // agent's script completes.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        rt.drain_jobs();
        let done = thread_hub().is_leaving();
        if done {
            // Flush any trailing jobs one last time, then stop.
            rt.drain_jobs();
            break;
        }
        if std::time::Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

// Child-side native functions (non-capturing; use agents_hub()).

fn child_receive_broadcast(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    // NOTE: the per-agent readiness signal that releases the parent's
    // `agent.start` call is raised in `agent_thread` once the agent's JS context
    // is fully set up (globals installed), *before* the source is evaluated — not
    // here. This way `agent.start` returns as soon as the agent can run, whether
    // or not it ever calls `receiveBroadcast` (some tests synchronize purely via
    // Atomics on a SharedArrayBuffer and never touch the broadcast channel).
    let b = match thread_hub().wait_broadcast() {
        // Teardown: the parent has torn down the hub without a broadcast. Exit
        // cleanly so the agent thread does not leak into the next test.
        None => return Ok(JSValue::UNDEFINED),
        Some(b) => b,
    };

    // Per test262, the callback receives the broadcast *message* directly. When
    // the message is a SAB (as with safeBroadcast), the child gets a SAB view
    // over the shared backing. Otherwise it receives the string message.
    let value = if let Some(data) = b.sab {
        let ab = ArrayBuffer::from_shared(ctx, data).map_err(|_| -1)?;
        ab.into()
    } else if let Some(msg) = b.msg {
        to_js_string(ctx, &msg)?
    } else {
        JSValue::UNDEFINED
    };

    // Signal the parent that the agent has *received* the broadcast, so
    // `$262.agent.broadcast`/`safeBroadcast` does not wait for the callback to
    // finish running. test262's broadcast is asynchronous: the parent proceeds
    // immediately and relies on `waitUntil`/`tryYield` (which poll the shared
    // buffer) to ensure the agent has started before `Atomics.notify`. If we
    // waited for the callback to complete, a callback that blocks in the
    // synchronous `Atomics.wait` (e.g. `notify-one.js`, `was-woken-before-timeout.js`)
    // would stall the parent until the wait's timeout, so a subsequent
    // `Atomics.notify` would find no live waiter and wrongly return 0.
    //
    // The signal is raised *before* invoking the callback, but after the agent
    // has the shared-buffer view in hand. For `Atomics.waitAsync` callbacks
    // (non-blocking), the waiter is registered synchronously right after the
    // callback's `Atomics.add(i32a, RUNNING, 1)`, so the parent's subsequent
    // `waitUntil(RUNNING, …)` + `tryYield` still guarantees the waiter is live
    // before `Atomics.notify`.
    if let Some(consumed) = b.consumed.as_ref() {
        let (lock, cvar) = &**consumed;
        *lock.lock().unwrap() = true;
        cvar.notify_one();
    }

    // Invoke the report callback (if any) with the broadcast value.
    let cb = cc.arg(0);
    if !cb.is_undefined()
        && !cb.is_null()
        && let Ok(cb_obj) = Object::try_from(cb)
    {
        let _ = cb_obj.call(ctx, JSValue::UNDEFINED, &[value]);
    }

    Ok(JSValue::UNDEFINED)
}

fn child_report(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    // test262's `$262.agent.report` sends the value as a string (the parent's
    // `getReportAsync` returns a string). Tests rely on this: e.g.
    // `value-not-equal-agent.js` asserts the report equals `'42'`, and
    // `no-spurious-wakeup-on-add.js` compares a reported duration with `>=`
    // (which coerces the numeric string back to a number). Preserve string
    // semantics exactly.
    let rep = cc
        .arg(0)
        .to_string(ctx)
        .map_err(|_| -1)?
        .encode_utf8(false)
        .to_string();
    thread_hub().report(rep);
    Ok(JSValue::UNDEFINED)
}

fn child_sleep(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let ms = cc.arg(0).to_number(ctx).ok().map(|n| n.to_f64());
    if let Some(ms) = ms
        && ms > 0.0
    {
        std::thread::sleep(std::time::Duration::from_secs_f64(ms / 1000.0));
    }
    Ok(JSValue::UNDEFINED)
}

fn child_leaving(_ctx: &mut ScopedContext, _cc: CallContext) -> Result<JSValue, i32> {
    thread_hub().leaving();
    Ok(JSValue::UNDEFINED)
}

fn child_monotonic_now(_ctx: &mut ScopedContext, _cc: CallContext) -> Result<JSValue, i32> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    Ok(JSValue::from(now as f64))
}
