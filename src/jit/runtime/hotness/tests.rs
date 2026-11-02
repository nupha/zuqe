use super::*;

/// The production base plus the second gate.
const BASE: (u32, u32, u32, u32) = (32, 56, 1024, 2048);

/// The crossing checks are the gate's contract; the budget line itself is
/// an implementation detail. Zero counts mean "just below a corner".
fn state(calls: u32, loops: u32) -> HotnessState {
    let mut state = HotnessState::default();
    for _ in 0..calls {
        state.record_call();
    }
    for _ in 0..loops {
        state.record_loop();
    }
    state
}

#[test]
fn without_measurements_the_base_is_unchanged() {
    let th = AdaptiveInputs::default().thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
    assert_eq!(th.reason, HotReason::NeutralBase);
    // T8: the budget line keeps the two OR-gates' corner points — `C`
    // entries alone and `L` back edges alone fire exactly as before.
    assert!(!state(31, 0).crossed_first_gate(&th));
    assert!(state(32, 0).crossed_first_gate(&th));
    assert!(!state(0, 55).crossed_first_gate(&th));
    assert!(state(0, 56).crossed_first_gate(&th));
    assert!(!state(31, 1).crossed_first_gate(&th), "one short of both");
    // The second gate: unchanged corners.
    assert!(!state(1023, 0).crossed_second_gate(&th));
    assert!(state(1024, 0).crossed_second_gate(&th));
    assert!(state(0, 2048).crossed_second_gate(&th));
}

/// T8's point: the two gates are one **weighted budget**, so a function
/// with half of each is hot too (the line cuts the OR-rectangle's corner).
#[test]
fn the_budget_mixes_calls_and_loops() {
    let th = AdaptiveInputs::default().thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
    assert!(!state(15, 28).crossed_first_gate(&th), "just under");
    assert!(
        state(16, 28).crossed_first_gate(&th),
        "16×56 + 28×32 = the budget"
    );
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
    assert_eq!(th.reason, HotReason::HelperDensity);
    assert!(!state(63, 0).crossed_first_gate(&th));
    assert!(state(64, 0).crossed_first_gate(&th));
    assert!(state(0, 112).crossed_first_gate(&th));
    assert!(!state(2047, 0).crossed_second_gate(&th));
    assert!(state(2048, 0).crossed_second_gate(&th));
    assert!(state(0, 4096).crossed_second_gate(&th));
}

#[test]
fn measured_work_lowers_the_base() {
    let inputs = AdaptiveInputs {
        measured_work: Some(4096),
        ..AdaptiveInputs::default()
    };
    let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
    assert_eq!(th.reason, HotReason::MeasuredWork);
    assert!(!state(15, 0).crossed_first_gate(&th));
    assert!(state(16, 0).crossed_first_gate(&th));
    assert!(!state(511, 0).crossed_second_gate(&th));
    assert!(state(512, 0).crossed_second_gate(&th));
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
    assert_eq!(th.reason, HotReason::HelperDensity);
    assert!(!state(63, 0).crossed_first_gate(&th));
    assert!(state(64, 0).crossed_first_gate(&th));
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

/// The size factor is **continuous**: `1 + bytes/1024`,
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
    assert!(!state(63, 0).crossed_first_gate(&th));
    assert!(state(64, 0).crossed_first_gate(&th));
    assert!(!state(0, 111).crossed_first_gate(&th));
    assert!(state(0, 112).crossed_first_gate(&th));
    // The second gate's corners are untouched by the size factor.
    assert!(!state(2047, 0).crossed_second_gate(&th));
    assert!(state(2048, 0).crossed_second_gate(&th));
    // 2 KiB → 3x.
    let th = inputs(2048).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
    assert!(!state(95, 0).crossed_first_gate(&th));
    assert!(state(96, 0).crossed_first_gate(&th));
    // 4 KiB → the 4x cap.
    let th = inputs(4096).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
    assert!(!state(127, 0).crossed_first_gate(&th));
    assert!(state(128, 0).crossed_first_gate(&th));
    assert!(state(0, 224).crossed_first_gate(&th));
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
    assert!(!state(127, 0).crossed_first_gate(&th));
    assert!(state(128, 0).crossed_first_gate(&th));
    assert_eq!(th.reason, HotReason::HelperDensity);
    // 2 KiB → 3x size → 6x, capped at 4x.
    let th = inputs(2048).thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 0);
    assert!(!state(127, 0).crossed_first_gate(&th));
    assert!(state(128, 0).crossed_first_gate(&th));
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
    assert_eq!(th.reason, HotReason::NeutralBase);
    assert!(
        state(1, 0).crossed_first_gate(&th),
        "the host asked for eager"
    );
    assert!(state(0, 1).crossed_first_gate(&th));
}

/// The compile-rate gate: the cost gate is a **floor** under the thresholds. A function
/// that is called a handful of times cannot pay for a compile, so it is
/// not compiled at all — whatever the configured thresholds say. With T8
/// the floor lifts the pair first and the budget line follows it.
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
    assert!(!state(31, 0).crossed_first_gate(&th));
    assert!(state(32, 0).crossed_first_gate(&th));

    // A gate of 300 activations: the call corner moves to 300, the back
    // edge corner keeps the configured 56/32 ratio (525).
    let th = inputs.thresholds(BASE.0, BASE.1, BASE.2, BASE.3, 300);
    assert!(!state(299, 0).crossed_first_gate(&th));
    assert!(state(300, 0).crossed_first_gate(&th));
    assert!(!state(0, 524).crossed_first_gate(&th));
    assert!(state(0, 525).crossed_first_gate(&th));
    // The second gate is a second compile: it gets the same floor, and
    // 300 is below its corners (1024 / 2048), so it is unchanged.
    assert!(!state(1023, 0).crossed_second_gate(&th));
    assert!(state(1024, 0).crossed_second_gate(&th));
    assert!(state(0, 2048).crossed_second_gate(&th));
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
    assert!(state(1, 0).crossed_first_gate(&th));
    assert!(state(0, 1).crossed_first_gate(&th));
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
