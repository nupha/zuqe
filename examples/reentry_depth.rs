// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>
//
// reentry_depth — measure how many bytes of *C stack* one native→JS reentry
// costs, i.e. `[0].forEach(cb)` where `cb` calls back into native code (which
// then starts the next native→JS round trip).
//
// Every such round trip re-enters the interpreter through
// `execute_trampoline` / `execute_with_arena`, so it eats a full C-stack
// frame chain. JS→JS recursion does not (the interpreter is stackless), which
// is why only this shape blows the thread stack. See the bug write-up in
// `bugs/native-reentry-cstack.md`.
//
// Method: probe the deepest `n` that still runs under a strict
// `thread_stack_limit` (measured from the eval-entry SP), at several limits,
// then least-squares fit depth vs. limit → "bytes per reentry level". The
// fixed eval overhead cancels in the fit. Same idea as `deepnest.rs`, which
// measures the parser instead.
//
// Usage:
//   cargo run --example reentry_depth                    # dev profile
//   cargo run --release --example reentry_depth
//   cargo run --example reentry_depth -- --gate 16384    # custom gate (bytes)
//   cargo run --example reentry_depth -- 512             # one limit only (KiB)
//
// The -O0 shape that *host* workspaces see (this repo's `[profile.dev]` does
// not apply to crates that depend on zuqe) — run it in a scratch target dir
// so it doesn't clobber the normal dev build:
//   PowerShell:
//     $env:CARGO_PROFILE_DEV_OPT_LEVEL = "0"
//     mbx build --example reentry_depth --target-dir target\opt0
//     .\target\opt0\debug\examples\reentry_depth.exe
//
// Exit status: non-zero when the measured cost exceeds the gate, so it can be
// used as a regression check.

use std::time::Instant;

use zuqe::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};

/// Scratch stack for the probe thread.
///
/// Limits are *strict* (`set_thread_stack_limit`): the guard line sits that
/// many bytes below the eval-entry SP, so a limit larger than the real thread
/// stack would arm the guard below the OS guard page and never fire. A 64 MiB
/// probe thread lets us run 4 MiB limits, which keeps the depth readings in
/// the dozens (a few dozen bytes of quantisation error instead of a whole
/// level) — essential when one level is tens of KiB.
const PROBE_THREAD_STACK: usize = 64 * 1024 * 1024;

/// Default gate: 16 KiB of C stack per native→JS reentry level.
///
/// Current numbers (Windows x86_64): -O0 ≈ 70 KiB (fails), dev opt-level 1
/// ≈ 3.6 KiB, release ≈ 2.9 KiB (both pass). The gate is the Stage-1 target
/// for an -O0 interpreter split (`execute_with_arena` + `execute_trampoline`).
const DEFAULT_GATE: usize = 16 * 1024;

/// Build a script performing exactly `n` nested native→JS reentries.
///
/// `f(n)` recurses through `[0].forEach(...)`: each level runs one native
/// builtin that calls back into JS, so `n` levels = `n` interpreter reentries.
/// A `n`-level run either evaluates to `n + 1` or fails with the engine's
/// catchable "stack overflow" — never a hard crash, because the guard compares
/// the live SP against the armed limit.
fn build_reentry(n: usize) -> String {
    format!(
        "function f(k) {{\n\
         \x20 if (k <= 0) return 0;\n\
         \x20 var r = 0;\n\
         \x20 [0].forEach(function () {{ r = f(k - 1); }});\n\
         \x20 return r + 1;\n\
         }}\n\
         f({n});\n"
    )
}

/// Returns true if `n` nested reentry levels fit under `limit` bytes of stack.
///
/// A fresh runtime per probe keeps `thread_stack_top` anchored at this call
/// site, so the strict limit is measured from a reproducible point.
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
    // Strict mode: the guard line sits `limit` bytes below the eval-entry SP.
    ctx.rt_mut().set_thread_stack_limit(limit);

    ctx.eval(
        build_reentry(n).as_bytes(),
        &EvalName::Name("<reentry_depth>".into()),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .is_ok()
}

