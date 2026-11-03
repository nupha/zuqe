// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

use crate::{
    bytecode::{arithmatic::TwoNumbers, opcodes as op},
    jit::test_harness::{
        self, Asm, FALSE as FALSE_BITS, NULL as NULL_BITS, TRUE as TRUE_BITS,
        UNDEFINED as UNDEFINED_BITS, float, int,
    },
};

fn bytes(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn u16b(value: u16) -> [u8; 2] {
    value.to_le_bytes()
}

fn i32b(value: i32) -> [u8; 4] {
    value.to_le_bytes()
}

fn local(op: u8, index: u16) -> [u8; 3] {
    let mut out = [0; 3];
    out[0] = op;
    out[1..].copy_from_slice(&u16b(index));
    out
}

fn push(value: i32) -> [u8; 5] {
    let mut out = [0; 5];
    out[0] = op::OP_push_i32;
    out[1..].copy_from_slice(&i32b(value));
    out
}

fn branch(op: u8, relative: i32) -> [u8; 5] {
    let mut out = [0; 5];
    out[0] = op;
    out[1..].copy_from_slice(&i32b(relative));
    out
}

/// Run one hand-written body through [`test_harness::run_inline`].
fn run(body: Vec<u8>, args: u16, vars: u16, values: &[u64]) -> Option<u64> {
    test_harness::run_inline(&body, args, vars, values.len() as u32, values, false)
}

fn assert_body(body: Vec<u8>, args: u16, vars: u16, values: &[u64], expected: u64) {
    assert_eq!(run(body, args, vars, values), Some(expected));
}

/// Bitwise semantics oracle: the interpreter's own [`TwoNumbers`] impl —
/// the single source for ToInt32 and result boxing. The lowered body is
/// compared against it (never against hand-written tags), so the JIT and
/// the interpreter cannot drift apart silently.
fn interpreter_bits(opcode: u8, a: i32, b: i32) -> u64 {
    let nums = TwoNumbers::Int(a, b);
    match opcode {
        op::OP_and => nums.and(),
        op::OP_or => nums.or(),
        op::OP_xor => nums.xor(),
        op::OP_shl => nums.shl(),
        op::OP_shr => nums.shr(),
        op::OP_sar => nums.sar(),
        other => unreachable!("not a bitwise opcode: {other}"),
    }
    .0
}

fn assert_bitwise(opcode: u8, a: i32, b: i32) {
    let body = bytes(&[&push(a), &push(b), &[opcode], &[op::OP_return]]);
    assert_eq!(
        run(body, 0, 0, &[]),
        Some(interpreter_bits(opcode, a, b)),
        "bitwise opcode {opcode} disagrees with the interpreter: {a}, {b}"
    );
}

/// Opcodes the JS frontend does not emit in the shapes the table covers:
/// `undefined`, `set_arg`, `set_loc`, `dup`, `drop`, `nop`, both return
/// forms, and both branch kinds.
#[test]
fn hand_written_undefined_and_stack_shapes() {
    assert_body(
        vec![op::OP_undefined, op::OP_return],
        0,
        0,
        &[],
        UNDEFINED_BITS,
    );
    assert_body(vec![op::OP_null, op::OP_return], 0, 0, &[], NULL_BITS);
    assert_body(vec![op::OP_push_true, op::OP_return], 0, 0, &[], TRUE_BITS);
    assert_body(
        vec![op::OP_push_false, op::OP_return],
        0,
        0,
        &[],
        FALSE_BITS,
    );
    assert_body(vec![op::OP_return_undef], 0, 0, &[], UNDEFINED_BITS);
    assert_body(
        bytes(&[&[op::OP_nop], &push(3), &[op::OP_return]]),
        0,
        0,
        &[],
        int(3),
    );
    assert_body(
        bytes(&[&push(1), &[op::OP_drop], &push(4), &[op::OP_return]]),
        0,
        0,
        &[],
        int(4),
    );
    assert_body(
        bytes(&[&push(7), &[op::OP_dup, op::OP_add, op::OP_return]]),
        0,
        0,
        &[],
        int(14),
    );
}

/// `set_arg`/`set_loc` keep the value on the stack; `put_*` pop it. The JS
/// optimizer may rewrite these away, so only hand-written bytecode pins
/// them.
#[test]
fn hand_written_argument_and_local_writes() {
    assert_body(
        bytes(&[
            &push(5),
            &local(op::OP_set_arg, 0),
            &[op::OP_drop],
            &local(op::OP_get_arg, 0),
            &[op::OP_return],
        ]),
        1,
        0,
        &[int(1)],
        int(5),
    );
    assert_body(
        bytes(&[
            &push(6),
            &local(op::OP_put_arg, 0),
            &local(op::OP_get_arg, 0),
            &[op::OP_return],
        ]),
        1,
        0,
        &[int(1)],
        int(6),
    );
    assert_body(
        bytes(&[
            &push(7),
            &local(op::OP_set_loc, 0),
            &[op::OP_drop],
            &local(op::OP_get_loc, 0),
            &[op::OP_return],
        ]),
        0,
        1,
        &[],
        int(7),
    );
    assert_body(
        bytes(&[
            &push(8),
            &local(op::OP_put_loc, 0),
            &local(op::OP_get_loc, 0),
            &[op::OP_return],
        ]),
        0,
        1,
        &[],
        int(8),
    );
}

/// Both branch kinds and a forward `goto`, at hand-assembled offsets.
#[test]
fn hand_written_forward_control_flow() {
    // The skipped arm must still be CFG-reachable (the verifier rejects
    // unreachable instructions), so the conditional edge keeps it alive:
    // pc0 push_true; pc1 if_false -> 11; pc6 goto -> 17;
    // pc11 push 1; pc16 return; pc17 push 2; pc22 return.
    let goto = bytes(&[
        &[op::OP_push_true],
        &branch(op::OP_if_false, 9),
        &branch(op::OP_goto, 10),
        &push(1),
        &[op::OP_return],
        &push(2),
        &[op::OP_return],
    ]);
    assert_body(goto, 0, 0, &[], int(2));

    // pc0 push_true; pc1 if_true -> pc12; pc6 push 1; pc11 return;
    // pc12 push 2; pc17 return.
    let if_true = bytes(&[
        &[op::OP_push_true],
        &branch(op::OP_if_true, 10),
        &push(1),
        &[op::OP_return],
        &push(2),
        &[op::OP_return],
    ]);
    assert_body(if_true, 0, 0, &[], int(2));

    // Same shape with `if_false`: the true condition falls through to 1.
    let if_false = bytes(&[
        &[op::OP_push_true],
        &branch(op::OP_if_false, 10),
        &push(1),
        &[op::OP_return],
        &push(2),
        &[op::OP_return],
    ]);
    assert_body(if_false, 0, 0, &[], int(1));
}

/// A backward edge is in the subset now. This body's loop arm is
/// statically reachable but never taken, so the harness never executes the
/// safepoint call (which needs a real frame) while still compiling it.
#[test]
fn hand_written_loop_cfg_compiles() {
    // pc0 push_false; pc1 if_false -> 17; pc6 push_false (loop header);
    // pc7 if_false -> 17; pc12 goto -> 6; pc17 push 7; pc22 return.
    // `push_false` exits before the header, so the back edge is compiled
    // but never executed.
    let body = bytes(&[
        &[op::OP_push_false],
        &branch(op::OP_if_false, 15),
        &[op::OP_push_false],
        &branch(op::OP_if_false, 9),
        &branch(op::OP_goto, -7),
        &push(7),
        &[op::OP_return],
    ]);
    assert_body(body, 0, 0, &[], int(7));
}

/// Every arithmetic/comparison/bit operator the inline lowering knows,
/// with the exact tagged result (not just "same as JS").
#[test]
fn hand_written_operators() {
    let binary =
        |opcode: u8, a: i32, b: i32| bytes(&[&push(a), &push(b), &[opcode], &[op::OP_return]]);
    assert_body(binary(op::OP_add, 7, 3), 0, 0, &[], int(10));
    assert_body(binary(op::OP_sub, 7, 3), 0, 0, &[], int(4));
    assert_body(binary(op::OP_mul, 7, 3), 0, 0, &[], int(21));
    assert_body(binary(op::OP_lt, 3, 7), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_lte, 7, 7), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_gt, 3, 7), 0, 0, &[], FALSE_BITS);
    assert_body(binary(op::OP_gte, 7, 7), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_eq, 7, 7), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_neq, 7, 3), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_strict_eq, 7, 7), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_strict_neq, 7, 7), 0, 0, &[], FALSE_BITS);
    // Bitwise ops: differential against the interpreter's `TwoNumbers`
    // (ToInt32 and the boxing rule live there only). The operand pairs
    // pin the tags too: `and`/`or`/`xor`/`shl`/`sar` box `From<i32>`
    // (always an Int), `shr` boxes `From<u32>` (a Float64 when the sign
    // bit is set).
    for opcode in [
        op::OP_and,
        op::OP_or,
        op::OP_xor,
        op::OP_shl,
        op::OP_shr,
        op::OP_sar,
    ] {
        for (a, b) in [
            (7, 3),
            (1, 3),
            (-8, 1),
            (-3, 2),
            (-1, 0),
            (0, -1),
            (-1, -1),
            (i32::MIN, -1),
            (i32::MAX, i32::MIN),
            (i32::MIN, 31),
            (0, 32),
        ] {
            assert_bitwise(opcode, a, b);
        }
    }

    let unary = |opcode: u8, value: i32| bytes(&[&push(value), &[opcode], &[op::OP_return]]);
    assert_body(unary(op::OP_plus, 7), 0, 0, &[], int(7));
    assert_body(unary(op::OP_neg, 7), 0, 0, &[], int(-7));
    assert_body(unary(op::OP_inc, 7), 0, 0, &[], int(8));
    assert_body(unary(op::OP_dec, 7), 0, 0, &[], int(6));
    assert_body(unary(op::OP_not, -1), 0, 0, &[], int(0));
    assert_body(unary(op::OP_lnot, 7), 0, 0, &[], FALSE_BITS);
    assert_body(
        bytes(&[&[op::OP_undefined, op::OP_lnot, op::OP_return]]),
        0,
        0,
        &[],
        TRUE_BITS,
    );
}

