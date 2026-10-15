// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Interpreter vs native timing for the iterative-fibonacci numeric loop.
//!
//! Run with: `cargo run --release --features jit --example jit-bench`.
//! The interpreter column is a separate, JIT-free runtime; the native column
//! warms up once (so the function is compiled and installed) before timing.

use std::time::{Duration, Instant};

use zuqe::{Context, EvalFlags, EvalName, EvalType, Jit, JitConfig, Runtime, ScopedContext};

const DEFINE: &[u8] =
    b"function fib(n){ var a = 0, b = 1; for (var i = 0; i < n; i++){ var t = a + b; a = b; b = t; } return b; } fib(1);";
const WORK: &str = "var s = 0; for (var k = 0; k < 4000; k++) s = fib(30); s;";

fn eval(ctx: &mut Context, src: &[u8]) -> i32 {
    ctx.eval(
        src,
        &EvalName::Name("jit-bench.js".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap()
    .as_int()
    .unwrap()
}

fn time_repeated(ctx: &mut Context, rounds: usize) -> Duration {
    let start = Instant::now();
    for _ in 0..rounds {
        std::hint::black_box(eval(ctx, WORK.as_bytes()));
    }
    start.elapsed()
}

fn main() {
    let rounds = 5;

    // Interpreter baseline.
    let mut base_rt = Runtime::create().unwrap();
    let mut base_ctx = base_rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    eval(&mut base_ctx, DEFINE);
    let interpreter = time_repeated(&mut base_ctx, rounds);

    // Native: compile first, then time the same work.
    let mut jit_rt = Runtime::create().unwrap();
    let mut jit_ctx = jit_rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(
        &jit_rt,
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .build(),
    )
    .unwrap();
    eval(&mut jit_ctx, DEFINE);
    jit.poll();
    let metrics = jit.metrics();
    assert!(metrics.functions_compiled > 0, "fib was not compiled");
    // Warm the native entry, then time.
    std::hint::black_box(eval(&mut jit_ctx, WORK.as_bytes()));
    let native = time_repeated(&mut jit_ctx, rounds);

    let ratio = interpreter.as_secs_f64() / native.as_secs_f64();
    println!("interpreter: {:>10.3?}", interpreter / rounds as u32);
    println!("native:      {:>10.3?}", native / rounds as u32);
    println!(
        "speedup:     {ratio:.2}x  (native is {:.0}% faster)",
        (ratio - 1.0) * 100.0
    );
    println!(
        "metrics: compiled={} native_entries={} verified={} rejected={}",
        metrics.functions_compiled,
        jit.metrics().native_entries,
        metrics.functions_verified,
        metrics.functions_rejected
    );
}
