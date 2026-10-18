// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! JIT configuration and its builder.

use super::metrics::JitMetrics;

/// Sink for human-readable diagnostics.
pub(crate) type DiagnosticSink = Box<dyn Fn(&str)>;
/// Sink called with a metrics snapshot after each poll.
pub(crate) type MetricsSink = Box<dyn Fn(&JitMetrics)>;

const DEFAULT_CALL_THRESHOLD: u32 = 32;
const DEFAULT_LOOP_THRESHOLD: u32 = 56;
/// Evidence a site must show before `Automatic` pays for a speculative fast
/// path (epic F5). Default 8 matches the reference's "at least 8 executions
/// before a trial": with the stock thresholds (32 calls / 56 backedges) a
/// genuinely hot site clears it for free, while a one-off observation in a
/// cold branch no longer earns a guard-miss path.
const DEFAULT_SPECIALIZATION_MIN_SAMPLES: u32 = 8;
const DEFAULT_MAX_CODE_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
const DEFAULT_MAX_QUEUE_LEN: usize = 256;
const DEFAULT_WORKERS: usize = 1;
const DEFAULT_COMPILE_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_MAX_COMPILE_ATTEMPTS: u32 = 4;

/// Which tier the JIT is allowed to reach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JitTierPolicy {
    /// JIT disabled: hooks count nothing, nothing is compiled.
    Interpreter,
    /// Baseline translation only.
    Tier1,
    /// Optimizing tier only. Fixed policy: every int-monomorphic site F1
    /// recorded is specialized, regardless of the profitability gate.
    Tier2,
    /// Pick the tier automatically from the hotness feedback, and apply the
    /// profitability gate (`JitConfig::specialization_min_samples`): a site is
    /// only specialized once the interpreter measured enough samples there.
    #[default]
    Automatic,
}

/// Host-tunable JIT settings. Build one with [`JitConfig::builder`], or take
/// [`JitConfig::default`] as-is.
pub struct JitConfig {
    pub(crate) call_threshold: u32,
    pub(crate) loop_threshold: u32,
    pub(crate) tier_policy: JitTierPolicy,
    /// Minimum feedback samples a site needs before `Automatic` specializes
    /// it (epic F5). Forced `Tier2` ignores it.
    pub(crate) specialization_min_samples: u32,
    pub(crate) max_code_bytes: usize,
    pub(crate) max_metadata_bytes: usize,
    pub(crate) max_queue_len: usize,
    /// Poll automatically at engine safe points, so the host never has to call
    /// `Jit::poll()` itself. On by default. Two points: the interpreter's
    /// trampoline (frame boundaries, so a hot callee runs natively for the rest
    /// of the *same* `eval`) and the event-loop tick (after the collection, so a
    /// long `eval_wait` / host loop keeps draining). The poll runs only when work
    /// is queued and no native frame is in flight. Turn it off only to keep full
    /// manual control of the compile timing (tests/benchmarks).
    pub(crate) auto_poll: bool,
    /// Reserved for the §12 background compiler (`jit_background`). The inline
    /// model compiles on the calling thread between two `poll`s, so this field
    /// is stored and reported but **not read anywhere yet**: `workers(8)` does
    /// not currently parallelize anything.
    pub(crate) workers: usize,
    /// Compile-time budget in milliseconds. In the inline model this is a
    /// **failure-reason classification, not a deadline**: `Jit::poll` runs
    /// verification and compilation synchronously, so it cannot preempt a slow
    /// compile. The budget only re-labels an already-`Failed` attempt whose
    /// elapsed time exceeded it as `TimedOut` (see `JitState::poll`); no compile
    /// is ever abandoned mid-flight.
    pub(crate) compile_timeout_ms: u64,
    pub(crate) max_compile_attempts: u32,
    /// Sink for human-readable diagnostics.
    pub(crate) diagnostic_callback: Option<DiagnosticSink>,
    /// Called with a snapshot after each `Jit::poll`.
    pub(crate) metrics_observer: Option<MetricsSink>,
}