/// The poison flag: a body whose operands leave the integer domain must
/// take the bailout edge instead of boxing a wrong value.
#[test]
fn hand_written_bailouts() {
    let add_arg = || {
        bytes(&[
            &local(op::OP_get_arg, 0),
            &push(1),
            &[op::OP_add, op::OP_return],
        ])
    };
    assert_eq!(run(add_arg(), 1, 0, &[UNDEFINED_BITS]), None);
    assert_eq!(run(add_arg(), 1, 0, &[NULL_BITS]), None);
    assert_eq!(run(add_arg(), 1, 0, &[TRUE_BITS]), None);
    assert_eq!(run(add_arg(), 1, 0, &[int(1)]), Some(int(2)));

    // i32 overflow, `-0` from `-0` / `0 * -1`, and `plus` of undefined.
    assert_eq!(
        run(
            bytes(&[&push(i32::MAX), &push(1), &[op::OP_add, op::OP_return]]),
            0,
            0,
            &[]
        ),
        None
    );
    assert_eq!(
        run(bytes(&[&push(0), &[op::OP_neg, op::OP_return]]), 0, 0, &[]),
        None
    );
    assert_eq!(
        run(
            bytes(&[&push(0), &push(-1), &[op::OP_mul, op::OP_return]]),
            0,
            0,
            &[]
        ),
        None
    );
    assert_eq!(
        run(
            bytes(&[&[op::OP_undefined, op::OP_plus, op::OP_return]]),
            0,
            0,
            &[]
        ),
        None
    );
}

