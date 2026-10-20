// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The compile queue's admission policy (P0-3, plan §13.6.5 item 4).
//!
//! A full queue used to drop the newcomer, first come first served. That
//! throws away a function that is *still hot* in favour of one that stopped
//! running long ago. The queue now drops a queued function that has not run
//! since it was queued and lets the newcomer take its place; when every queued
//! function is still running, the newcomer is the coldest one and is dropped.
//!
//! The test drives the storm by hand: `auto_poll(false)` keeps the queue
//! filling, so the admission decision is the only thing that decides which
//! function is compiled.

#![cfg(feature = "jit")]

use crate::common::settle;

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, JSResult, Jit, JitConfig, JitMetrics, Runtime,
    ScopedContext,
};

/// Three functions that go hot and then stop, and one that goes hot last.
const DEFINE: &str = "\
function coldA(){ return 1; }
function coldB(){ return 2; }
function coldC(){ return 3; }
function hot(){ var s = 0; for (var i = 0; i < 100; i++) s = (s + i) | 0; return s; }";

/// The storm: the three cold functions cross the gate and are not called
/// again, then `hot` crosses it.
const STORM: &str = "\
coldA(); coldB(); coldC();
coldA(); coldB(); coldC();
coldA(); coldB(); coldC();
hot();";

/// The observed run: only the hot function is called.
const RUN: &str = "\
var s = 0; for (var i = 0; i < 100; i++) s += hot(); s;";

fn eval(ctx: &mut Context, name: &EvalName, src: &str) -> JSResult {
    ctx.eval(src.as_bytes(), name, EvalType::Global, EvalFlags::empty())
}

/// A queue of two entries and a low gate, so the storm overflows it. The
/// automatic poll is off: the queue must fill up before anything is compiled.
fn config() -> JitConfig {
    JitConfig::builder()
        .call_threshold(2)
        .loop_threshold(2)
        .max_queue_len(2)
        .auto_poll(false)
        .build()
}

struct StormRun {
    metrics: JitMetrics,
}

/// Run the storm and report the metrics. The compile happens in `settle`,
/// after the whole storm has been offered to the queue.
fn storm() -> StormRun {
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let jit = Jit::attach_to(&rt, config()).expect("attach");
    let name = EvalName::Name("jit-queue.js".into());

    eval(&mut ctx, &name, DEFINE).expect("define");
    eval(&mut ctx, &name, STORM).expect("storm");
    // Everything the storm queued is compiled now.
    settle(&jit);
    // The observed run: only the hot function runs, and nothing compiles
    // any more (the automatic poll is off), so a native entry can only come
    // from a function that was compiled above.
    eval(&mut ctx, &name, RUN).expect("run");

    StormRun {
        metrics: jit.metrics(),
    }
}

/// The storm overflows the queue, and the hot function — the one that is
/// still running — is the one that gets compiled.
#[test]
fn a_hot_function_survives_a_full_queue() {
    let run = storm();
    assert!(
        run.metrics.queue_drops > 0,
        "the storm did not overflow the queue: {:?}",
        run.metrics
    );
    assert!(
        run.metrics.native_entries > 0,
        "the hot function was not compiled: {:?}",
        run.metrics
    );
}

/// The compile cost is measured: the metrics carry the time the attempts
/// took, so a host can tell whether the compile budget is worth it (P0-1).
#[test]
fn the_compile_cost_is_measured() {
    let run = storm();
    assert!(
        run.metrics.compile_attempts > 0,
        "no compile attempt was measured: {:?}",
        run.metrics
    );
    assert!(
        run.metrics.compile_nanos > 0,
        "the compile time is zero: {:?}",
        run.metrics
    );
    assert_eq!(
        run.metrics.compile_histogram.iter().sum::<u64>(),
        run.metrics.compile_attempts,
        "every attempt lands in exactly one bucket"
    );
}
