use std::hash::Hasher;

use super::*;

use crate::jit::fast_hash::FastHasher;

/// A coordinator with the two gates: the first at `calls`/`loops`, the
/// second at `tier2_calls`/`tier2_loops`.
fn two_tier(
    calls: u32,
    loops: u32,
    tier2_calls: u32,
    tier2_loops: u32,
    max_attempts: u32,
) -> Coordinator {
    Coordinator::new(
        calls,
        loops,
        tier2_calls,
        tier2_loops,
        Tiering::TwoTier,
        500,
        8,
        max_attempts,
        30_000,
        0,
    )
}

fn coordinator(max_attempts: u32) -> Coordinator {
    Coordinator::new(
        1,
        1,
        1024,
        2048,
        Tiering::Single(Tier::Tier2),
        500,
        8,
        max_attempts,
        30_000,
        0,
    )
}

/// Drive a function to hot and queue it (no bytecode pin in tests).
fn make_hot(c: &mut Coordinator, m: &mut JitMetrics, id: u64) {
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];
    let tier = c
        .record_call(m, id, &opcodes, 0)
        .expect("should become hot");
    c.enqueue(m, id, None, tier);
}

/// Simulate `poll` taking the next request off the queue.
fn dispatch(c: &mut Coordinator) {
    let _ = c.begin_next();
}

/// Profitability-gate wiring: the interpreter's measurement reaches the thresholds. A
/// helper-dominated function (every opcode a `Helper`) is expensive to
/// compile, so its static shape doubles the base; proven work (4096
/// observations) halves it instead. An eager base (`1`, what the diagnostic
/// harnesses ask for) is never adapted.
#[test]
fn measured_work_adapts_the_production_base_only() {
    let helper_dense = [crate::bytecode::opcodes::OP_add; 4];
    let mut m = JitMetrics::default();

    // Production base (32/56) + a little measured work: helper density
    // doubles it, so 64 calls are needed instead of 32.
    let mut c = two_tier(32, 56, 1024, 2048, 4);
    for _ in 0..63 {
        assert!(
            c.record_call(&mut m, 1, &helper_dense, 8).is_none(),
            "64 calls are needed"
        );
    }
    assert!(
        c.record_call(&mut m, 1, &helper_dense, 8).is_some(),
        "the 64th call is hot"
    );

    // Proven work (4096 observations) halves the base instead: the 16th.
    let mut c = two_tier(32, 56, 1024, 2048, 4);
    for _ in 0..15 {
        assert!(c.record_call(&mut m, 4, &helper_dense, 4096).is_none());
    }
    assert!(
        c.record_call(&mut m, 4, &helper_dense, 4096).is_some(),
        "16th call"
    );

    // Same function, no measurement (a baseline tier): the *static* helper
    // density still doubles the base, so 64 calls are needed.
    let mut c = two_tier(32, 56, 1024, 2048, 4);
    for _ in 0..63 {
        assert!(c.record_call(&mut m, 2, &helper_dense, 0).is_none());
    }
    assert!(
        c.record_call(&mut m, 2, &helper_dense, 0).is_some(),
        "the static shape applies without a measurement"
    );

    // Eager base: adaptation is skipped, so the first call queues it.
    let mut c = coordinator(4);
    assert!(c.record_call(&mut m, 3, &helper_dense, 4096).is_some());
}

#[test]
fn entry_crosses_threshold_once() {
    let mut c = two_tier(2, 2, 1024, 2048, 4);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];
    assert!(c.record_call(&mut m, 7, &opcodes, 0).is_none());
    assert!(c.record_call(&mut m, 7, &opcodes, 0).is_some());
    assert!(
        c.record_call(&mut m, 7, &opcodes, 0).is_none(),
        "already queued"
    );
    assert_eq!(m.distinct_functions, 1);
}

/// T8 end to end: the two interpreter hooks feed **one** weighted budget.
/// 16 entries plus 28 back edges cross the 32/56 line although neither
/// counter has reached its own corner (16 < 32, 28 < 56) — the wiring the
/// `hotness` unit tests cannot see.
#[test]
fn mixed_calls_and_loops_share_one_budget() {
    let mut c = two_tier(32, 56, 1024, 2048, 4);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];
    for _ in 0..15 {
        assert!(c.record_call(&mut m, 7, &opcodes, 0).is_none());
    }
    for _ in 0..28 {
        assert!(
            c.record_loop(&mut m, 7, &opcodes, 0).is_none(),
            "15×56 + 28×32 is still under the budget"
        );
    }
    assert!(
        c.record_call(&mut m, 7, &opcodes, 0).is_some(),
        "16×56 + 28×32 crosses the budget with neither corner reached"
    );
}

