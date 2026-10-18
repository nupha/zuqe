// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Hotness state machine (epic D1).
//!
//! The interpreter counts entries and backward branches here; when a function
//! crosses its threshold it is queued for compilation exactly once. Thresholds
//! start from the configured base. The function's static shape (bytecode size,
//! helper density) can raise them on any tier; measured work — the Tier 2
//! feedback table (epic F1) supplies it — can lower them further.

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

/// Entry/backedge counts a function must reach before it is queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HotThresholds {
    pub(crate) calls: u32,
    pub(crate) loops: u32,
    pub(crate) reason: HotReason,
}

impl HotThresholds {
    pub(crate) const fn neutral(calls: u32, loops: u32) -> Self {
        Self {
            calls,
            loops,
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

    /// Larger and helper-dominated functions need more evidence before we
    /// spend compile time on them; functions already shown to do a lot of work
    /// can be compiled sooner. The static shape (bytecode size / helper
    /// density) is read unconditionally, so it also guides a tier that records
    /// no measurements; the measured work only lowers the base.
    ///
    /// An eager base (both thresholds 1) is left alone: the host asked for
    /// "compile on the first event", and doubling that would silently defeat
    /// the diagnostic/test harnesses that rely on it.
    pub(crate) fn thresholds(&self, base_calls: u32, base_loops: u32) -> HotThresholds {
        if base_calls <= 1 && base_loops <= 1 {
            return HotThresholds::neutral(base_calls, base_loops);
        }
        // Proven work outranks the static shape: a function observed doing a lot
        // of interpreter work is worth compiling even if it is large.
        if self.measured_work.is_some_and(|work| work >= 4096) {
            return HotThresholds {
                calls: base_calls / 2,
                loops: base_loops / 2,
                reason: HotReason::MeasuredWork,
            };
        }
        if self.instruction_count != 0
            && self.helper_ops.saturating_mul(4) >= self.instruction_count
        {
            return HotThresholds {
                calls: base_calls.saturating_mul(2),
                loops: base_loops.saturating_mul(2),
                reason: HotReason::HelperDensity,
            };
        }
        if self.bytecode_bytes >= 4096 {
            return HotThresholds {
                calls: base_calls.saturating_mul(2),
                loops: base_loops.saturating_mul(2),
                reason: HotReason::BytecodeScale,
            };
        }
        HotThresholds::neutral(base_calls, base_loops)
    }
}

/// Per-function hotness counters. `queued` makes the "queue exactly once"
/// invariant explicit: once set, additional hot events are ignored until the
/// coordinator clears it (retry, invalidation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HotnessState {
    calls: u32,
    loops: u32,
    queued: bool,
}

impl HotnessState {
    /// Count one function entry; returns true when this is the event that makes
    /// the function hot and it should be enqueued.
    pub(crate) fn record_call(&mut self, thresholds: &HotThresholds) -> bool {
        self.calls = self.calls.saturating_add(1);
        self.decide(self.calls >= thresholds.calls)
    }

    /// Count one taken backward branch.
    pub(crate) fn record_loop(&mut self, thresholds: &HotThresholds) -> bool {
        self.loops = self.loops.saturating_add(1);
        self.decide(self.loops >= thresholds.loops)
    }

    fn decide(&mut self, hot: bool) -> bool {
        if hot && !self.queued {
            self.queued = true;
            true
        } else {
            false
        }
    }

    pub(crate) fn clear_queued(&mut self) {
        self.queued = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_measurements_the_base_is_unchanged() {
        let th = AdaptiveInputs::default().thresholds(32, 56);
        assert_eq!(th.calls, 32);
        assert_eq!(th.loops, 56);
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
        let th = inputs.thresholds(32, 56);
        assert_eq!((th.calls, th.loops), (64, 112));
        assert_eq!(th.reason, HotReason::HelperDensity);
    }

    #[test]
    fn measured_work_lowers_the_base() {
        let inputs = AdaptiveInputs {
            measured_work: Some(4096),
            ..AdaptiveInputs::default()
        };
        let th = inputs.thresholds(32, 56);
        assert_eq!(th.calls, 16);
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
        let th = inputs.thresholds(32, 56);
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
        assert_eq!(inputs.thresholds(32, 56).reason, HotReason::BytecodeScale);
    }

    #[test]
    fn an_eager_base_is_never_adapted() {
        let inputs = AdaptiveInputs {
            instruction_count: 10,
            helper_ops: 10,
            bytecode_bytes: 8192,
            measured_work: Some(4096),
        };
        let th = inputs.thresholds(1, 1);
        assert_eq!((th.calls, th.loops), (1, 1), "the host asked for eager");
        assert_eq!(th.reason, HotReason::NeutralBase);
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

    #[test]
    fn crosses_each_threshold_exactly_once() {
        let th = HotThresholds::neutral(2, 2);
        let mut state = HotnessState::default();
        assert!(!state.record_call(&th));
        assert!(state.record_call(&th), "second entry crosses");
        assert!(!state.record_call(&th), "already queued");
        assert!(!state.record_loop(&th), "below the loop threshold");
        state.clear_queued();
        assert!(state.record_loop(&th), "loop threshold after clear");
        assert!(!state.record_loop(&th), "queued again");
    }
}
