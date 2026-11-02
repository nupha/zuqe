// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! JIT configuration and its builder.

use super::metrics::JitMetrics;

/// Sink called with a metrics snapshot after each poll.
pub(crate) type MetricsSink = Box<dyn Fn(&JitMetrics)>;

const DEFAULT_CALL_THRESHOLD: u32 = 32;
const DEFAULT_LOOP_THRESHOLD: u32 = 56;
/// The invocation count that promotes an already-compiled function to the
/// optimizing tier (the first gate compiles the baseline). 1024 sits in the
/// mainstream band (V8 Maglev 400/1000, SpiderMonkey Ion 1500, JSC DFG 1000).
const DEFAULT_TIER2_CALL_THRESHOLD: u32 = 1024;
/// The back-edge counterpart of [`DEFAULT_TIER2_CALL_THRESHOLD`], keeping the
/// same 1:2 ratio the first gate has (32 calls / 56 back edges).
const DEFAULT_TIER2_LOOP_THRESHOLD: u32 = 2048;
/// How many invocations a demoted function must see before it may be promoted
/// again (V8's `minimum_invocations_after_ic_update = 500`). It stops the
/// promotion and the tier-down from chasing each other.
const DEFAULT_TIER2_MIN_INVOCATIONS: u32 = 500;
/// Evidence a site must show before `Automatic` pays for a speculative fast
/// path (8 matches the reference's "at least 8 executions before a trial"):
/// with the stock thresholds a genuinely hot site clears it for free, while a
/// one-off observation in a cold branch no longer earns a guard-miss path.
const DEFAULT_SPECIALIZATION_MIN_SAMPLES: u32 = 8;
const DEFAULT_MAX_CODE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_QUEUE_LEN: usize = 256;
const DEFAULT_WORKERS: usize = 1;
const DEFAULT_COMPILE_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_COMPILE_ATTEMPTS: u32 = 4;
/// The benefit side of the cost gate: what one bytecode of native execution
/// saves over interpreting it (the interpreter spends ~20–50 ns per bytecode).
/// The cost side is measured at run time.
const DEFAULT_NATIVE_SAVING_NS_PER_BYTECODE: u64 = 20;

/// Which tier the JIT is allowed to reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JitTierPolicy {
    /// JIT disabled: hooks count nothing, nothing is compiled.
    Interpreter,
    /// Baseline translation only.
    Tier1,
    /// Optimizing tier only, compiled at the first gate: every int-monomorphic
    /// site the feedback recorded is specialized, regardless of the
    /// profitability gate.
    Tier2,
    /// Pick the tier automatically: the first gate (`call_threshold` /
    /// `loop_threshold`) compiles the baseline, the second
    /// (`tier2_call_threshold` / `tier2_loop_threshold`) promotes to the
    /// optimizing tier. The profitability gate applies to the optimizing tier
    /// only.
    #[default]
    Automatic,
}

/// Host-tunable JIT settings. Build one with [`JitConfig::builder`], or take
/// [`JitConfig::default`] as-is.
pub struct JitConfig {
    pub(crate) call_threshold: u32,
    pub(crate) loop_threshold: u32,
    /// Second gate: the invocation count that promotes a compiled function to
    /// the optimizing tier. Only the `Automatic` policy promotes; the forced
    /// policies compile their tier right away.
    pub(crate) tier2_call_threshold: u32,
    /// The back-edge counterpart of [`Self::tier2_call_threshold`].
    pub(crate) tier2_loop_threshold: u32,
    /// Invocations a demoted function must see before it may be promoted
    /// again.
    pub(crate) tier2_min_invocations: u32,
    pub(crate) tier_policy: JitTierPolicy,
    /// Minimum feedback samples a site needs before `Automatic` specializes it.
    /// Forced `Tier2` ignores it.
    pub(crate) specialization_min_samples: u32,
    pub(crate) max_code_bytes: usize,
    pub(crate) max_metadata_bytes: usize,
    pub(crate) max_queue_len: usize,
    /// Poll at engine safe points, so the host never has to call `Jit::poll()`
    /// itself. Two points: the interpreter's trampoline (frame boundaries, so a
    /// hot callee runs natively for the rest of the *same* `eval`) and the
    /// event-loop tick (after a collection, so a long `eval_wait` keeps
    /// draining). The poll runs only when work is queued or compiling and no
    /// native frame is in flight. Off means only an explicit `Jit::poll()`
    /// compiles anything — useful for tests that pin the compile moment.
    pub(crate) auto_poll: bool,
    /// Number of compiler workers.
    pub(crate) workers: usize,
    /// Compile-time budget in milliseconds: a real deadline. An attempt past it
    /// is abandoned (counted `TimedOut`) and its late completion is discarded
    /// by attempt id.
    pub(crate) compile_timeout_ms: u64,
    pub(crate) max_compile_attempts: u32,
    /// The benefit side of the cost gate: nanoseconds one bytecode of native
    /// execution saves over interpreting it. Compared with the **measured**
    /// cost of a compile (`jit::rate`), it decides whether a compile can pay
    /// for itself. `0` turns the gate off: every function that crosses the
    /// thresholds is compiled.
    pub(crate) native_saving_ns_per_bytecode: u64,
    /// Called with a snapshot after each `Jit::poll`.
    pub(crate) metrics_observer: Option<MetricsSink>,
}