/// The map's hash has to spread the node addresses over the buckets.
/// They are aligned pointers, so a hash that keeps their low bits would
/// file them all under one bucket and turn every lookup into a walk over
/// the same chain. The mixer itself is pinned in `jit::fast_hash`.
#[test]
fn the_pointer_hash_spreads_aligned_keys() {
    let hash_of = |ptr: u64| {
        let mut h = FastHasher::default();
        h.write_u64(ptr);
        h.finish()
    };
    // The addresses the GC hands out: aligned, and a page apart.
    let keys = [
        0x1000u64, 0x2000, 0x3000, 0x4000, 0x5000, 0x6000, 0x7000, 0x8000,
    ];
    // The low bits pick the bucket (128 of them is a small table).
    let buckets: std::collections::HashSet<u64> = keys.iter().map(|k| hash_of(*k) & 0x7f).collect();
    assert!(
        buckets.len() > keys.len() / 2,
        "the hash clusters the keys: {buckets:?}"
    );
}

/// A queue of one: the newcomer takes the place of the queued function,
/// which has not run since it was queued. The queue keeps its length and
/// the dropped request is counted.
#[test]
fn a_full_queue_drops_the_idle_function() {
    let mut c = Coordinator::new(
        1,
        1,
        1024,
        2048,
        Tiering::Single(Tier::Tier2),
        500,
        1,
        4,
        30_000,
        0,
    );
    let mut m = JitMetrics::default();
    make_hot(&mut c, &mut m, 1);
    // Second hot function: the queue is full, so the idle one is dropped.
    make_hot(&mut c, &mut m, 2);
    assert_eq!(m.queue_drops, 1);
    assert_eq!(m.functions_queued, 2);
    assert_eq!(c.queue_len(), 1);
}

/// The queue admission policy: a full queue does not drop the newcomer
/// blindly. A queued function that has **stopped running** is dropped
/// instead, so the compile budget goes to the function that is still hot.
#[test]
fn a_full_queue_drops_an_idle_function_for_the_newcomer() {
    let mut c = Coordinator::new(
        1,
        1,
        1024,
        2048,
        Tiering::Single(Tier::Tier2),
        500,
        2,
        4,
        30_000,
        0,
    );
    let mut m = JitMetrics::default();
    make_hot(&mut c, &mut m, 1);
    make_hot(&mut c, &mut m, 2);
    assert_eq!(c.queue_len(), 2, "the queue is full");

    // The third function is hot and takes the place of the first queued
    // one, which has not run since it was queued.
    make_hot(&mut c, &mut m, 3);
    assert_eq!(m.queue_drops, 1, "the idle function was dropped");
    assert_eq!(c.queue_len(), 2, "the queue keeps its length");
    assert_eq!(m.functions_queued, 3);

    // The evicted function is queued again by its next call: it keeps its
    // hotness counters.
    let tier = c
        .record_call(&mut m, 1, &[crate::bytecode::opcodes::OP_return_undef], 0)
        .expect("still hot");
    c.enqueue(&mut m, 1, None, tier);
    assert_eq!(m.functions_queued, 4);
}

/// When every queued function is still running, the newcomer is the
/// coldest one and is dropped — the live functions are kept.
#[test]
fn a_full_queue_keeps_the_functions_that_are_still_running() {
    let mut c = Coordinator::new(
        1,
        1,
        1024,
        2048,
        Tiering::Single(Tier::Tier2),
        500,
        2,
        4,
        30_000,
        0,
    );
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];
    make_hot(&mut c, &mut m, 1);
    make_hot(&mut c, &mut m, 2);
    // Both queued functions run again: they are not idle any more.
    c.record_call(&mut m, 1, &opcodes, 0);
    c.record_call(&mut m, 2, &opcodes, 0);

    make_hot(&mut c, &mut m, 3);
    assert_eq!(m.queue_drops, 1, "the newcomer was dropped");
    assert_eq!(c.queue_len(), 2);
    assert_eq!(m.functions_queued, 2, "the newcomer did not get in");
}

