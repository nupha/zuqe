# Known Issue: release fat-LTO miscompile — GC root loss → SEGV

Status: **not reproducible on current master** (`d8b6bb2f`, = `origin/master`).
Diagnosis complete for the pre-fix revisions (see `memo/lto-ub-handover.md`,
whose §20.2 holds the LLVM report draft); upstream root-cause
attribution (engine UB vs LLVM bug) still open.

Verified on master with the mitigation flag removed — rustc flags confirmed as
`-C opt-level=3 -C panic=abort -C lto=fat -C codegen-units=1`, with **no**
`-Cllvm-args=-memdep-block-scan-limit=1` (checked via `cargo build -v`):
**both** Windows toolchains are clean — window 0/30 and full suite 47940
tests / 0 failed each (MSVC rustc 1.96.1, GNU rustc 1.96.0; both LLVM 22.1.2).
The defect reproduces **only on pre-fix revisions**, e.g. `53040f5c` (see
toolchain matrix below).

## Affected builds (pre-fix revisions)

Release builds with **fat LTO + `panic="abort"` + opt-level ≥ 2**, on **both
Windows targets** — `x86_64-pc-windows-msvc` **and** `x86_64-pc-windows-gnu`
(LLVM 22.1.2; tested with rustc 1.96.1 on MSVC and 1.96.0 on GNU; nightly
1.100.0-nightly too). The defect is in the LLVM LTO backend, which both targets
share; the target ABI/linker differ only downstream of the wrong transform. The
GNU target is hit **more often** than MSVC — see the toolchain matrix below.

## Symptom

- Full Test262 suite SEGVs (`0xC0000005`) within 1–3 minutes.
- Window (`built-ins/Promise/**/*.js`, 732 tests): ~25–60 % of runs crash
  across toolchains (MSVC ~25 %, GNU ~60 %; ASLR/hash-seed sensitive).
- Crash site: `PromiseResolveFunc::class_call` (settle.rs:54) — reading
  `resolved: Rc<Cell<bool>>` from a GC-node payload that was already swept and
  freed (box reused by string allocations).
- Mechanism: the GC mark phase reads a queued job's **tracer capture**
  (`Box<dyn GcTraceFn>` holding copied JSValues) and sees stale/wrong data, so
  a still-referenced resolve-function node is swept. Of the two per-iteration
  capture copies (runnable / tracer), the **tracer side is lost, the runnable
  side is intact**.

## Build matrix

Pure combination results on `x86_64-pc-windows-msvc`. Window =
`built-ins/Promise/**/*.js` (732 tests); crash rate is ASLR/hash-seed
sensitive. The same opt-level / LTO / panic thresholds hold for the GNU
target (see toolchain matrix below).

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

- The `panic="unwind"` cross-product was fully swept at the diagnosis revision
  (2026-08-29): every unwind cell is clean (24 window runs), so
  `panic = "unwind"` is a global workaround regardless of opt-level / LTO
  (window-level verification; full suite not re-run at unwind).
- ASAN (dev profile, nightly): full suite clean — but sanitizer instrumentation
  perturbs codegen and masks the issue at release + fat-LTO (clean there too,
  non-probative).

## Toolchain matrix (MSVC vs GNU)

The error was reproduced in this test on revision `53040f5c` (2026-08-29), the
pre-mitigation parent of `491bdabb`. Release profile (fat LTO + `panic="abort"`
+ O3), built **without** the MemCpyOpt mitigation flag and with an isolated
`CARGO_TARGET_DIR`. Window = `built-ins/Promise/**/*.js` (732 tests), 20 runs
each:

| target | toolchain | window crashes | crash rate | exit code |
|---|---|---|---|---|
| `x86_64-pc-windows-msvc` | rustc 1.96.1, LLVM 22.1.2 | 5 / 20 | 25 % | `0xC0000005` |
| `x86_64-pc-windows-gnu` | rustc 1.96.0, LLVM 22.1.2 | 12 / 20 | 60 % | `0xC0000005` |

Both targets crash in the Promise tests; GNU is hit about 2.4× as often. This
rules out an MSVC-only toolchain artifact and pins the defect to the shared
LLVM LTO pipeline — the mitigation flag is a MemDep-consumer knob, so its
presence is what changes the outcome, not the target ABI.

Measurement caveats:

- **Environment is not fully pinned at `53040f5c`.** That revision has no
  `Cargo.lock`, and its `Cargo.toml` uses path dependencies, so the build
  resolved against the sibling repos as they were at test time:
  `gc-lite-v2` @ `c111899` (2026-09-13) and `zuqe-zapi` @ `a507205`
  (2026-09-14) — i.e. newer than the 2026-08-29 source. The rustc patch also
  differs between the two runs (MSVC 1.96.1 vs GNU stable 1.96.0); both share
  LLVM 22.1.2, which is the layer the defect lives in.
- `.cargo/config.toml` used to inject `-Cllvm-args=-memdep-block-scan-limit=1`
  globally, masking the issue. Cross-toolchain measurement requires removing
  that flag. **The flag injection and the build.rs guard have since been
  removed** — the flag now lives in the inactive `.cargo/config_obsolete.toml`
  (`5d96a8f9b`), the guard was deleted (`cdb6484a2`); this file now documents
  the historical state.
- **Cargo target-dir reuse across toolchains (gotcha).** Without an explicit
  `CARGO_TARGET_DIR`, both toolchains write to `target/release`, and cargo does
  **not** invalidate on a host/toolchain switch: after a GNU build, a plain
  MSVC `cargo build --release` finished in `0.47 s` and reused the GNU binary
  (`objdump -p` showed `msvcrt.dll`, no `VCRUNTIME140`). Cross-toolchain
  testing therefore requires a separate `CARGO_TARGET_DIR` (or a `cargo clean`)
  per target, otherwise you silently measure the other toolchain's binary.

### Fix attribution

Verified by bisecting at the commit boundary, both toolchains, mitigation
removed, fresh target dirs, no `Cargo.lock`:

| revision | toolchain | window | full suite |
|---|---|---|---|
| `b58d56e5` (499b45b9^, before fix) | MSVC rustc 1.96.1 | **6 / 20 crash** | **CRASH** at ~test 11563 |
| `b58d56e5` (499b45b9^, before fix) | GNU rustc 1.96.0 | **8 / 20 crash** | — |
| `499b45b9` (the fix commit) | MSVC rustc 1.96.1 | 0 / 20 | 47940 / 0 |
| `499b45b9` (the fix commit) | GNU rustc 1.96.0 | 0 / 20 | 47940 / 0 |

`499b45b9 fix(runtime): run each microtask in its own GC scope` is the
commit that eliminates the miscompile. The `b58d56e5` (parent) full suite
crashed deterministically — last log line: `built-ins/Promise/allSettled/
resolved-all-mixed.js`, exit `0xC0000005`.

#### What the fix does and why it resolves the crash

One-line change in `src/runtime/tick.rs` (the oneshot-job drain loop):

```rust
- let res = job.run(cx);
+ let res = cx.with_new_scope(|job_cx| job.run(job_cx));
```

Before: the whole microtask queue drained inside a single GC scope, so the
temporaries one job allocates (async resolve functions, Promise objects created
per `await`) stayed GC-rooted until the queue emptied. Since each `await`
enqueues the next job, an `await` loop pinned every step's locals and
accumulated memory without bound.

The fix runs each microtask in its **own child scope**, so its locals are
released when the job ends. This changes the lifetime of the per-iteration
tracer capture (`Box<dyn GcTraceFn>` holding copied JSValues): the capture is
rooted exactly as long as its job runs, and is swept promptly instead of
drifting across many jobs on a shared scope.

That is the precise condition the miscompile exploited: the GC mark phase read
a stale/long-lived tracer capture, decided a still-referenced resolve-function
node was dead, swept it, and the queued job later called the freed node → SEGV
in `PromiseResolveFunc::class_call` (settle.rs:54). Scoping each job closes the
window — the mark-phase root read and the job's live duration now agree, so
the MemCpyOpt rewrite can no longer produce a use-after-free at runtime.

## How to eliminate the release UB

The `499b45b9` fix removes the UB entirely, so none of the historical
workarounds below are needed on current master. They are kept for reference
(and still apply to any pre-fix revision):

1. Don't use `panic = "abort"`.
2. Don't use `lto = "fat"`.
3. Use `opt-level = 1` (or lower).
4. (Pre-fix only, mechanism now deleted) `RUSTFLAGS="-Cllvm-args=-memdep-block-scan-limit=1"`
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
