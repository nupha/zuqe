// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Hotness state machine.
//!
//! The interpreter counts entries and backward branches here; when a function
//! crosses its threshold it is queued for compilation exactly once. Thresholds
//! start from the configured base. The function's static shape (bytecode size,
//! helper density) can raise them on any tier; measured work — the Tier 2
//! feedback table (type feedback) supplies it — can lower them further.
//!
//! Two gates, two tiers: the first gate compiles the
//! **baseline** tier, the second one promotes the function to the **optimizing**
//! tier. Each gate is one **weighted budget**: an entry
//! weighs as much as the configured loop threshold, a back edge as much as the
//! call threshold, and the gate fires once the sum reaches their product —
//! `loops_weight × calls + calls_weight × loops ≥ budget`. The line runs
//! through the configured two corner points (unchanged from the old
//! `calls ≥ C ∨ loops ≥ L` gates), so `calls*W + loops` is the explicit form of
//! "an entry is worth more than a back edge" instead of an intuition.
//!
//! The counters behind both gates are the same, but the second gate must also
//! see the work a function does *after* it was compiled — the interpreter stops
//! counting then, so the native tier reports its own activations and back
//! edges back into the same counters.

use crate::{
    bytecode::{OP_COUNT_FINAL, opcode_info_final},
    jit::policy::{JitPolicy, policy_for},
};

/// Why a function needed the thresholds it did. Kept for diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HotReason {
    NeutralBase,
    MeasuredWork,
    BytecodeScale,
    HelperDensity,
}

/// The tier a function's code is at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Not compiled yet.
    Cold,
    /// Baseline: compiled without the tier-2 specialization.
    Tier1,
    /// Optimizing: compiled with the tier-2 specialization.
    Tier2,
}

/// One gate's weighted budget: the `calls ∨ loops`
/// crossing becomes `calls_weight × calls + loops_weight × loops ≥ budget`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GateBudget {
    /// An entry's weight among back edges (`= loop_threshold`).
    calls_weight: u32,
    /// A back edge's weight among entries (`= call_threshold`).
    loops_weight: u32,
    /// The gate fires at this many weighted points (`call × loop`, in `u64`
    /// because the host can configure unbounded thresholds).
    budget: u64,
}

/// The budgets a function must reach before it is queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HotThresholds {
    /// The first gate: entries and back edges reach this many weighted points.
    first: GateBudget,
    /// The second gate: the counts that promote an already-compiled function
    /// to the optimizing tier.
    tier2: GateBudget,
    pub(crate) reason: HotReason,
}

/// The size factor's fixed-point scale: the factor is kept in 1/16ths so the
/// thresholds stay exact integers.
const SIZE_SCALE: u32 = 16;
/// The cap of the size factor: 4x the base.
const SIZE_FACTOR_MAX: u32 = 4 * SIZE_SCALE;

impl GateBudget {
    /// The budget line through the configured pair's two corner points: `N`
    /// entries alone (`N = call_threshold`) and `L` back edges alone
    /// (`L = loop_threshold`) fire, exactly where the old OR-gates did (T8).
    /// `u32 × u32` fits `u64` exactly, so no saturation is needed.
    const fn of(calls: u32, loops: u32) -> Self {
        Self {
            calls_weight: loops,
            loops_weight: calls,
            budget: calls as u64 * loops as u64,
        }
    }
}

impl HotThresholds {
    pub(crate) const fn neutral(
        calls: u32,
        loops: u32,
        tier2_calls: u32,
        tier2_loops: u32,
    ) -> Self {
        Self {
            first: GateBudget::of(calls, loops),
            tier2: GateBudget::of(tier2_calls, tier2_loops),
            reason: HotReason::NeutralBase,
        }
    }
}

/// Static facts about a function, used once to pick its thresholds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdaptiveInputs {
    pub(crate) bytecode_bytes: u32,
    pub(crate) instruction_count: u32,
    pub(crate) helper_ops: u32,
    /// Interpreter work measured for this function. `None` until Tier 2
    /// feedback exists (type feedback); while it is `None` the thresholds are the
    /// configured base, exactly as before.
    pub(crate) measured_work: Option<u32>,
}

