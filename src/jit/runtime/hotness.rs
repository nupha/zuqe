// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Hotness state machine (epic D1).
//!
//! The interpreter counts entries and backward branches here; when a function
//! crosses its threshold it is queued for compilation exactly once. Thresholds
//! start from the configured base. The function's static shape (bytecode size,
//! helper density) can raise them on any tier; measured work — the Tier 2
//! feedback table (epic F1) supplies it — can lower them further.
//!
//! Two gates, two tiers (plan §13.6.5 item 1): the first gate compiles the
//! **baseline** tier, the second one promotes the function to the **optimizing**
//! tier. The counters behind both gates are the same, but the second gate must
//! also see the work a function does *after* it was compiled — the interpreter
//! stops counting then, so the native tier reports its own activations and back
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

/// The tier a function's code is at (plan §13.6.5 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    /// Not compiled yet.
    Cold,
    /// Baseline: compiled without the tier-2 specialization.
    Tier1,
    /// Optimizing: compiled with the tier-2 specialization.
    Tier2,
}

/// Entry/backedge counts a function must reach before it is queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HotThresholds {
    pub(crate) calls: u32,
    pub(crate) loops: u32,
    /// The second gate: the counts that promote an already-compiled function
    /// to the optimizing tier.
    pub(crate) tier2_calls: u32,
    pub(crate) tier2_loops: u32,
    pub(crate) reason: HotReason,
}

/// The size factor's fixed-point scale: the factor is kept in 1/16ths so the
/// thresholds stay exact integers.
const SIZE_SCALE: u32 = 16;
/// The cap of the size factor: 4x the base (plan §13.6.5 item 2).
const SIZE_FACTOR_MAX: u32 = 4 * SIZE_SCALE;

