// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Multi-mode benchmark matrix (M2b-6): interpreter / tier1 / automatic over a
//! small set of numeric, property and array kernels (kernels ported from
//! quickjs-jit `benchmarks/scripts/*.js`).
//!
//! Run with: `cargo run --release --features jit --example jit-bench -- \
//!   --modes interpreter,tier1,automatic` (or `scripts/jit-bench-matrix.sh`).
//!
//! Protocol: 2 warm-up rounds discarded, then 7 timed rounds; the per-round
//! median is reported. Every mode must produce the same result per kernel,
//! asserted bit-exactly before that row is printed. `tier2` needs epic F.

use std::time::{Duration, Instant};

use zuqe::{
    Context, EvalFlags, EvalName, EvalType, Jit, JitConfig, JitTierPolicy, Runtime, ScopedContext,
};

const WARMUP: usize = 2;
/// Odd so the median is an actual sample.
const ROUNDS: usize = 7;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    /// Policy `Interpreter`: JIT attached but every hook inert.
    Interpreter,
    /// Forced baseline tier.
    Tier1,
    /// Production default: pick the tier from hotness.
    Automatic,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Mode::Interpreter => "interpreter",
            Mode::Tier1 => "tier1",
            Mode::Automatic => "automatic",
        }
    }

    const fn policy(self) -> JitTierPolicy {
        match self {
            Mode::Interpreter => JitTierPolicy::Interpreter,
            Mode::Tier1 => JitTierPolicy::Tier1,
            Mode::Automatic => JitTierPolicy::Automatic,
        }
    }

    /// Eager thresholds so the single warm-up call in `define` queues the
    /// function and `poll` installs it before any timing starts.
    fn config(self) -> JitConfig {
        JitConfig::builder()
            .call_threshold(1)
            .loop_threshold(1)
            .tier_policy(self.policy())
            .build()
    }
}

struct Kernel {
    name: &'static str,
    /// Defines `workload(...)` and warms it up once; must not throw.
    define: &'static str,
    /// The timed computation; returns the kernel's result value.
    work: &'static str,
}

const KERNELS: &[Kernel] = &[
    // Iterative fib (the original jit-bench kernel), loop moved into workload.
    Kernel {
        name: "fib-iter",
        define: "function fib(n){ var a = 0, b = 1; for (var i = 0; i < n; i++){ var t = a + b; a = b; b = t; } return b; } \
                 function workload(){ var s = 0; for (var k = 0; k < 4000; k++) s = fib(30); return s; } workload();",
        work: "workload();",
    },
    // scalar-loop.js: integer induction + addition, one back edge.
    Kernel {
        name: "scalar-loop",
        define: "function workload(n, seed){ var sum = seed; for (var i = seed; i < n; i++) sum = sum + i; return sum; } workload(10, 1);",
        work: "workload(2000000, 1);",
    },
    // quickjs-bitops.js: shifts, xor, int32 wrap.
    Kernel {
        name: "bitops",
        define: "function workload(n, seed){ var v = seed | 0; for (var i = 0; i < n; i++) v = ((v << 5) ^ (v >>> 3) ^ i) | 0; return v; } workload(10, 1);",
        work: "workload(1000000, 12345);",
    },
    // property-heavy.js: monomorphic property read/write, receiver as argument.
    Kernel {
        name: "property",
        define: "var point = { x: 0, y: 1 }; \
                 function workload(n, seed, p){ p.x = seed; p.y = 1; var toggle = 0; \
                   for (var i = 0; i < n; i++){ p.x = p.x + p.y; p.y = p.y + toggle; toggle = 1 - toggle; } \
                   return p.x + p.y; } workload(10, 1, point);",
        work: "workload(500000, 1, point);",
    },
    // packed-array-traversal.js: dense array sum with int32 wrap.
    Kernel {
        name: "array-sum",
        define: "var values = new Array(2000); \
                 for (var i = 0; i < values.length; i++) values[i] = (i * 17) & 0xffff; \
                 function workload(seed, a){ var s = seed; \
                   for (var r = 0; r < 500; r++){ s = seed; for (var i = 0; i < a.length; i++) s = (s + a[i]) | 0; } \
                   return s; } workload(0, values);",
        work: "workload(7, values);",
    },
];

/// Parse `--modes interpreter,tier1,automatic`; default is all three runnable
/// modes. `tier2` prints a note and is skipped; unknown modes exit non-zero.
fn parse_modes(args: &[String]) -> Vec<Mode> {
    let mut selected = vec![Mode::Interpreter, Mode::Tier1, Mode::Automatic];
    let Some(i) = args.iter().position(|a| a == "--modes") else {
        return selected;
    };
    let Some(list) = args.get(i + 1) else {
        eprintln!("--modes needs a comma list, e.g. --modes interpreter,tier1,automatic");
        std::process::exit(2);
    };
    selected.clear();
    for tok in list.split(',') {
        let mode = match tok.trim() {
            "" => continue,
            "interpreter" => Mode::Interpreter,
            "tier1" => Mode::Tier1,
            "automatic" => Mode::Automatic,
            "tier2" => {
                println!("tier2: not implemented (epic F), skipping");
                continue;
            }
            other => {
                eprintln!(
                    "unknown mode {other:?}: valid modes are interpreter, tier1, automatic (tier2 not implemented)"
                );
                std::process::exit(2);
            }
        };
        if !selected.contains(&mode) {
            selected.push(mode);
        }
    }
    selected
}

