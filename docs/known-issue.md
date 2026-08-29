# Known Issue: release fat-LTO miscompile — GC root loss → SEGV

Status: **open**. Diagnosis complete (see `memo/lto-ub-handover.md`,
LLVM report draft in `memo/llvm-bug-report.md`). Root-cause attribution
(engine UB vs LLVM bug) pending upstream review.

## Affected builds

`x86_64-pc-windows-msvc`, release builds with **fat LTO + `panic="abort"` +
opt-level ≥ 2** (rustc 1.96.1 / LLVM 22.1.2; nightly 1.100.0-nightly too).

## Symptom

- Full Test262 suite SEGVs (`0xC0000005`) within 1–3 minutes.
- Window (`built-ins/Promise/**/*.js`, 732 tests): ~25–50 % of runs crash
  (ASLR/hash-seed sensitive).
- Crash site: `PromiseResolveFunc::class_call` (settle.rs:54) — reading
  `resolved: Rc<Cell<bool>>` from a GC-node payload that was already swept and
  freed (box reused by string allocations).
- Mechanism: the GC mark phase reads a queued job's **tracer capture**
  (`Box<dyn GcTraceFn>` holding copied JSValues) and sees stale/wrong data, so
  a still-referenced resolve-function node is swept. Of the two per-iteration
  capture copies (runnable / tracer), the **tracer side is lost, the runnable
  side is intact**.

## Build matrix

Pure combination results. Window = `built-ins/Promise/**/*.js` (732 tests);
crash rate is ASLR/hash-seed sensitive.

| opt-level | LTO | panic | result |
|---|---|---|---|
| O3 | fat | abort | **CRASH** (~25–50 % of window runs; full suite deterministic) |
| O2 | fat | abort | **CRASH** (2/8) |
| O1 | fat | abort | clean 0/6 (all 732 pass) |
| O3 | thin | abort | clean 0/6 |
| O3 | off | abort | clean 0/10 |
| O3 | fat | unwind | clean 0/6 |
| O2 | fat | unwind | clean 0/6 |
| O1 | fat | unwind | clean 0/4 |
| O3 | thin | unwind | clean 0/4 |
| O3 | off | unwind | clean 0/4 |
| O0 (debug) | off | unwind | clean |

- The `panic="unwind"` cross-product was fully swept on the current source
  (2026-08-29): every unwind cell is clean (24 window runs), so
  `panic = "unwind"` is a global workaround regardless of opt-level / LTO
  (window-level verification; full suite not re-run at unwind).
- ASAN (dev profile, nightly): full suite clean — but sanitizer instrumentation
  perturbs codegen and masks the issue at release + fat-LTO (clean there too,
  non-probative).

## How to eliminate the release UB

Apply any one of the following:

1. Don't use `panic = "abort"`.
2. Don't use `lto = "fat"`.
3. Use `opt-level = 1` (or lower).
4. If you must keep `panic = "abort"` + O2/O3 + `lto = "fat"`, then add:
   `RUSTFLAGS="-Cllvm-args=-memdep-block-scan-limit=1"` extra flags when build with cargo.
   (limits 1 Ok, 4/8 maybe Ok, 16+ does not; verified with the full Test262 suite).

## Diagnosis summary

- Identified carrier: **MemCpyOpt (MemoryDependenceAnalysis-based) in the LTO
  backend pipeline** — the scan-limit flag only affects MemDep consumers;
  JumpThreading, vectorization, inlining budget, CGU count, allocator (arena
  on/off) and struct layout are all excluded. Pre-LTO IR of the affected
  memcpys is intact and semantically correct; the wrong transformation happens
  only after cross-crate LTO merging.
- Trigger requires **nounwind (abort)**: unwinding builds are clean even with
  fat-LTO + O3 — consistent with a cross-call store/load transformation that
  LLVM only performs when calls cannot unwind.
- Engine GC rooting logic is audited clean (full chain); the noalias-scope
  channel is closed at IR level (all scopes assert genuinely disjoint facts;
  mark-side reads are scope-less/conservative). Standalone 100 %-safe-Rust
  reproducers mirroring the enqueue/tracer/run shapes do **not** reproduce.
- Working hypothesis: source-level UB elsewhere in the engine (e.g. poison via
  raw-pointer arithmetic) authorizes the transform — unconfirmed; poison
  triage and an upstream LLVM review are in progress.