/// Maximum reentry depth that still fits under `limit`, via exponential then
/// binary search. `max_n` caps the probe range.
fn find_max_depth(limit: usize, max_n: usize) -> usize {
    let mut hi = 1usize;
    while hi <= max_n && probe(limit, hi) {
        hi *= 2;
    }
    if hi > max_n {
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

/// Least-squares slope of `depth` vs. `limit` → bytes per level.
///
/// depth = overhead + limit / bytes_per_level, so bytes_per_level = 1 / slope.
/// Only limits with depth ≥ 1 contribute (depth 0 would pin the line at the
/// fixed eval overhead and dominate the fit).
fn fit_bytes_per_level(points: &[(usize, usize)]) -> Option<f64> {
    let pts: Vec<(f64, f64)> = points
        .iter()
        .filter(|&(_, d)| *d >= 1)
        .map(|&(l, d)| (l as f64, d as f64))
        .collect();
    if pts.len() < 2 {
        return None;
    }

    let n = pts.len() as f64;
    let (mx, my) = (
        pts.iter().map(|p| p.0).sum::<f64>() / n,
        pts.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let sxy = pts.iter().map(|(x, y)| (x - mx) * (y - my)).sum::<f64>();
    let sxx = pts.iter().map(|(x, _)| (x - mx) * (x - mx)).sum::<f64>();
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx;
    if slope <= 0.0 {
        return None;
    }
    Some(1.0 / slope)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut gate = DEFAULT_GATE;
    let mut max_n = 100_000usize;
    let mut limits: Vec<usize> = vec![128, 256, 512, 1024, 2048, 4096];

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--gate" | "--max" => {
                let key = args[i].clone();
                if i + 1 < args.len()
                    && let Ok(v) = args[i + 1].parse::<usize>()
                {
                    if key == "--gate" {
                        gate = v;
                    } else {
                        max_n = v;
                    }
                    i += 2;
                } else {
                    i += 1;
                }
            }
            s => {
                if let Ok(v) = s.parse::<usize>() {
                    limits = vec![v];
                }
                i += 1;
            }
        }
    }

    // Probes run on a scratch big-stack thread so large strict limits stay
    // inside the real thread stack (see PROBE_THREAD_STACK).
    let code = std::thread::Builder::new()
        .name("reentry-depth".into())
        .stack_size(PROBE_THREAD_STACK)
        .spawn(move || run_measurement(&limits, max_n, gate))
        .expect("spawn probe thread")
        .join()
        .unwrap_or(2);

    std::process::exit(code);
}

/// Scan the limits, fit bytes/level, check the gate. Returns the exit code.
fn run_measurement(limits: &[usize], max_n: usize, gate: usize) -> i32 {
    println!(
        "reentry_depth: {} build, probe cap = {}, gate = {} bytes/level",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        max_n,
        gate
    );

    let start = Instant::now();
    let mut points: Vec<(usize, usize)> = Vec::new();
    let mut capped = false;
    for &kib in limits {
        let limit = kib * 1024;
        let depth = find_max_depth(limit, max_n);
        capped |= depth >= max_n;
        println!("  limit = {limit:>7} bytes ({kib:>4} KiB) -> max depth = {depth}");
        points.push((limit, depth));
    }
    println!("  search took {:?}", start.elapsed());

    let Some(bytes) = fit_bytes_per_level(&points) else {
        println!("  => cannot fit bytes/level (depths too small at these limits)");
        return 2;
    };

    println!("  => approx {:.0} bytes of C stack per native->JS reentry level", bytes);
    if capped {
        println!("  (note: depth hit --max at some limit; raise --max for a tighter fit)");
    }

    if bytes <= gate as f64 {
        println!("  gate ({} bytes/level): PASS", gate);
        0
    } else {
        println!("  gate ({} bytes/level): FAIL ({:.1}x over)", gate, bytes / gate as f64);
        1
    }
}