/// The tag, not the numeric value, decides the inline path: a Float64 that
/// happens to hold an integer still poisons, and the sentinel tags poison
/// everything.
#[test]
fn hand_written_tag_boundaries() {
    let add_one = bytes(&[
        &local(op::OP_get_arg, 0),
        &push(1),
        &[op::OP_add, op::OP_return],
    ]);
    assert_eq!(run(add_one.clone(), 1, 0, &[float(3.0)]), None);
    assert_eq!(run(add_one.clone(), 1, 0, &[float(f64::NAN)]), None);
    assert_eq!(run(add_one.clone(), 1, 0, &[float(-0.0)]), None);
    assert_eq!(run(add_one, 1, 0, &[test_harness::UNINITIALIZED]), None);

    // `lnot`/truthiness on the simple tags only.
    assert_eq!(
        run(
            bytes(&[&[op::OP_push_false, op::OP_lnot, op::OP_return]]),
            0,
            0,
            &[]
        ),
        Some(TRUE_BITS)
    );
    assert_eq!(
        run(
            bytes(&[&[op::OP_push_true, op::OP_lnot, op::OP_return]]),
            0,
            0,
            &[]
        ),
        Some(FALSE_BITS)
    );
    let truthy = bytes(&[
        &local(op::OP_get_arg, 0),
        &branch(op::OP_if_true, 10),
        &push(0),
        &[op::OP_return],
        &push(1),
        &[op::OP_return],
    ]);
    assert_eq!(run(truthy.clone(), 1, 0, &[float(0.0)]), None);
    assert_eq!(run(truthy.clone(), 1, 0, &[TRUE_BITS]), Some(int(1)));
    assert_eq!(run(truthy.clone(), 1, 0, &[FALSE_BITS]), Some(int(0)));
    assert_eq!(run(truthy, 1, 0, &[test_harness::UNINITIALIZED]), None);
}

