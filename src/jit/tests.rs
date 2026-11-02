use super::*;

/// The record-table key must not be the GC address.
///
/// gc-lite reuses freed node addresses, so two functions that never
/// coexisted can share one. With an address key the second would inherit
/// the first one's record — its heat, its tier, and an `Installed` or
/// `Settled` lifecycle that silently stops it from ever being compiled.
/// A serial is stable for the function and never repeats.
#[test]
fn a_function_keeps_its_id_and_ids_do_not_repeat() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let name = EvalName::Name("jit-serial.js".into());
    let bytecode_of = |ctx: &mut crate::Context, src: &[u8]| {
        let value = ctx
            .eval(src, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        let bfun = crate::function::BCWrapper::try_from(value).expect("a function");
        bfun.payload().bytecode()
    };

    let first = bytecode_of(&mut ctx, b"function f(){} f;");
    let second = bytecode_of(&mut ctx, b"function g(){} g;");

    assert_eq!(
        function_id(&first),
        function_id(&first),
        "a function's id must not change"
    );
    assert_ne!(
        function_id(&first),
        function_id(&second),
        "two functions must not share an id"
    );

    // The same bytecode reached through a different handle keeps its id
    // (the serial lives on the function, not on the access path).
    let again = bytecode_of(&mut ctx, b"f;");
    assert_eq!(
        function_id(&again),
        function_id(&first),
        "re-entering the same function changed its id"
    );
    assert_ne!(
        function_id(&again),
        function_id(&second),
        "the two functions still must not collide"
    );
}

