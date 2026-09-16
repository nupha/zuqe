// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>
//
// deepnest — measure the maximum nesting depth the zuqe parser can handle
// before its C-stack recursion guard fires.
//
// The parser installs a stack-overflow check at `next_token` (and at the
// function-definition entry point). When the native stack pointer drops more
// than `stack_overflow` bytes below where the runtime was captured, parsing
// returns a `SyntaxError` ("stack overflow") instead of crashing the process.
// See src/runtime/rt.rs (`STACK_OVERFLOW_LIMIT`, `set_stack_overflow`) and
// src/parse/tokenize.rs / src/parse/function.rs (`js_check_stack_overflow`).
//
// By binary-searching the deepest `if` chain that still parses, we get a
// concrete "bytes per nesting level" number. Comparing it before vs. after a
// parser change (e.g. inlining `stmt_if`/`stmt_while`/`stmt_do` with
// `#[inline(always)]`) quantifies the stack saving — no more hand-wavy claims.
//
// Usage:
//   cargo run --example deepnest                 # default 512 KiB limit
//   cargo run --example deepnest 262144          # 256 KiB limit
//   cargo run --example deepnest 262144 --max 50000
//
// Run the two limits (or before/after a change) and compare the reported
// "bytes per nesting level".

use std::time::Instant;

use zuqe::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};

/// Build `if(1)if(1)...if(1){var x=1;}` — `n` levels of `if` nesting.
/// Condition is the literal `1` and the body is `var x=1;`, so the program is
/// valid at runtime and any `Err` from `eval` is a *parse* stack overflow.
fn build_deep_if(n: usize) -> String {
    let mut s = String::with_capacity(n * 6 + 16);
    for _ in 0..n {
        s.push_str("if(1)");
    }
    s.push_str("{var x=1;}");
    s
}

/// Returns true if a chain of `n` nested `if`s parses (and runs) successfully
/// under the given stack-overflow limit.
fn probe(limit: usize, n: usize) -> bool {
    let mut rt = match Runtime::create() {
        Ok(rt) => rt,
        Err(_) => return false,
    };
    #[cfg(debug_assertions)]
    rt.install_panic_handler();

    let mut ctx = match rt.create_context(ScopedContext::ECMA_CONTEXT_SETUP) {
        Ok(ctx) => ctx,
        Err(_) => return false,
    };
    // Set the C-stack recursion guard, then let eval refresh stack_top for us.
    ctx.rt_mut().set_stack_budget(limit);

    let src = build_deep_if(n);
    ctx.eval(
        src.as_bytes(),
        &EvalName::Name("<deepnest>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .is_ok()
}

/// Find the maximum nesting depth that still parses, via exponential + binary
/// search. `max_n` caps the probe range (a sanity bound so we never build or
/// test absurdly large sources).
fn find_max_depth(limit: usize, max_n: usize) -> usize {
    // Exponential search for an upper bound that fails.
    let mut hi = 1usize;
    while hi <= max_n && probe(limit, hi) {
        hi *= 2;
    }
    if hi > max_n {
        // Even the maximum probe depth parses fine.
        return max_n;
    }
    // `lo` is known to pass, `hi` is known to fail.
    let mut lo = hi / 2;
    while lo + 1 < hi {
        let mid = lo + (hi - lo) / 2;
        if probe(limit, mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut limit = 512 * 1024;
    let mut max_n = 200_000usize;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--max" => {
                if i + 1 < args.len() {
                    if let Ok(v) = args[i + 1].parse::<usize>() {
                        max_n = v;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            s => {
                if let Ok(v) = s.parse::<usize>() {
                    limit = v;
                }
                i += 1;
            }
        }
    }

    println!(
        "deepnest: stack limit = {} bytes ({} KiB), max probe depth = {}",
        limit,
        limit / 1024,
        max_n
    );

    let start = Instant::now();
    let depth = find_max_depth(limit, max_n);
    let elapsed = start.elapsed();

    println!("  max nesting depth before stack overflow = {}", depth);
    println!("  search took {:?}", elapsed);

    if depth >= max_n {
        println!(
            "  (no overflow within probe range; frames per level < {:.1} bytes)",
            limit as f64 / max_n as f64
        );
    } else {
        println!(
            "  => approx {:.1} bytes per nesting level (limit / depth)",
            limit as f64 / depth as f64
        );
    }
}