/// Every numeric operator at the edge of its domain, including the shifts
/// where JS masks the count to five bits.
#[test]
fn hand_written_overflow_and_shift_boundaries() {
    let binary =
        |opcode: u8, a: i32, b: i32| bytes(&[&push(a), &push(b), &[opcode], &[op::OP_return]]);
    assert_eq!(run(binary(op::OP_add, i32::MAX, 1), 0, 0, &[]), None);
    assert_eq!(run(binary(op::OP_sub, i32::MIN, 1), 0, 0, &[]), None);
    assert_eq!(run(binary(op::OP_mul, i32::MAX, 2), 0, 0, &[]), None);
    assert_eq!(run(binary(op::OP_mul, i32::MIN, -1), 0, 0, &[]), None);
    assert_eq!(
        run(
            bytes(&[&push(i32::MAX), &[op::OP_inc, op::OP_return]]),
            0,
            0,
            &[]
        ),
        None
    );
    assert_eq!(
        run(
            bytes(&[&push(i32::MIN), &[op::OP_dec, op::OP_return]]),
            0,
            0,
            &[]
        ),
        None
    );
    assert_eq!(
        run(
            bytes(&[&push(i32::MIN), &[op::OP_neg, op::OP_return]]),
            0,
            0,
            &[]
        ),
        None
    );
    assert_body(binary(op::OP_lt, i32::MIN, i32::MAX), 0, 0, &[], TRUE_BITS);
    assert_body(binary(op::OP_gt, i32::MIN, i32::MAX), 0, 0, &[], FALSE_BITS);
}

/// Shift counts are masked with 31 and the boxing rule (`shl`/`sar` as
/// i32, `shr` as u32) is the interpreter's — differential against
/// `TwoNumbers`, one terminating body per case.
#[test]
fn hand_written_shift_count_masking() {
    assert_bitwise(op::OP_shl, 1, 31);
    assert_bitwise(op::OP_shl, 1, 32);
    assert_bitwise(op::OP_shl, 1, -1);
    assert_bitwise(op::OP_sar, -1, 0);
    assert_bitwise(op::OP_sar, -1, 32);
    assert_bitwise(op::OP_shr, -1, 32);
}

/// The `call_method` window moves the arguments one slot further, and the
/// declared/runtime argument counts are independent.
#[test]
fn hand_written_argument_windows() {
    let first = bytes(&[&local(op::OP_get_arg, 0), &[op::OP_return]]);
    let second = bytes(&[&local(op::OP_get_arg, 1), &[op::OP_return]]);
    assert_eq!(
        test_harness::run_inline(&first, 1, 0, 1, &[int(42)], true),
        Some(int(42))
    );
    assert_eq!(
        test_harness::run_inline(&first, 1, 0, 1, &[int(42)], false),
        Some(int(42))
    );
    // Missing argument: the slot is `undefined`.
    assert_eq!(
        test_harness::run_inline(&second, 2, 0, 1, &[int(7)], false),
        Some(UNDEFINED_BITS)
    );
    assert_eq!(
        test_harness::run_inline(&first, 1, 0, 0, &[], false),
        Some(UNDEFINED_BITS)
    );
    // Extra arguments are ignored (no `arguments` object in the subset).
    assert_eq!(
        test_harness::run_inline(&first, 1, 0, 3, &[int(1), int(2), int(3)], false),
        Some(int(1))
    );
}

/// The inline caps are inclusive, and every unsupported shape is rejected
/// before the lowering can see it.
#[test]
fn hand_written_eligibility_boundaries() {
    let mut nops_96 = Asm::new();
    for _ in 0..95 {
        nops_96.op(op::OP_nop);
    }
    nops_96.op(op::OP_return_undef);
    assert!(test_harness::inlinable(&nops_96.take(), 0, 0));

    let mut nops_97 = Asm::new();
    for _ in 0..96 {
        nops_97.op(op::OP_nop);
    }
    nops_97.op(op::OP_return_undef);
    assert!(!test_harness::inlinable(&nops_97.take(), 0, 0));

    assert!(test_harness::inlinable(&[op::OP_return_undef], 16, 16));
    assert!(!test_harness::inlinable(&[op::OP_return_undef], 17, 16));
    assert!(!test_harness::inlinable(&[op::OP_return_undef], 16, 17));
    assert!(test_harness::inlinable_with(
        &[op::OP_return_undef],
        0,
        0,
        32
    ));
    assert!(!test_harness::inlinable_with(
        &[op::OP_return_undef],
        0,
        0,
        33
    ));

    // A loop is inside the subset now: its back edge polls the
    // caller frame, so the eligibility gate no longer rejects it.
    let mut loop_forever = Asm::new();
    loop_forever.branch(op::OP_goto, 0);
    assert!(test_harness::inlinable(&loop_forever.take(), 0, 0));

    // Helper opcodes are outside the subset.
    assert!(!test_harness::inlinable(
        &bytes(&[&[op::OP_undefined, op::OP_typeof, op::OP_return]]),
        0,
        0
    ));
    assert!(!test_harness::inlinable(
        &bytes(&[
            &push(0),
            &local(op::OP_call, 0),
            &[op::OP_drop, op::OP_return_undef]
        ]),
        0,
        0
    ));
    assert!(!test_harness::inlinable(
        &bytes(&[
            &local(op::OP_set_loc_uninitialized, 0),
            &[op::OP_return_undef]
        ]),
        0,
        1
    ));
}