impl AdaptiveInputs {
    /// Walk the opcode stream once. Unknown/corrupt opcodes stop the scan; a
    /// live function's bytecode is always well-formed, so this only guards
    /// against a size field overrunning the buffer.
    pub(crate) fn from_opcodes(bc: &[u8]) -> Self {
        let mut instruction_count = 0u32;
        let mut helper_ops = 0u32;
        let mut pos = 0usize;
        while pos < bc.len() {
            let op = bc[pos];
            if op as usize >= OP_COUNT_FINAL as usize {
                break;
            }
            instruction_count += 1;
            if policy_for(op) == JitPolicy::Helper {
                helper_ops += 1;
            }
            pos += (opcode_info_final(op).size as usize).max(1);
        }
        Self {
            bytecode_bytes: bc.len() as u32,
            instruction_count,
            helper_ops,
            measured_work: None,
        }
    }

    /// Attach the interpreter's measurement: the hottest hooks pass
    /// the feedback table's observation count. `0` means "nothing recorded"
    /// (a baseline tier never records), which keeps the thresholds neutral
    /// instead of pretending the function was measured as doing no work.
    pub(crate) const fn with_measured_work(mut self, samples: u32) -> Self {
        self.measured_work = if samples == 0 { None } else { Some(samples) };
        self
    }

    /// The continuous size factor: `1 + bytes/1024`,
    /// capped at 4x, in 1/16ths. A 500-byte function and a 4000-byte one no
    /// longer get the same treatment — the compile cost grows with the size,
    /// the benefit does not.
    pub(crate) fn size_factor(&self) -> u32 {
        let extra = self.bytecode_bytes.saturating_mul(SIZE_SCALE) / 1024;
        (SIZE_SCALE + extra).min(SIZE_FACTOR_MAX)
    }

    /// Whether the function's opcodes are mostly helper calls. A helper-dominated
    /// function is expensive to compile relative to its size, so it waits
    /// longer.
    pub(crate) const fn helper_dense(&self) -> bool {
        self.instruction_count != 0 && self.helper_ops.saturating_mul(4) >= self.instruction_count
    }

    /// Larger and helper-dominated functions need more evidence before we
    /// spend compile time on them; functions already shown to do a lot of work
    /// can be compiled sooner. The static shape (bytecode size / helper
    /// density) is read unconditionally, so it also guides a tier that records
    /// no measurements; the measured work only lowers the base.
    ///
    /// The size is a **continuous** factor (`× (1 + bytes/1024)`, capped at
    /// 4x) instead of the old one-shot `bytes >= 4096 → ×2` step, and a
    /// helper-dominated function doubles that factor on top (the total stays
    /// capped at 4x).
    ///
    /// `payback` is the **cost gate**: the number of activations a
    /// function must be observed at before its compile can pay for itself,
    /// derived from the measured cost of a compile and the host's estimate of
    /// what native execution saves. It is a floor under the thresholds, so a
    /// function that is called a handful of times is not compiled at all.
    ///
    /// An eager base (both thresholds 1) is left alone: the host asked for
    /// "compile on the first event", and raising that would silently defeat
    /// the diagnostic/test harnesses that rely on it.
    ///
    /// T8: each gate is one **weighted budget**. The configured pair `(C, L)`
    /// survives as the line's two corner points — `C` entries alone and `L`
    /// back edges alone fire exactly as before — but a mixed function now
    /// fires early: the crossing is `L×calls + C×loops ≥ C×L`, so e.g.
    /// `C/2` entries plus `L/2` back edges are enough (the line cuts the
    /// rectangle's corner). The cost gate floors the pair, and the line follows
    /// it.
    pub(crate) fn thresholds(
        &self,
        base_calls: u32,
        base_loops: u32,
        base_tier2_calls: u32,
        base_tier2_loops: u32,
        payback: u32,
    ) -> HotThresholds {
        if base_calls <= 1 && base_loops <= 1 {
            return HotThresholds::neutral(
                base_calls,
                base_loops,
                base_tier2_calls,
                base_tier2_loops,
            );
        }
        // Proven work outranks the static shape: a function observed doing a lot
        // of interpreter work is worth compiling even if it is large. The
        // budget follows the halved pair (T8: half the calls, half the back
        // edges).
        if self.measured_work.is_some_and(|work| work >= 4096) {
            return HotThresholds {
                first: GateBudget::of(base_calls / 2, base_loops / 2),
                tier2: GateBudget::of(base_tier2_calls / 2, base_tier2_loops / 2),
                reason: HotReason::MeasuredWork,
            };
        }
        let helper_dense = self.helper_dense();
        let factor = if helper_dense {
            self.size_factor().saturating_mul(2).min(SIZE_FACTOR_MAX)
        } else {
            self.size_factor()
        };
        // The cost gate: a floor under the pair, budget line through it.
        // `u32::max(0)` is the identity, so `payback == 0` (the gate off) needs
        // no branch.
        let calls = (base_calls.saturating_mul(factor) / SIZE_SCALE).max(payback);
        let loops = (base_loops.saturating_mul(factor) / SIZE_SCALE)
            .max(payback.saturating_mul(base_loops) / base_calls.max(1));
        let tier2_calls = (base_tier2_calls.saturating_mul(factor) / SIZE_SCALE).max(payback);
        let tier2_loops = (base_tier2_loops.saturating_mul(factor) / SIZE_SCALE)
            .max(payback.saturating_mul(base_tier2_loops) / base_tier2_calls.max(1));
        let reason = if helper_dense {
            HotReason::HelperDensity
        } else if factor == SIZE_SCALE {
            HotReason::NeutralBase
        } else {
            HotReason::BytecodeScale
        };
        HotThresholds {
            first: GateBudget::of(calls, loops),
            tier2: GateBudget::of(tier2_calls, tier2_loops),
            reason,
        }
    }
}