impl HotThresholds {
    pub(crate) const fn neutral(
        calls: u32,
        loops: u32,
        tier2_calls: u32,
        tier2_loops: u32,
    ) -> Self {
        Self {
            calls,
            loops,
            tier2_calls,
            tier2_loops,
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
    /// feedback exists (epic F1); while it is `None` the thresholds are the
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

    /// Attach the interpreter's measurement (epic F5): the hottest hooks pass
    /// the feedback table's observation count. `0` means "nothing recorded"
    /// (a baseline tier never records), which keeps the thresholds neutral
    /// instead of pretending the function was measured as doing no work.
    pub(crate) const fn with_measured_work(mut self, samples: u32) -> Self {
        self.measured_work = if samples == 0 { None } else { Some(samples) };
        self
    }

    /// The continuous size factor (plan §13.6.5 item 2): `1 + bytes/1024`,
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
    /// `payback` is the **cost gate** (P1-3): the number of activations a
    /// function must be observed at before its compile can pay for itself,
    /// derived from the measured cost of a compile and the host's estimate of
    /// what native execution saves. It is a floor under the thresholds, so a
    /// function that is called a handful of times is not compiled at all.
    ///
    /// An eager base (both thresholds 1) is left alone: the host asked for
    /// "compile on the first event", and raising that would silently defeat
    /// the diagnostic/test harnesses that rely on it.
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
        // of interpreter work is worth compiling even if it is large.
        if self.measured_work.is_some_and(|work| work >= 4096) {
            return HotThresholds {
                calls: base_calls / 2,
                loops: base_loops / 2,
                tier2_calls: base_tier2_calls / 2,
                tier2_loops: base_tier2_loops / 2,
                reason: HotReason::MeasuredWork,
            };
        }
        let helper_dense = self.helper_dense();
        let size = self.size_factor();
        let factor = if helper_dense {
            size.saturating_mul(2).min(SIZE_FACTOR_MAX)
        } else {
            size
        };
        let mut thresholds = if factor == SIZE_SCALE {
            HotThresholds::neutral(base_calls, base_loops, base_tier2_calls, base_tier2_loops)
        } else {
            HotThresholds {
                calls: base_calls.saturating_mul(factor) / SIZE_SCALE,
                loops: base_loops.saturating_mul(factor) / SIZE_SCALE,
                tier2_calls: base_tier2_calls.saturating_mul(factor) / SIZE_SCALE,
                tier2_loops: base_tier2_loops.saturating_mul(factor) / SIZE_SCALE,
                reason: if helper_dense {
                    HotReason::HelperDensity
                } else {
                    HotReason::BytecodeScale
                },
            }
        };
        // The cost gate: a floor under the first gate (the compile has to pay
        // for itself) and under the second one (the promotion is a second
        // compile). The back-edge gate keeps the configured ratio to the call
        // gate: a back edge is a smaller unit of work than a call.
        if payback > 0 {
            thresholds.calls = thresholds.calls.max(payback);
            thresholds.loops = thresholds
                .loops
                .max(payback.saturating_mul(base_loops) / base_calls.max(1));
            thresholds.tier2_calls = thresholds.tier2_calls.max(payback);
            thresholds.tier2_loops = thresholds
                .tier2_loops
                .max(payback.saturating_mul(base_tier2_loops) / base_tier2_calls.max(1));
        }
        thresholds
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

    /// Whether the first gate is crossed.
    pub(crate) const fn crossed_first_gate(&self, th: &HotThresholds) -> bool {
        self.calls >= th.calls || self.loops >= th.loops
    }

    /// Whether the second gate is crossed.
    pub(crate) const fn crossed_second_gate(&self, th: &HotThresholds) -> bool {
        self.calls >= th.tier2_calls || self.loops >= th.tier2_loops
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The production base plus the second gate (plan §13.6.5 item 1).
    const BASE: (u32, u32, u32, u32) = (32, 56, 1024, 2048);

    #[test]
    fn without_measurements_the_base_is_unchanged() {
        let th = AdaptiveInputs::default().thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!(th.calls, 32);
        assert_eq!(th.loops, 56);
        assert_eq!((th.tier2_calls, th.tier2_loops), (1024, 2048));
        assert_eq!(th.reason, HotReason::NeutralBase);
    }

    /// The static shape is read even when no measurement exists (a baseline
    /// tier), so the audit's "dead input" concern cannot come back: a
    /// helper-dominated function doubles its base with `measured_work: None`.
    #[test]
    fn a_helper_dense_function_doubles_without_a_measurement() {
        let inputs = AdaptiveInputs {
            instruction_count: 10,
            helper_ops: 3,
            measured_work: None,
            ..AdaptiveInputs::default()
        };
        let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (64, 112));
        assert_eq!((th.tier2_calls, th.tier2_loops), (2048, 4096));
        assert_eq!(th.reason, HotReason::HelperDensity);
    }

    #[test]
    fn measured_work_lowers_the_base() {
        let inputs = AdaptiveInputs {
            measured_work: Some(4096),
            ..AdaptiveInputs::default()
        };
        let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!(th.calls, 16);
        assert_eq!(th.tier2_calls, 512);
        assert_eq!(th.reason, HotReason::MeasuredWork);
    }

    #[test]
    fn helper_heavy_inputs_double_the_base() {
        let inputs = AdaptiveInputs {
            instruction_count: 10,
            helper_ops: 3, // 3*4 >= 10
            measured_work: Some(1),
            ..AdaptiveInputs::default()
        };
        let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!(th.calls, 64);
        assert_eq!(th.reason, HotReason::HelperDensity);
    }

    #[test]
    fn large_bytecode_doubles_the_base() {
        let inputs = AdaptiveInputs {
            instruction_count: 100,
            helper_ops: 0,
            bytecode_bytes: 4096,
            measured_work: Some(1),
        };
        assert_eq!(
            inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0).reason,
            HotReason::BytecodeScale
        );
    }

    /// The size factor is **continuous** (plan §13.6.5 item 2): `1 + bytes/1024`,
    /// capped at 4x. The old rule was a one-shot `bytes >= 4096 → ×2`, so a
    /// 500-byte and a 4000-byte function got the same treatment.
    #[test]
    fn the_size_factor_scales_continuously() {
        let factor = |bytes: u32| {
            AdaptiveInputs {
                bytecode_bytes: bytes,
                ..AdaptiveInputs::default()
            }
            .size_factor()
        };
        assert_eq!(factor(0), 16, "1.0x");
        assert_eq!(factor(512), 24, "1.5x");
        assert_eq!(factor(1024), 32, "2.0x");
        assert_eq!(factor(2048), 48, "3.0x");
        assert_eq!(factor(3072), 64, "4.0x — the cap");
        assert_eq!(factor(8192), 64, "still the cap");
    }

    #[test]
    fn a_bigger_function_waits_longer() {
        let inputs = |bytes| AdaptiveInputs {
            instruction_count: 100,
            helper_ops: 0,
            bytecode_bytes: bytes,
            measured_work: Some(1),
        };
        // 1 KiB → 2x: 64 calls / 112 back edges.
        let th = inputs(1024).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (64, 112));
        assert_eq!((th.tier2_calls, th.tier2_loops), (2048, 4096));
        // 2 KiB → 3x.
        let th = inputs(2048).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (96, 168));
        // 4 KiB → the 4x cap.
        let th = inputs(4096).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (128, 224));
    }

    /// A helper-dominated function doubles the size factor on top, and the
    /// total stays capped at 4x.
    #[test]
    fn a_helper_dense_function_doubles_the_size_factor() {
        let inputs = |bytes| AdaptiveInputs {
            instruction_count: 100,
            helper_ops: 25,
            bytecode_bytes: bytes,
            measured_work: Some(1),
        };
        // 1 KiB → 2x size → 4x total.
        let th = inputs(1024).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (128, 224));
        assert_eq!(th.reason, HotReason::HelperDensity);
        // 2 KiB → 3x size → 6x, capped at 4x.
        let th = inputs(2048).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (128, 224));
    }

    #[test]
    fn an_eager_base_is_never_adapted() {
        let inputs = AdaptiveInputs {
            instruction_count: 10,
            helper_ops: 10,
            bytecode_bytes: 8192,
            measured_work: Some(4096),
        };
        let th = inputs.thresholds(1, 1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (1, 1), "the host asked for eager");
        assert_eq!(th.reason, HotReason::NeutralBase);
    }

    /// P1-3: the cost gate is a **floor** under the thresholds. A function
    /// that is called a handful of times cannot pay for a compile, so it is
    /// not compiled at all — whatever the configured thresholds say.
    #[test]
    fn the_cost_gate_raises_the_thresholds_to_the_payback() {
        let inputs = AdaptiveInputs {
            instruction_count: 10,
            helper_ops: 0,
            bytecode_bytes: 0,
            measured_work: Some(1),
        };
        // No gate: the configured base.
        let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
        assert_eq!((th.calls, th.loops), (32, 56));

        // A gate of 300 activations: the call gate moves to 300, the back
        // edge gate keeps the configured 56/32 ratio (525).
        let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 300);
        assert_eq!((th.calls, th.loops), (300, 525));
        // The second gate is a second compile: it gets the same floor.
        assert_eq!((th.tier2_calls, th.tier2_loops), (1024, 2048));
    }

    /// The eager base is exempt: the host asked for the code at the first
    /// event, and the cost gate would silently defeat that.
    #[test]
    fn the_cost_gate_does_not_touch_an_eager_base() {
        let inputs = AdaptiveInputs {
            instruction_count: 10,
            helper_ops: 0,
            bytecode_bytes: 100,
            measured_work: Some(1),
        };
        let th = inputs.thresholds(1, 1, BASE.2, BASE.3, 300);
        assert_eq!((th.calls, th.loops), (1, 1));
    }

    #[test]
    fn zero_samples_means_no_measurement() {
        let inputs = AdaptiveInputs::from_opcodes(&[crate::bytecode::opcodes::OP_add]);
        assert!(inputs.measured_work.is_none());
        let with = inputs.with_measured_work(0);
        assert!(
            with.measured_work.is_none(),
            "0 is 'nothing recorded', not 'no work'"
        );
        assert_eq!(with.with_measured_work(7).measured_work, Some(7));
    }

    #[test]
    fn counts_the_opcode_stream() {
        let inputs = AdaptiveInputs::from_opcodes(&[crate::bytecode::opcodes::OP_add]);
        assert_eq!(inputs.instruction_count, 1);
        assert_eq!(inputs.helper_ops, 1);
        assert!(inputs.measured_work.is_none());
    }

    /// The two gates are independent: the first one is crossed by the counts
    /// the interpreter sees, the second one by the counts the *native* tier
    /// reports back (the interpreter stops counting once the code is
    /// installed).
    #[test]
    fn the_two_gates_are_counted_separately() {
        let th = HotThresholds::neutral(2, 2, 10, 10);
        let mut state = HotnessState::default();
        state.record_call();
        assert!(!state.crossed_first_gate(&th));
        state.record_call();
        assert!(state.crossed_first_gate(&th), "the second entry crosses");
        assert!(!state.crossed_second_gate(&th));

        // The native tier reports its own work into the same counters.
        state.record_native_loops(10);
        assert!(state.crossed_second_gate(&th), "the native back edges do");
        assert_eq!(state.loops(), 10);
        assert_eq!(state.calls(), 2);
    }
}