/// A pending tier-2 promotion that is evicted keeps the installed code:
/// only the promotion request is dropped, and the second gate fires again
/// on the next activation.
#[test]
fn evicting_a_pending_promotion_keeps_the_installed_code() {
    let mut c = Coordinator::new(1, 1, 2, 2, Tiering::TwoTier, 500, 1, 4, 30_000, 0);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];

    // The baseline tier is installed.
    let tier = c.record_call(&mut m, 1, &opcodes, 0).expect("hot");
    c.enqueue(&mut m, 1, None, tier);
    dispatch(&mut c);
    c.complete(&mut m, 1, CompileOutcome::Installed);
    assert_eq!(c.tier(1), Tier::Tier1);

    // The second gate queues the promotion.
    let tier = c.record_native_call(1, 2).expect("promoted");
    c.enqueue(&mut m, 1, None, tier);
    assert_eq!(c.queue_len(), 1, "the queue is full");

    // A newcomer takes the place of the idle promotion.
    make_hot(&mut c, &mut m, 2);
    assert_eq!(m.queue_drops, 1);
    assert_eq!(c.queue_len(), 1);
    assert_eq!(c.tier(1), Tier::Tier1, "the installed code is still there");

    // The second gate fires again on the next activation.
    assert!(
        c.record_native_call(1, 2).is_some(),
        "the promotion is queued again"
    );
}

#[test]
fn failure_retries_then_settles() {
    let mut c = coordinator(2);
    let mut m = JitMetrics::default();
    make_hot(&mut c, &mut m, 9);
    dispatch(&mut c);

    c.complete(&mut m, 9, CompileOutcome::Failed);
    assert_eq!(m.functions_retried, 1);
    assert_eq!(m.functions_settled, 0);
    assert_eq!(c.queue_len(), 1, "requeued");

    dispatch(&mut c);
    c.complete(&mut m, 9, CompileOutcome::Failed);
    assert_eq!(m.functions_settled, 1);
    assert_eq!(c.queue_len(), 0, "out of attempts");
    assert_eq!(m.functions_verified, 1, "verified counted once");
}

#[test]
fn rejected_settles_without_retry() {
    let mut c = coordinator(4);
    let mut m = JitMetrics::default();
    make_hot(&mut c, &mut m, 3);
    dispatch(&mut c);
    c.complete(&mut m, 3, CompileOutcome::Rejected);
    assert_eq!(m.functions_rejected, 1);
    assert_eq!(m.functions_retried, 0);
    assert_eq!(c.queue_len(), 0);
}

#[test]
fn installed_counts_verified_and_compiled() {
    let mut c = coordinator(4);
    let mut m = JitMetrics::default();
    make_hot(&mut c, &mut m, 4);
    dispatch(&mut c);
    c.complete(&mut m, 4, CompileOutcome::Installed);
    assert_eq!(m.functions_verified, 1);
    assert_eq!(m.functions_compiled, 1);
    // The B/C invariant: every queued function is verified or rejected.
    assert_eq!(
        m.functions_verified + m.functions_rejected,
        m.functions_queued
    );
}

/// The first gate compiles the baseline tier, the second one
/// promotes the function to the optimizing tier. The promotion is driven
/// by the counts the *native* tier reports back, because the interpreter
/// stops counting once the code is installed.
#[test]
fn the_second_gate_promotes_a_compiled_function() {
    let mut c = two_tier(2, 2, 4, 8, 4);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];

    // Two calls cross the first gate: the baseline tier.
    assert!(c.record_call(&mut m, 1, &opcodes, 0).is_none());
    let tier = c.record_call(&mut m, 1, &opcodes, 0).expect("hot");
    assert_eq!(tier, Tier::Tier1);
    c.enqueue(&mut m, 1, None, tier);
    dispatch(&mut c);
    c.complete(&mut m, 1, CompileOutcome::Installed);
    assert_eq!(
        c.tier(1),
        Tier::Tier1,
        "the baseline tier does not speculate"
    );

    // The native tier reports one more activation: still below the
    // second gate (4 calls).
    assert!(c.record_native_call(1, 1).is_none());
    // ... and the back edges of a hot loop cross it.
    let tier = c.record_native_loops(1, 8).expect("promoted");
    assert_eq!(tier, Tier::Tier2);
    c.enqueue(&mut m, 1, None, tier);
    dispatch(&mut c);
    c.complete(&mut m, 1, CompileOutcome::Installed);
    assert_eq!(c.tier(1), Tier::Tier2, "the optimizing tier speculates");
    assert_eq!(m.tier2_promotions, 1);
    assert_eq!(m.functions_recompiled, 1);
    assert_eq!(m.functions_compiled, 2);
}