/// `poll` is where the record table gets swept: the threshold only ever
/// changes inside a sweep, so one poll after the table reached it proves the
/// sweep is wired in (a missing call would leave the table growing for the
/// runtime's lifetime).
#[test]
fn poll_prunes_the_record_table() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    // Nothing may compile: the threshold is out of reach, so the run only
    // builds records (one per function).
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1_000_000)
            .loop_threshold(1_000_000)
            .build(),
    )
    .unwrap();

    let mut src = String::new();
    for i in 0..1500 {
        src.push_str(&format!("function f{i}(x){{ return x + {i}; }} f{i}(1);"));
    }
    ctx.eval(
        src.as_bytes(),
        &EvalName::Name("jit-prune.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();

    let state = rt.jit().unwrap();
    let before = state.coordinator.borrow().record_len();
    assert!(
        before >= crate::jit::runtime::MIN_RECORDS,
        "the test must fill the table first (records: {before})"
    );
    assert_eq!(
        state.coordinator.borrow().prune_at(),
        crate::jit::runtime::MIN_RECORDS,
        "no sweep may have run yet"
    );

    jit.poll();

    let after = state.coordinator.borrow().record_len();
    assert_eq!(
        after, before,
        "every record ran in this epoch, so none may be dropped"
    );
    assert!(
        state.coordinator.borrow().prune_at() > crate::jit::runtime::MIN_RECORDS,
        "poll did not sweep the record table"
    );
}

/// The record table must not grow with `eval` churn: a host that keeps
/// evaluating short-lived functions used to accumulate one record per
/// distinct function for the whole runtime lifetime. Idle records are
/// swept, so the table stays around the live set.
#[test]
fn the_record_table_does_not_grow_with_eval_churn() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    // Nothing compiles: this is a table-growth test, not a compile test.
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1_000_000)
            .loop_threshold(1_000_000)
            .build(),
    )
    .unwrap();
    let state = rt.jit().unwrap();

    const ROUNDS: usize = 250;
    const PER_ROUND: usize = 80;
    let mut total = 0usize;
    let mut peak = 0usize;
    for round in 0..ROUNDS {
        let mut src = String::new();
        for i in 0..PER_ROUND {
            src.push_str(&format!(
                "function f{i}(x){{ return x + {i}; }} f{i}({round});"
            ));
        }
        ctx.eval(
            src.as_bytes(),
            &EvalName::Name("jit-churn.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
        total += PER_ROUND;
        // The host's frame boundary: the sweep runs here.
        jit.poll();
        peak = peak.max(state.coordinator.borrow().record_len());
    }

    let tracked = state.coordinator.borrow().record_len();
    assert!(
        peak < crate::jit::runtime::MIN_RECORDS * 2,
        "{total} functions went through the JIT but the table peaked at {peak} records \
             (tracked now: {tracked})"
    );
}

/// The layout contract the array guard reads, end to end: a
/// `JSValue` holds the **GC node** pointer, and the offsets in
/// `object::jit_layout` are relative to *that*. Reading the fields at the
/// node pointer must land on the flags word and the payload — the mistake
/// here does not fault, it just makes the guard miss forever (which looks
/// like "the inline guard bought nothing").
#[test]
fn the_array_layout_contract_holds_from_a_js_value() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext, jit::layout::object as jit_layout};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let name = EvalName::Name("jit-layout.js".into());
    let value = ctx
        .eval(
            b"globalThis.arr = [11, 22, 33]; arr;",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
    let object = value.as_object().expect("an array");
    // The node pointer: what a `JSValue` actually holds.
    let node = value.as_gc_node().expect("a GC node").as_ptr() as usize;

    // The flags word: the class id and the fast-array flag.
    let flags =
        unsafe { *((node as *const u8).add(jit_layout::FLAGS_OFFSET as usize) as *const u32) };
    assert_eq!(
        flags as i64 & jit_layout::CLASS_ID_MASK,
        i64::from(crate::JS_CLASS_ARRAY),
        "the flags word is not where the JIT expects it"
    );
    assert_ne!(
        flags as i64 & jit_layout::FAST_ARRAY_MASK,
        0,
        "the fast-array flag is not where the JIT expects it"
    );

    // The payload and the elements it holds.
    let payload =
        unsafe { *((node as *const u8).add(jit_layout::PAYLOAD_OFFSET as usize) as *const u64) };
    assert_ne!(
        payload, 0,
        "the payload pointer is not where the JIT expects it"
    );
    let data = unsafe {
        *((payload as *const u8).add(crate::jit::layout::array::DATA_OFFSET as usize) as *const u64)
    };
    let len = unsafe {
        *((payload as *const u8).add(crate::jit::layout::array::LEN_OFFSET as usize) as *const u32)
    };
    assert_eq!(
        len, 3,
        "the elements' length is not where the JIT expects it"
    );
    let second = unsafe { *((data as *const u8).add(8) as *const u64) };
    let third = unsafe { *((data as *const u8).add(16) as *const u64) };
    assert_eq!(second, object.payload_array().unwrap().values()[1].0);
    assert_eq!(third, object.payload_array().unwrap().values()[2].0);
}