fn eval(ctx: &mut Context, src: &[u8]) -> f64 {
    let v = ctx
        .eval(
            src,
            &EvalName::Name("jit-bench.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .unwrap();
    // Normalize Int/Float64 tags: numeric equality is what the bench checks
    // (tag-exact equality is covered by tests/jit_differential.rs).
    v.as_int()
        .map(|i| i as f64)
        .or_else(|| v.as_float())
        .expect("kernel must return a number")
}

struct Run {
    result: f64,
    median: Duration,
    compiled: u64,
}

fn run_kernel(kernel: &Kernel, mode: Mode) -> Run {
    let mut rt = Runtime::create().unwrap();
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();
    let jit = Jit::attach_to(&rt, mode.config()).unwrap();
    eval(&mut ctx, kernel.define.as_bytes());
    jit.poll();
    let compiled = jit.metrics().functions_compiled;
    match mode {
        Mode::Interpreter => assert_eq!(
            compiled, 0,
            "{}: interpreter policy must not compile",
            kernel.name
        ),
        _ => assert!(
            compiled > 0,
            "{}: {} did not compile the kernel",
            kernel.name,
            mode.label()
        ),
    }

    // Verify run first, then warm-up (discarded), then timed rounds.
    let result = eval(&mut ctx, kernel.work.as_bytes());
    for _ in 0..WARMUP {
        std::hint::black_box(eval(&mut ctx, kernel.work.as_bytes()));
    }
    let mut samples: Vec<Duration> = (0..ROUNDS)
        .map(|_| {
            let start = Instant::now();
            let r = eval(&mut ctx, kernel.work.as_bytes());
            // Every timed round must still produce the verified result.
            assert_eq!(
                r.to_bits(),
                result.to_bits(),
                "{}: {} timed round drifted",
                kernel.name,
                mode.label()
            );
            start.elapsed()
        })
        .collect();
    samples.sort_unstable();
    Run {
        result,
        median: samples[ROUNDS / 2],
        compiled,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show = parse_modes(&args);
    if show.is_empty() {
        println!("no runnable mode selected; nothing to benchmark");
        return;
    }
    // The interpreter column is the speedup baseline, so always run it even
    // when it was not selected.
    let mut run = show.clone();
    if !run.contains(&Mode::Interpreter) {
        run.insert(0, Mode::Interpreter);
    }

    println!(
        "jit-bench matrix: median of {ROUNDS} timed rounds, {WARMUP} warm-up rounds discarded"
    );
    let header: String = run
        .iter()
        .map(|m| {
            if show.contains(m) {
                format!("{:>19}", m.label())
            } else {
                String::new()
            }
        })
        .collect();
    println!("{:<14}{}", "kernel", header);

    let mut compiled: Vec<(Mode, u64)> = run.iter().map(|&m| (m, 0)).collect();
    for kernel in KERNELS {
        let runs: Vec<(Mode, Run)> = run.iter().map(|&m| (m, run_kernel(kernel, m))).collect();
        let base = runs
            .iter()
            .find(|(m, _)| *m == Mode::Interpreter)
            .expect("interpreter baseline")
            .1
            .result;
        // Result consistency across modes is part of the acceptance.
        for (m, r) in &runs {
            assert_eq!(
                r.result.to_bits(),
                base.to_bits(),
                "{}: {} disagrees with interpreter",
                kernel.name,
                m.label()
            );
        }

        let row: String = runs
            .iter()
            .map(|(m, r)| {
                if !show.contains(m) {
                    return String::new();
                }
                let base_s = runs
                    .iter()
                    .find(|(bm, _)| *bm == Mode::Interpreter)
                    .unwrap()
                    .1
                    .median
                    .as_secs_f64();
                let speedup = base_s / r.median.as_secs_f64().max(f64::EPSILON);
                format!("{:>9.3}ms {:>6.2}x", r.median.as_secs_f64() * 1e3, speedup)
            })
            .collect::<Vec<_>>()
            .join("");
        println!("{:<14}{}", kernel.name, row);

        for (m, r) in &runs {
            let slot = compiled.iter_mut().find(|(cm, _)| cm == m).unwrap();
            slot.1 += r.compiled;
        }
    }

    println!("results: identical across modes (asserted bit-exact per kernel)");
    let summary: String = compiled
        .iter()
        .map(|(m, c)| format!("{}={c}", m.label()))
        .collect::<Vec<_>>()
        .join(" ");
    println!("functions_compiled: {summary}");
}