/// A demoted function (tier-down) must not be promoted right
/// back — the cooldown keeps the two recompiles from chasing each other.
#[test]
fn a_demoted_function_waits_before_it_is_promoted_again() {
    let mut c = two_tier(1, 1, 2, 2, 4);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];

    // First gate: the baseline tier.
    let tier = c.record_call(&mut m, 1, &opcodes, 0).expect("hot");
    assert_eq!(tier, Tier::Tier1);
    c.enqueue(&mut m, 1, None, tier);
    dispatch(&mut c);
    c.complete(&mut m, 1, CompileOutcome::Installed);
    assert_eq!(c.tier(1), Tier::Tier1);

    // Second gate: the optimizing tier.
    let tier = c.record_native_call(1, 2).expect("promoted");
    assert_eq!(tier, Tier::Tier2);
    c.enqueue(&mut m, 1, None, tier);
    dispatch(&mut c);
    c.complete(&mut m, 1, CompileOutcome::Installed);
    assert_eq!(c.tier(1), Tier::Tier2);

    // The guards miss too often: the function is demoted.
    c.note_guard_misses(1, TIER_DOWN_MISSES as u32);
    assert!(c.needs_tier_down_requeue(1));
    c.enqueue(&mut m, 1, None, Tier::Tier1);
    dispatch(&mut c);
    c.complete(&mut m, 1, CompileOutcome::Installed);
    assert_eq!(c.tier(1), Tier::Tier1);
    assert_eq!(m.tier_downs, 1);

    // The second gate is crossed, but the cooldown blocks the promotion.
    assert!(c.record_native_call(1, 100).is_none());
    // ... until enough new invocations have passed.
    assert!(c.record_native_call(1, 500).is_some(), "cooldown over");
}

/// The record table must track the live set, not every function the
/// runtime ever made hot: a sweep drops the records that did not run and
/// hold no code, and keeps the ones that are still in use.
#[test]
fn a_sweep_drops_idle_records_and_keeps_code() {
    let mut c = coordinator(2);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];

    // Four seen functions; the sweep runs (the table reached the
    // threshold) and keeps them because they all ran.
    c.prune_at = 4;
    for id in 1..=4 {
        let _ = c.record_call(&mut m, id, &opcodes, 0);
    }
    c.prune_if_grown();
    assert_eq!(c.record_len(), 4, "a sweep dropped live records");
    assert!(c.prune_at() > 4, "the threshold must grow after a sweep");

    // Function 2 gets code installed; 1 keeps running; 3 and 4 go idle.
    c.records.get_mut(&2).unwrap().tier = Tier::Tier1;
    let _ = c.record_call(&mut m, 1, &opcodes, 0);
    c.prune_at = 4;
    c.prune_if_grown();

    assert!(c.records.contains_key(&1), "a running function was dropped");
    assert!(
        c.records.contains_key(&2),
        "a function with installed code was dropped"
    );
    assert!(!c.records.contains_key(&3), "an idle record survived");
    assert!(!c.records.contains_key(&4), "an idle record survived");
}

/// A sweep only runs once the table has doubled since the previous one, so
/// the pass is amortized O(1) per new record.
#[test]
fn the_sweep_threshold_doubles() {
    let mut c = coordinator(2);
    let mut m = JitMetrics::default();
    let opcodes = [crate::bytecode::opcodes::OP_return_undef];

    c.prune_at = 2;
    let _ = c.record_call(&mut m, 1, &opcodes, 0);
    let _ = c.record_call(&mut m, 2, &opcodes, 0);
    assert_eq!(c.prune_at(), 2, "no sweep yet");
    c.prune_if_grown();
    assert_eq!(c.prune_at(), MIN_RECORDS);
    // Fill to the new threshold; every record runs in this epoch, so the
    // next sweep keeps all of them and doubles again.
    for id in 1..=MIN_RECORDS as u64 {
        let _ = c.record_call(&mut m, id, &opcodes, 0);
    }
    c.prune_if_grown();
    assert_eq!(c.prune_at(), MIN_RECORDS * 2, "the threshold doubles");
}