impl std::fmt::Debug for JitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitConfig")
            .field("call_threshold", &self.call_threshold)
            .field("loop_threshold", &self.loop_threshold)
            .field("tier2_call_threshold", &self.tier2_call_threshold)
            .field("tier2_loop_threshold", &self.tier2_loop_threshold)
            .field("tier2_min_invocations", &self.tier2_min_invocations)
            .field("tier_policy", &self.tier_policy)
            .field(
                "specialization_min_samples",
                &self.specialization_min_samples,
            )
            .field("max_code_bytes", &self.max_code_bytes)
            .field("max_metadata_bytes", &self.max_metadata_bytes)
            .field("max_queue_len", &self.max_queue_len)
            .field("auto_poll", &self.auto_poll)
            .field("workers", &self.workers)
            .field("compile_timeout_ms", &self.compile_timeout_ms)
            .field("max_compile_attempts", &self.max_compile_attempts)
            .finish_non_exhaustive()
    }
}

impl Default for JitConfig {
    fn default() -> Self {
        Self {
            call_threshold: DEFAULT_CALL_THRESHOLD,
            loop_threshold: DEFAULT_LOOP_THRESHOLD,
            tier2_call_threshold: DEFAULT_TIER2_CALL_THRESHOLD,
            tier2_loop_threshold: DEFAULT_TIER2_LOOP_THRESHOLD,
            tier2_min_invocations: DEFAULT_TIER2_MIN_INVOCATIONS,
            tier_policy: JitTierPolicy::default(),
            specialization_min_samples: DEFAULT_SPECIALIZATION_MIN_SAMPLES,
            max_code_bytes: DEFAULT_MAX_CODE_BYTES,
            max_metadata_bytes: DEFAULT_MAX_METADATA_BYTES,
            max_queue_len: DEFAULT_MAX_QUEUE_LEN,
            auto_poll: true,
            workers: DEFAULT_WORKERS,
            compile_timeout_ms: DEFAULT_COMPILE_TIMEOUT_MS,
            max_compile_attempts: DEFAULT_MAX_COMPILE_ATTEMPTS,
            native_saving_ns_per_bytecode: DEFAULT_NATIVE_SAVING_NS_PER_BYTECODE,
            metrics_observer: None,
        }
    }
}

impl JitConfig {
    pub fn builder() -> JitConfigBuilder {
        JitConfigBuilder(Self::default())
    }
}

/// Builder for [`JitConfig`]. Counts are clamped to `>= 1` so an accidental
/// `0` cannot make a function compile on every single call.
#[derive(Default)]
pub struct JitConfigBuilder(JitConfig);

impl JitConfigBuilder {
    pub fn call_threshold(mut self, n: u32) -> Self {
        self.0.call_threshold = n.max(1);
        self
    }

    pub fn loop_threshold(mut self, n: u32) -> Self {
        self.0.loop_threshold = n.max(1);
        self
    }

    /// Second gate: the invocation count that promotes a compiled function to
    /// the optimizing tier. Clamped to `>= 1`; the forced `Tier1`/`Tier2`
    /// policies ignore it (they compile their tier right away).
    pub fn tier2_call_threshold(mut self, n: u32) -> Self {
        self.0.tier2_call_threshold = n.max(1);
        self
    }

    /// The back-edge counterpart of [`Self::tier2_call_threshold`].
    pub fn tier2_loop_threshold(mut self, n: u32) -> Self {
        self.0.tier2_loop_threshold = n.max(1);
        self
    }

    /// Invocations a demoted function must see before it may be promoted
    /// again (V8's `minimum_invocations_after_ic_update`).
    pub fn tier2_min_invocations(mut self, n: u32) -> Self {
        self.0.tier2_min_invocations = n.max(1);
        self
    }

    pub const fn tier_policy(mut self, p: JitTierPolicy) -> Self {
        self.0.tier_policy = p;
        self
    }

    /// Samples a feedback site needs before `Automatic` specializes it
    /// Clamped to `>= 1`; `Tier2` ignores this entirely.
    pub fn specialization_min_samples(mut self, n: u32) -> Self {
        self.0.specialization_min_samples = n.max(1);
        self
    }

    pub const fn max_code_bytes(mut self, n: usize) -> Self {
        self.0.max_code_bytes = n;
        self
    }

    pub const fn max_metadata_bytes(mut self, n: usize) -> Self {
        self.0.max_metadata_bytes = n;
        self
    }

    pub fn max_queue_len(mut self, n: usize) -> Self {
        self.0.max_queue_len = n.max(1);
        self
    }

    /// Turn the automatic poll on/off; off means only an explicit `Jit::poll()`
    /// compiles anything.
    pub const fn auto_poll(mut self, on: bool) -> Self {
        self.0.auto_poll = on;
        self
    }

    /// Number of compiler workers.
    pub fn workers(mut self, n: usize) -> Self {
        self.0.workers = n.max(1);
        self
    }

    /// Compile-time budget in milliseconds; an attempt past it is abandoned.
    pub fn compile_timeout_ms(mut self, n: u64) -> Self {
        self.0.compile_timeout_ms = n.max(1);
        self
    }

    pub const fn max_compile_attempts(mut self, n: u32) -> Self {
        self.0.max_compile_attempts = n;
        self
    }

    /// Benefit side of the cost gate; `0` turns it off.
    pub const fn native_saving_ns_per_bytecode(mut self, n: u64) -> Self {
        self.0.native_saving_ns_per_bytecode = n;
        self
    }

    /// Install an observer called with a metrics snapshot after each poll.
    pub fn metrics_observer(mut self, f: impl Fn(&JitMetrics) + 'static) -> Self {
        self.0.metrics_observer = Some(Box::new(f));
        self
    }

    pub fn build(self) -> JitConfig {
        self.0
    }
}
