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
use zuqe::{
    CallContext, Callback, EvalFlags, EvalName, EvalType, JSValue, Object, Runtime, ScopedContext,
};

#[derive(Clone)]
struct Broadcast {
    // shared memory backing, if the broadcast message is (or includes) a SAB
    sab: Option<Arc<RwLock<Box<[u8]>>>>,
    // textual message, if the broadcast carries a string
    msg: Option<String>,
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
    static CURRENT_HUB: RefCell<Option<Arc<Hub>>> = RefCell::new(None);
    static READY_SIG: RefCell<Option<Arc<(Mutex<bool>, Condvar)>>> = RefCell::new(None);
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
    if let Ok(ab) = ArrayBuffer::try_from(cc.arg(1)) {
        if let Some(data) = ab.shared_data() {
            sab = Some(data);
        }
    }

    agents_hub().current().broadcast(Broadcast { sab, msg });
    Ok(JSValue::UNDEFINED)
}

fn agent_get_report(ctx: &mut ScopedContext, _cc: CallContext) -> Result<JSValue, i32> {
    let r = agents_hub().current().get_report();
    to_js_string(ctx, &r)
}

fn agent_sleep(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let ms = cc.arg(0).to_number(ctx).ok().map(|n| n.to_f64());
    if let Some(ms) = ms {
        if ms > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(ms / 1000.0));
        }
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
    // (e.g. an `await Atomics.waitAsync(...).value` inside the agent callback in
    // `false-for-timeout-agent.js`). Those run as promise reactions that only
    // execute when the event loop is driven. Since the agent runs on a plain OS
    // thread (no tokio executor), drain them synchronously here so the agent's
    // `report()` actually fires before the thread exits.
    rt.drain_jobs();
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

    // Invoke the report callback (if any) with the broadcast value.
    let cb = cc.arg(0);
    if !cb.is_undefined() && !cb.is_null() {
        if let Ok(cb_obj) = Object::try_from(cb) {
            let _ = cb_obj.call(ctx, JSValue::UNDEFINED, &[value]);
        }
    }

    Ok(JSValue::UNDEFINED)
}

fn child_report(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let msg = cc
        .arg(0)
        .to_string(ctx)
        .map_err(|_| -1)?
        .encode_utf8(false)
        .to_string();
    thread_hub().report(msg);
    Ok(JSValue::UNDEFINED)
}

fn child_sleep(ctx: &mut ScopedContext, cc: CallContext) -> Result<JSValue, i32> {
    let ms = cc.arg(0).to_number(ctx).ok().map(|n| n.to_f64());
    if let Some(ms) = ms {
        if ms > 0.0 {
            std::thread::sleep(std::time::Duration::from_secs_f64(ms / 1000.0));
        }
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