/// The typed array's layout contract, end to end (the property inline guard): the view's
/// buffer, the element count and the buffer's bytes must be where the
/// generated code reads them.
#[test]
fn the_typed_array_layout_contract_holds_from_a_js_value() {
    use crate::{
        EvalFlags, EvalName, EvalType, ScopedContext,
        jit::layout::{
            array_buffer as buffer_layout, object as jit_layout, typed_array as ta_layout,
        },
    };

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let name = EvalName::Name("jit-layout-ta.js".into());
    let value = ctx
        .eval(
            b"globalThis.ta = new Int32Array([7, 8, 9]); ta;",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
    let object = value.as_object().expect("a typed array");
    let node = value.as_gc_node().expect("a GC node").as_ptr() as usize;

    // The typed array's payload: the buffer and the element count.
    let payload =
        unsafe { *((node as *const u8).add(jit_layout::PAYLOAD_OFFSET as usize) as *const u64) };
    let buffer =
        unsafe { *((payload as *const u8).add(ta_layout::BUFFER_OFFSET as usize) as *const u64) };
    let count =
        unsafe { *((payload as *const u8).add(ta_layout::COUNT_OFFSET as usize) as *const u32) };
    assert_eq!(
        count, 3,
        "the element count is not where the JIT expects it"
    );

    // The buffer's payload and the view of its bytes.
    let buf_payload =
        unsafe { *((buffer as *const u8).add(jit_layout::PAYLOAD_OFFSET as usize) as *const u64) };
    let data = unsafe {
        *((buf_payload as *const u8)
            .add((buffer_layout::VIEW_OFFSET + buffer_layout::VIEW_DATA_OFFSET) as usize)
            as *const u64)
    };
    let direct = unsafe {
        *((buf_payload as *const u8)
            .add((buffer_layout::VIEW_OFFSET + buffer_layout::VIEW_DIRECT_OFFSET) as usize)
            as *const u32)
    };
    assert_eq!(direct, 1, "the bytes are not directly readable");
    // The first element: an `i32` in the buffer.
    let first = unsafe { *((data as *const u8).cast::<i32>()) };
    assert_eq!(first, 7);
    assert_eq!(object.payload_typed_array().unwrap().count(), 3);
}

/// The layout contract the inline property guard reads, end to
/// end: the object's shape pointer, the shape's id, and the property
/// slots — read exactly the way the generated code reads them.
#[test]
fn the_property_layout_contract_holds_from_a_js_value() {
    use crate::{
        EvalFlags, EvalName, EvalType, ScopedContext,
        jit::layout::object as jit_layout,
        object::{JSProperty, PROP_TAG_NORMAL},
    };

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let name = EvalName::Name("jit-layout-prop.js".into());
    let value = ctx
        .eval(
            b"globalThis.o = { x: 42, y: 'hi' }; o;",
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
    let object = value.as_object().expect("an object");
    // The node pointer: what a `JSValue` actually holds.
    let node = value.as_gc_node().expect("a GC node").as_ptr() as usize;

    // The shape: the node pointer the object holds.
    let shape_node =
        unsafe { *((node as *const u8).add(jit_layout::SHAPE_OFFSET as usize) as *const u64) };
    let shape = object.shape().expect("a shape");
    assert_eq!(
        shape_node,
        shape.gc_ref().node_ptr().as_ptr() as u64,
        "the shape is not where the JIT expects it"
    );
    // The shape's payload pointer: what the inline cache records.
    let shape_payload = shape_node + jit_layout::SHAPE_PAYLOAD_OFFSET as u64;
    assert_eq!(
        shape_payload,
        unsafe { shape.gc_ref().as_ptr().as_ptr() as u64 },
        "the shape's payload is not where the JIT expects it"
    );
    // The shape's id.
    let uid = unsafe {
        *((shape_node as *const u8).add(jit_layout::SHAPE_UID_OFFSET as usize) as *const u32)
    };
    assert_eq!(
        uid, shape.uid,
        "the shape's id is not where the JIT reads it"
    );

    // The shape's cached property count — what the guard's range check
    // reads. It must equal the shape's actual prop defs.
    let count = unsafe {
        *((shape_node as *const u8).add(jit_layout::SHAPE_PROP_COUNT_OFFSET as usize) as *const u32)
    };
    assert_eq!(
        count as usize,
        shape.len(),
        "the shape's cached count drifted from its prop defs"
    );
    let slots =
        unsafe { *((node as *const u8).add(jit_layout::PROP_DATA_OFFSET as usize) as *const u64) };
    // The first slot: the tag and the value it holds.
    let slot = slots as *const u8;
    let tag = unsafe { *slot.add(JSProperty::tag_offset() as usize) };
    assert_eq!(
        tag, PROP_TAG_NORMAL,
        "the slot's tag is not where the JIT reads it"
    );
    let raw = unsafe {
        *slot
            .add(JSProperty::normal_value_offset() as usize)
            .cast::<u64>()
    };
    assert_eq!(
        raw,
        object
            .prop_slot(0)
            .value(&mut ctx.root_scope(), value)
            .unwrap()
            .0,
        "the slot's value is not where the JIT reads it"
    );
}

/// Bring the JIT to rest before asserting on compile results. `poll` is
/// non-blocking, so spin until no attempt is in flight and the queue is
/// empty.
fn settle(jit: &Jit) {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        jit.poll();
        let m = jit.metrics();
        if m.queue_len == 0 && m.functions_compiling == 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the compiler workers never settled: {m:?}"
        );
        // The worker compiles in the background; give it the CPU.
        std::thread::sleep(Duration::from_micros(50));
    }
}