impl std::fmt::Debug for JitConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JitConfig")
            .field("call_threshold", &self.call_threshold)
            .field("loop_threshold", &self.loop_threshold)
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
            tier_policy: JitTierPolicy::default(),
            specialization_min_samples: DEFAULT_SPECIALIZATION_MIN_SAMPLES,
            max_code_bytes: DEFAULT_MAX_CODE_BYTES,
            max_metadata_bytes: DEFAULT_MAX_METADATA_BYTES,
            max_queue_len: DEFAULT_MAX_QUEUE_LEN,
            auto_poll: true,
            workers: DEFAULT_WORKERS,
            compile_timeout_ms: DEFAULT_COMPILE_TIMEOUT_MS,
            max_compile_attempts: DEFAULT_MAX_COMPILE_ATTEMPTS,
            diagnostic_callback: None,
            metrics_observer: None,
        }
    }
}

impl JitConfig {
    pub fn builder() -> JitConfigBuilder {
        JitConfigBuilder(Self::default())
    }

    pub const fn call_threshold(&self) -> u32 {
        self.call_threshold
    }

    pub const fn loop_threshold(&self) -> u32 {
        self.loop_threshold
    }

    pub const fn tier_policy(&self) -> JitTierPolicy {
        self.tier_policy
    }

    /// Profitability gate for `Automatic` (epic F5): the samples a feedback
    /// site must have before it earns a speculative fast path.
    pub const fn specialization_min_samples(&self) -> u32 {
        self.specialization_min_samples
    }

    pub const fn max_code_bytes(&self) -> usize {
        self.max_code_bytes
    }

    pub const fn max_metadata_bytes(&self) -> usize {
        self.max_metadata_bytes
    }

    pub const fn max_queue_len(&self) -> usize {
        self.max_queue_len
    }

    /// Whether the JIT polls itself at engine safe points (trampoline frame
    /// boundaries and the event-loop tick). See the `auto_poll` field.
    pub const fn auto_poll(&self) -> bool {
        self.auto_poll
    }

    pub const fn workers(&self) -> usize {
        self.workers
    }

    pub const fn compile_timeout_ms(&self) -> u64 {
        self.compile_timeout_ms
    }

    pub const fn max_compile_attempts(&self) -> u32 {
        self.max_compile_attempts
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

    pub const fn tier_policy(mut self, p: JitTierPolicy) -> Self {
        self.0.tier_policy = p;
        self
    }

    /// Samples a feedback site needs before `Automatic` specializes it
    /// (epic F5). Clamped to `>= 1`; `Tier2` ignores this entirely.
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

    /// Turn the automatic poll on/off (on by default; see the `auto_poll`
    /// field). Off means only an explicit `Jit::poll()` compiles anything, which
    /// is useful for tests that pin the compile moment.
    pub const fn auto_poll(mut self, on: bool) -> Self {
        self.0.auto_poll = on;
        self
    }

    /// Reserved for the §12 background compiler: stored, reported, and
    /// otherwise ignored today (see [`JitConfig::workers`]).
    pub fn workers(mut self, n: usize) -> Self {
        self.0.workers = n.max(1);
        self
    }

    /// Compile-time budget in milliseconds. In the inline model it only
    /// classifies an already-failed attempt as `TimedOut`; it is not a deadline
    /// that can preempt a running compile (see [`JitConfig::compile_timeout_ms`]).
    pub fn compile_timeout_ms(mut self, n: u64) -> Self {
        self.0.compile_timeout_ms = n.max(1);
        self
    }

    pub const fn max_compile_attempts(mut self, n: u32) -> Self {
        self.0.max_compile_attempts = n;
        self
    }

    /// Install a sink for human-readable diagnostics.
    pub fn diagnostic_callback(mut self, f: impl Fn(&str) + 'static) -> Self {
        self.0.diagnostic_callback = Some(Box::new(f));
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