/// A loop in the caller puts a post-call OSR entry in the middle of the
/// block: the later call site is lowered twice. The test-only emission
/// counter pins that the second lowering happened (and the per-emission
/// variable-id block keeps Cranelift from rejecting the duplicate).
#[test]
fn osr_tail_lowers_the_same_inline_site_twice() {
    let callee = bytes(&[&push(1), &[op::OP_return]]);
    let mut caller = Asm::new();
    caller.push_i32(0);
    caller.op_u16(op::OP_put_loc, 0);
    let header = caller.pc();
    caller.op_u16(op::OP_get_loc, 0);
    caller.push_i32(3);
    caller.op(op::OP_lt);
    let end = caller.branch_placeholder(op::OP_if_false);
    caller.push_i32(0);
    let first_call = caller.pc();
    caller.op_u16(op::OP_call, 0);
    caller.op(op::OP_drop);
    caller.push_i32(0);
    let second_call = caller.pc();
    caller.op_u16(op::OP_call, 0);
    caller.op(op::OP_drop);
    caller.op_u16(op::OP_get_loc, 0);
    caller.push_i32(1);
    caller.op(op::OP_add);
    caller.op_u16(op::OP_put_loc, 0);
    let back = caller.branch_placeholder(op::OP_goto);
    let end_pc = caller.pc();
    caller.op(op::OP_return_undef);
    caller.patch_branch(end, end_pc);
    caller.patch_branch(back, header);

    test_harness::reset_inline_emissions();
    test_harness::compile_caller_with_inline(
        &caller.take(),
        0,
        1,
        &[(first_call, &callee), (second_call, &callee)],
    )
    .expect("the caller with two inline sites compiles");
    assert!(
        test_harness::inline_emissions() >= 3,
        "the second call site must be lowered again for the post-call OSR entry"
    );
}

/// Nested inlining: a call inside a copied body emits the nested body too.
#[test]
fn nested_inline_bodies_are_emitted() {
    let leaf = bytes(&[&push(1), &[op::OP_return]]);
    // `push_i32` (5 bytes) then `call` at pc 5: compile-only, the callee
    // value is never a real function at runtime.
    let mid = bytes(&[&push(0), &local(op::OP_call, 0), &[op::OP_return]]);
    let mut caller = Asm::new();
    caller.push_i32(0);
    let site = caller.pc();
    caller.op_u16(op::OP_call, 0);
    caller.op(op::OP_return);

    test_harness::reset_inline_emissions();
    test_harness::compile_caller_with_nested_inline(
        &caller.take(),
        0,
        0,
        &[(site, &mid, &[(5, &leaf)])],
    )
    .expect("caller with a nested inline site compiles");
    assert_eq!(
        test_harness::inline_emissions(),
        2,
        "the outer body and its nested callee must both be emitted"
    );
}

/// The same assembler/compile/execute path works for a *whole* function,
/// which is what future bytecode-to-JIT optimization tests need.
#[test]
fn hand_written_whole_function_runs() {
    assert_eq!(
        test_harness::run_function(&bytes(&[&push(5), &[op::OP_return]]), 0, 0, &[]),
        Some(int(5))
    );
    assert_eq!(
        test_harness::run_function(&[op::OP_return_undef], 0, 0, &[]),
        Some(UNDEFINED_BITS)
    );
    assert_eq!(
        test_harness::run_function(
            &bytes(&[
                &local(op::OP_get_arg, 0),
                &push(1),
                &[op::OP_add, op::OP_return]
            ]),
            1,
            0,
            &[int(41)]
        ),
        Some(int(42))
    );
}