#[test]
fn attach_reports_metrics_and_target() {
    let rt = crate::Runtime::create().unwrap();
    let jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
    assert_eq!(jit.metrics(), JitMetrics::default());
    assert!(jit.target_isa().is_some());
}

#[test]
fn second_attach_is_rejected() {
    let rt = crate::Runtime::create().unwrap();
    let _jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
    assert!(matches!(
        Jit::attach_to(&rt, JitConfig::default()),
        Err(JitError::AlreadyAttached)
    ));
}

#[test]
fn poll_counts() {
    let rt = crate::Runtime::create().unwrap();
    let jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
    jit.poll();
    jit.poll();
    assert_eq!(jit.metrics().polls, 2);
}

#[test]
fn attached_jit_observes_runs_without_changing_results() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let src = b"var s = 0; for (var i = 0; i < 200; i++) s += i; s;";
    let name = EvalName::Name("jit-probe.js".into());

    let before = ctx
        .eval(src, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();

    let jit = Jit::attach_to(&rt, JitConfig::default()).unwrap();
    let after = ctx
        .eval(src, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();

    assert_eq!(before.as_int(), after.as_int());
    let m = jit.metrics();
    assert!(m.function_entries > 0, "entry hook fired");
    assert!(m.backedges > 0, "backedge hook fired");
}

#[test]
fn poll_verifies_every_queued_function_exactly_once() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .build(),
    )
    .unwrap();

    // A script with several functions and a loop, so both entry and
    // backedge thresholds fire and at least one function is rejected
    // (`for...of`).
    let src = br#"
            function add(a, b) { return a + b; }
            function loop() { var s = 0; for (var i = 0; i < 100; i++) s = add(s, i); return s; }
            function iter() { var s = 0; for (var v of [1, 2, 3]) s += v; return s; }
            loop() + iter();
        "#;
    ctx.eval(
        src,
        &EvalName::Name("jit-verify-probe.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();

    settle(&jit);
    let m = jit.metrics();
    assert!(m.functions_queued > 0, "nothing queued");
    assert_eq!(
        m.functions_verified + m.functions_rejected,
        m.functions_queued,
        "every queued function must be accounted for"
    );
    assert!(m.functions_verified > 0, "nothing verified");
    assert_eq!(m.queue_len, 0, "poll drained the queue");
}

/// A function waiting to compile must be handed to the GC root scan, or it
/// could be freed (and its `jit` slot with it) before `poll` installs the
/// result. This asserts the trace itself, deterministically.
#[test]
fn queued_functions_are_traced_as_gc_roots() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .auto_poll(false) // the test drives the enqueue → GC → poll window
            .build(),
    )
    .unwrap();

    ctx.eval(
        b"function f(a){ return a + 1; } f(1);",
        &EvalName::Name("jit-pin.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();
    assert!(jit.metrics().functions_queued > 0, "nothing queued");

    let mut gcx = ctx.gc_heap().create_trace_ctx(64);
    rt.jit().unwrap().trace(&mut gcx);
    assert!(
        !gcx.take_nodes().is_empty(),
        "a queued function was not traced as a GC root"
    );
}

/// End-to-end version of the same guarantee: collect *between* enqueue and
/// poll. Without the pin the function's bytecode (and slot) could be gone
/// by the time `poll` compiles; with it, poll still installs a valid entry.
#[test]
fn gc_between_enqueue_and_poll_still_installs() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .build(),
    )
    .unwrap();

    // The inner function becomes hot, then the IIFE returns and drops the
    // only reference to it.
    ctx.eval(
        b"(function(){ function f(a){ return a + 1; } return f(1); })();",
        &EvalName::Name("jit-pin-gc.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();
    let queued = jit.metrics().functions_queued;
    assert!(queued > 0, "nothing queued");

    ctx.with_root_scope(|mut cx| cx.rt_mut().gc_collect());

    settle(&jit);
    let m = jit.metrics();
    assert_eq!(m.queue_len, 0, "poll drained the queue");
    assert!(
        m.functions_compiled > 0,
        "nothing compiled from the pinned snapshot (queued={queued})"
    );
}

/// M2b-8: when the platform refuses executable memory the attempt must
/// fail, be retried once, settle the function in the interpreter — and the
/// observable result must not change. This is the fault-injection half of
/// the hardening gate (generated code cannot run under Miri).
///
/// The fault switch is thread-local and `publish` allocates on the runtime
/// thread, so it stays armed while the workers lower the function.
#[test]
fn allocation_failure_falls_back_to_the_interpreter() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    const DEFINE: &[u8] =
        b"function f(n){ var s = 0; for (var i = 0; i < n; i++) s = (s + i) | 0; return s; } f(1);";
    const CALL: &[u8] = b"f(500)";
    let name = EvalName::Name("jit-fault.js".into());

    let mut base_rt = crate::Runtime::create().unwrap();
    let mut base_ctx = base_rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    base_ctx
        .eval(DEFINE, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    let expected = base_ctx
        .eval(CALL, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .max_compile_attempts(1)
            .auto_poll(false) // inject the fault before the first compile
            .build(),
    )
    .unwrap();
    ctx.eval(DEFINE, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();

    platform::fault::set(true);
    settle(&jit);
    platform::fault::set(false);

    let m = jit.metrics();
    assert_eq!(
        m.functions_compiled, 0,
        "code was published with no executable memory"
    );
    assert!(
        m.compile_failures > 0,
        "the refused mapping was not recorded"
    );
    assert!(
        m.functions_settled > 0,
        "the function did not settle in the interpreter"
    );
    assert_eq!(m.code_bytes, 0, "code bytes leaked despite the fault");

    let got = ctx
        .eval(CALL, &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    assert_eq!(
        got.as_int(),
        expected.as_int(),
        "the fallback changed the result"
    );
}

/// The code cache quota refuses a publish the same way; the quota test
/// above and this one together cover both failure entries of `publish`.
#[test]
fn code_cache_quota_refuses_then_settles_after_retries() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    // One byte of code budget: every publish is refused.
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .max_code_bytes(1)
            .build(),
    )
    .unwrap();

    ctx.eval(
        b"function f(a){ return a + 1; } f(1);",
        &EvalName::Name("jit-quota.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();

    settle(&jit);
    let m = jit.metrics();
    assert_eq!(m.queue_len, 0, "poll drained the queue");
    assert_eq!(m.functions_compiled, 0, "nothing fits the quota");
    assert!(m.compile_failures > 0, "the refused publish was recorded");
    assert!(m.functions_retried > 0, "the attempt was retried first");
    assert!(m.functions_settled > 0, "out of attempts -> settled");
}

/// Contention between the runtime thread and the workers: they compile
/// while the interpreter keeps running, while the GC traces and collects,
/// and while a tight code-cache quota retires live mappings (dropping them
/// through `invalidate`) — with several workers and host polls interleaved.
///
/// The design makes a data race impossible (only `Send`, pointer-free
/// snapshots cross; the runtime thread is the sole writer), so this test
/// checks the *logical* races those rules exist to prevent: a lost pin (the
/// GC frees a compiling function), a stale completion installed over a
/// retired mapping, or a record stranded in `Compiling`. Any of those shows
/// up as a wrong value, a crash, or a `settle` that never reaches quiet.
///
/// `auto_poll(false)` is deliberate: `poll` dispatches a batch, then a long
/// interpreted loop and a collection run **while** the workers are still
/// compiling, so the overlap is real rather than incidental.
#[test]
fn background_compilation_races_the_interpreter_gc_and_eviction() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    // A long, allocation-free loop that keeps the interpreter busy while the
    // workers lower the round's functions.
    const CHURN: &[u8] = b"var s = 0; for (var i = 0; i < 100000; i++) s = (s + (i & 7)) | 0; s;";
    const CHURN_SUM: i32 = 12_500 * 28; // (100000 / 8) * (0+1+..+7)

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    // Four workers, eager thresholds, two code pages (installs retire each
    // other), and no automatic poll so the overlap is explicit.
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .workers(4)
            .max_code_bytes(2 * 4096)
            .auto_poll(false)
            .build(),
    )
    .unwrap();
    let name = EvalName::Name("jit-race.js".into());

    for round in 0..20i32 {
        // Six fresh functions per round: the cache must retire the previous
        // rounds' mappings while this round's compiles are in flight.
        let mut define = String::new();
        for k in 0..6 {
            define.push_str(&format!(
                    "function f{round}_{k}(n){{ var s = 0; for (var i = 0; i < n; i++) s = (s + i * {} + {k}) | 0; return s; }}",
                    round + 1
                ));
        }
        define.push_str(&format!("f{round}_0(300); f{round}_1(300);"));
        ctx.eval(
            define.as_bytes(),
            &name,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();

        // Dispatch the batch, then run the long loop and collect **while**
        // the workers are still lowering.
        jit.poll();
        let churn = ctx
            .eval(CHURN, &name, EvalType::Global, EvalFlags::empty())
            .unwrap();
        assert_eq!(
            churn.as_int(),
            Some(CHURN_SUM),
            "round {round}: the interpreter changed under contention"
        );
        ctx.with_root_scope(|mut cx| cx.rt_mut().gc_collect());
        jit.poll();

        let got = ctx
            .eval(
                format!("f{round}_0(300)").as_bytes(),
                &name,
                EvalType::Global,
                EvalFlags::empty(),
            )
            .unwrap();
        let expected: i32 = (0..300).map(|i| (i * (round + 1))).sum::<i32>();
        assert_eq!(
            got.as_int(),
            Some(expected),
            "round {round} diverged while compiles were in flight"
        );
    }

    settle(&jit);
    let m = jit.metrics();
    assert_eq!(
        (m.queue_len, m.functions_compiling),
        (0, 0),
        "an attempt was stranded by the contention ({m:?})"
    );
    assert!(
        m.code_bytes <= 2 * 4096,
        "the quota was exceeded under churn ({m:?})"
    );
}

/// Single-owner retirement end to end: with a budget of one code page, compiling a
/// second function retires the first live mapping instead of refusing the
/// compile. The retired function must fall back to the interpreter and
/// still return the right answer (a stale entry would be a UAF, not a
/// wrong result, so this test's real value is that it does not crash).
#[test]
fn the_code_cache_retires_a_live_mapping_under_a_tight_quota() {
    use crate::{EvalFlags, EvalName, EvalType, ScopedContext};

    let mut rt = crate::Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    // One page (`CodeMem` rounds every mapping up to a page): one function.
    let jit = Jit::attach_to(
        &rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(JitTierPolicy::Tier1)
            .max_code_bytes(4096)
            .build(),
    )
    .unwrap();
    let name = EvalName::Name("jit-retire.js".into());

    ctx.eval(
        b"function a(n){ var s = 0; for (var i = 0; i < n; i++) s += i; return s; } a(10);",
        &name,
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();
    settle(&jit);
    let after_a = jit.metrics();
    assert!(after_a.functions_compiled >= 1, "a was not compiled");
    assert!(after_a.code_bytes <= 4096, "a exceeded the quota");

    ctx.eval(
        b"function b(n){ var s = 1; for (var i = 0; i < n; i++) s = s + 1; return s + n; } b(10);",
        &name,
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();
    settle(&jit);
    let after_b = jit.metrics();
    assert!(
        after_b.functions_compiled > after_a.functions_compiled,
        "b was not compiled; the quota refused it instead of retiring a"
    );
    assert!(
        after_b.code_bytes <= 4096,
        "the retired mapping was not reclaimed ({after_b:?})"
    );
    assert!(
        after_b.code_retired >= 1,
        "the retirement was not counted ({after_b:?})"
    );
    assert!(
        after_b.code_never_entered >= 1,
        "compiled code that never ran should be counted ({after_b:?})"
    );

    // `a`'s code is gone; running it again must not call the freed mapping.
    let got = ctx
        .eval(b"a(100)", &name, EvalType::Global, EvalFlags::empty())
        .unwrap();
    assert_eq!(got.as_int(), Some(4950), "the retired function is wrong");
}