/// Per-function hotness counters.
///
/// The counters keep running after the function was compiled: the interpreter
/// stops seeing its entries and back edges, so the native tier reports its
/// own (see [`Self::record_native_calls`] / [`Self::record_native_loops`]).
/// That is what lets the second gate be crossed at all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HotnessState {
    calls: u32,
    loops: u32,
}

impl HotnessState {
    /// Count one function entry.
    pub(crate) const fn record_call(&mut self) {
        self.calls = self.calls.saturating_add(1);
    }

    /// Count one taken backward branch.
    pub(crate) const fn record_loop(&mut self) {
        self.loops = self.loops.saturating_add(1);
    }

    /// Count the activations the native tier ran for this function.
    pub(crate) const fn record_native_calls(&mut self, n: u32) {
        self.calls = self.calls.saturating_add(n);
    }

    /// Count the back edges the native tier ran for this function.
    pub(crate) const fn record_native_loops(&mut self, n: u32) {
        self.loops = self.loops.saturating_add(n);
    }

    #[cfg(test)]
    pub(crate) const fn calls(&self) -> u32 {
        self.calls
    }

    #[cfg(test)]
    pub(crate) const fn loops(&self) -> u32 {
        self.loops
    }

    /// Whether the first gate is crossed: the weighted
    /// budget `loops_weight × calls + calls_weight × loops ≥ budget`, with the
    /// entry heavier than a back edge (its weight is the configured loop
    /// threshold).
    pub(crate) const fn crossed_first_gate(&self, th: &HotThresholds) -> bool {
        Self::crossed(&th.first, self.calls, self.loops)
    }

    /// Whether the second gate is crossed.
    pub(crate) const fn crossed_second_gate(&self, th: &HotThresholds) -> bool {
        Self::crossed(&th.tier2, self.calls, self.loops)
    }

    /// One gate's crossing, in `u64` (`weight × count` and the budget are all
    /// the product of two `u32`s).
    const fn crossed(gate: &GateBudget, calls: u32, loops: u32) -> bool {
        gate.calls_weight as u64 * calls as u64 + gate.loops_weight as u64 * loops as u64
            >= gate.budget
    }
}

#[cfg(test)]
mod tests;
