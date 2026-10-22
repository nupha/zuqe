// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! D-6 route B, step 2: copy an inlinable callee's body into the caller.
//!
//! The first step enters the callee's own compiled code; this step removes
//! that code's arena frame as well. The callee's arguments, locals and
//! operand stack live in caller-frame variables (Cranelift SSA), so a call
//! site costs no `alloc_frame`/`init_frame_args` and no second activation.
//!
//! # The subset
//!
//! The copied body is *pure*: no heap helper except the allocation entries
//! below, no closure/global/`this`. Every operation is lowered for the
//! `Int`/`Bool`/`Null`/`Undefined` tags. An operand outside them sets a failure
//! flag instead of splitting the block; at the `return` a single branch sends
//! the whole call to the general path, which re-runs the callee from scratch
//! with the untouched call window. That is why a bailout is safe without a
//! deopt map: the inline path has no observable side effect, so nothing
//! observable has happened. D-25's flush rule still holds: no register-resident
//! value ever crosses a helper or an exit.
//!
//! The extensions are independent (T7):
//!
//! * **Loops** (T7.1): a backward edge polls the caller frame's safepoint
//!   first; the inline body carries only integer-domain values (or copies of
//!   the original arguments, still in the call window), so the caller frame is
//!   a complete GC root set without a frame of its own. A non-zero status —
//!   and a poisoned back edge — bails to the general path.
//! * **Calls** (T7.2): a monomorphic nested site copies its callee too (the
//!   snapshot carries the body and its own sites, depth ≤2). The nested guard
//!   compares the live callee's bytecode identity with a compile-time
//!   constant; a miss bails.
//! * **Exceptions** (T7.3): `catch`/`nip_catch` only keep the stack height
//!   right; every `throw` bails, so the interpreter's catch scan (including a
//!   handler inside the callee) owns the semantics.
//! * **Allocation** (T7.4): only `{}` and `[]` are in the subset. The helper
//!   writes the new object to the frame scratch slot; that is safe because an
//!   allocation never collects (it only sets `gc_req`), so a copied subtree
//!   that allocates must contain no safepoint (`hazards`).

use std::collections::HashMap;

use cranelift_codegen::ir::{Block, InstBuilder, MemFlags, SigRef, Value, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, Variable};

use super::{
    abi::{HELPER_ALLOC_ARRAY0, HELPER_ALLOC_OBJECT, HELPER_SAFEPOINT},
    bytecode::VerifiedFunction,
    compiler::{box_bit_result, is_int},
};

const UNDEFINED: i64 = 3 << 48;
const NULL: i64 = 2 << 48;
const BOOL_TAG: i64 = 1 << 48;
/// The nested guard's hit counter (T7.2 instrumentation).
const O_NESTED_INLINE_HITS: i32 =
    std::mem::offset_of!(crate::jit::abi::JitExecFrame, nested_inline_hits) as i32;
/// The allocation helper's result slot (T7.4).
const O_SCRATCH: i32 = std::mem::offset_of!(crate::jit::abi::JitExecFrame, scratch) as i32;

/// Bounds on a copied body. They keep the caller's generated code (and the
/// inline plan) small; the same limits are already implied by the snapshot's
/// bytecode cap, but these are the numbers the subset was tested at.
const MAX_INSTRUCTIONS: usize = 96;
const MAX_BLOCKS: usize = 16;

/// One verified body plus its nested monomorphic call sites (T7.2). The plan is
/// built once per top-level inline site and consumed by [`lower`].
pub(super) struct Plan {
    pub(super) body: VerifiedFunction,
    /// The callee's bytecode identity, compared by the nested guard.
    pub(super) callee: u64,
    /// Nested call site pc -> the body it may copy.
    pub(super) sites: HashMap<u32, Plan>,
}

/// May this callee's body be copied? This is the whole structural contract:
/// the lowering below only knows the opcodes [`supported`] accepts, plus a
/// `call`/`call_method` at a pc [`Plan::sites`] has a body for.
pub(super) fn eligible(body: &VerifiedFunction, sites: &HashMap<u32, Plan>) -> bool {
    let snapshot = body.snapshot();
    if snapshot.arg_count > 16
        || snapshot.var_count > 16
        || snapshot.stack_size > 32
        || body.instructions().len() > MAX_INSTRUCTIONS
        || body.control_flow_graph().blocks().len() > MAX_BLOCKS
    {
        return false;
    }
    for instruction in body.instructions() {
        let name = instruction.name();
        if matches!(name, "call" | "call_method") {
            if !sites.contains_key(&instruction.pc()) {
                return false;
            }
            continue;
        }
        if name == "array_from" && instruction.operand_u16(1) != 0 {
            // Only the empty-array helper returns through `scratch`; a
            // non-empty literal would need its elements in memory.
            return false;
        }
        if !supported(name) {
            return false;
        }
    }
    // An allocation leaves the new object only in a register. That is safe
    // exactly while no GC point can run in this copied subtree: an allocation
    // never collects by itself (it only sets `gc_req`).
    let (allocates, has_safepoint) = hazards(body, sites);
    !(allocates && has_safepoint)
}

/// `(allocates, has a back edge)` for a copied subtree.
fn hazards(body: &VerifiedFunction, sites: &HashMap<u32, Plan>) -> (bool, bool) {
    let mut allocates = false;
    let mut safepoint = false;
    for instruction in body.instructions() {
        match instruction.name() {
            "object" | "array_from" => allocates = true,
            "goto" | "if_true" | "if_false" => {
                if instruction
                    .branch_target()
                    .is_some_and(|target| target <= i64::from(instruction.pc()))
                {
                    safepoint = true;
                }
            }
            _ => {}
        }
    }
    for plan in sites.values() {
        let (nested_alloc, nested_safepoint) = hazards(&plan.body, &plan.sites);
        allocates |= nested_alloc;
        safepoint |= nested_safepoint;
    }
    (allocates, safepoint)
}

/// The opcodes the inline lowering implements. Everything the ordinary
/// lowering would send to a helper is either lowered here for `Int` operands
/// or left out of the subset entirely.
fn supported(name: &str) -> bool {
    matches!(
        name,
        "nop"
            | "get_arg"
            | "get_loc"
            | "put_arg"
            | "put_loc"
            | "set_arg"
            | "set_loc"
            | "push_i32"
            | "undefined"
            | "null"
            | "push_false"
            | "push_true"
            | "drop"
            | "dup"
            | "goto"
            | "if_true"
            | "if_false"
            | "catch"
            | "nip_catch"
            | "throw"
            | "object"
            | "array_from"
            | "return"
            | "return_undef"
            | "add"
            | "sub"
            | "mul"
            | "add_loc"
            | "inc_loc"
            | "dec_loc"
            | "lt"
            | "lte"
            | "gt"
            | "gte"
            | "eq"
            | "neq"
            | "strict_eq"
            | "strict_neq"
            | "and"
            | "or"
            | "xor"
            | "shl"
            | "sar"
            | "shr"
            | "plus"
            | "neg"
            | "inc"
            | "dec"
            | "lnot"
            | "not"
    )
}

/// Emit the callee's body in the current function.
///
/// `args` holds the callee's declared arguments (missing ones filled with
/// `undefined`). `store` is `Some((base, sp_after))` for the top-level copy —
/// the result is written to the call window and `merge` receives `sp_after` —
/// and `None` for a nested copy, whose result travels as `merge`'s block
/// parameter. `bail` is the general call path, entered whenever an operand
/// needs semantics this subset does not implement.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower(
    b: &mut FunctionBuilder,
    plan: &Plan,
    args: &[Value],
    store: Option<(Value, Value)>,
    next_tag: &mut u32,
    entry: Block,
    bail: Block,
    merge: Block,
    frame: Value,
    helpers: Value,
    helper_sig: SigRef,
) {
    #[cfg(test)]
    crate::jit::test_harness::note_inline_emission();
    let tag = *next_tag;
    *next_tag += 1;
    let body = &plan.body;
    let snapshot = body.snapshot();
    let undefined = b.ins().iconst(types::I64, UNDEFINED);

    let mut arg_vars = Vec::with_capacity(snapshot.arg_count as usize);
    for index in 0..snapshot.arg_count {
        let var = Variable::from_u32(tag * 0x100 + 0x10 + u32::from(index));
        b.declare_var(var, types::I64);
        let value = args.get(index as usize).copied().unwrap_or(undefined);
        b.def_var(var, value);
        arg_vars.push(var);
    }

    let mut locals = Vec::with_capacity(snapshot.var_count as usize);
    for index in 0..snapshot.var_count {
        let var = Variable::from_u32(tag * 0x100 + 0x40 + u32::from(index));
        b.declare_var(var, types::I64);
        b.def_var(var, undefined);
        locals.push(var);
    }

    // The callee's operand stack is caller-frame state too: one variable per
    // depth. The verifier's heights keep every use/def below the live top.
    let stack_size = usize::from(snapshot.stack_size) + 1;
    let mut stack = Vec::with_capacity(stack_size);
    for index in 0..stack_size {
        let var = Variable::from_u32(tag * 0x100 + 0x80 + index as u32);
        b.declare_var(var, types::I64);
        stack.push(var);
    }

    // One flag collects every "this operand is outside the subset" answer, so
    // a straight-line body stays a single block and the branch happens once.
    let bad = Variable::from_u32(tag * 0x100 + 0x04);
    b.declare_var(bad, types::I8);
    let zero_flag = b.ins().iconst(types::I8, 0);
    b.def_var(bad, zero_flag);

    let mut blocks: HashMap<u32, Block> = HashMap::new();
    let mut pending: Vec<u32> = Vec::new();
    blocks.insert(0, entry);
    pending.push(0);

    while let Some(start_pc) = pending.pop() {
        let block = blocks[&start_pc];
        // The caller is already in the entry block; only the other blocks
        // need switching to.
        if block != entry {
            b.switch_to_block(block);
        }
        let range = body
            .control_flow_graph()
            .block(start_pc)
            .map(|block| block.instruction_range())
            .expect("every inline block start is a CFG block");
        let instructions = body.instructions();
        let mut height = usize::from(body.stack_heights()[range.start]);
        let mut terminated = false;

        for index in range.clone() {
            let instruction = &instructions[index];
            if terminated {
                break;
            }
            debug_assert_eq!(
                height,
                usize::from(body.stack_heights()[index]),
                "inline stack height diverged at pc {}",
                instruction.pc()
            );
            let name = instruction.name();
            match name {
                "nop" => {}
                "push_i32" => {
                    let value = b
                        .ins()
                        .iconst(types::I64, i64::from(instruction.operand_u32(1)));
                    push(b, &stack, &mut height, value);
                }
                "undefined" => push(b, &stack, &mut height, undefined),
                "null" => {
                    let value = b.ins().iconst(types::I64, NULL);
                    push(b, &stack, &mut height, value);
                }
                "push_false" | "push_true" => {
                    let bit = i64::from(name == "push_true");
                    let value = b.ins().iconst(types::I64, BOOL_TAG | bit);
                    push(b, &stack, &mut height, value);
                }
                "get_arg" => {
                    let value = b.use_var(arg_vars[usize::from(instruction.operand_u16(1))]);
                    push(b, &stack, &mut height, value);
                }
                "get_loc" => {
                    let value = b.use_var(locals[usize::from(instruction.operand_u16(1))]);
                    push(b, &stack, &mut height, value);
                }
                "put_arg" => {
                    let value = pop(b, &stack, &mut height);
                    b.def_var(arg_vars[usize::from(instruction.operand_u16(1))], value);
                }
                "put_loc" => {
                    let value = pop(b, &stack, &mut height);
                    b.def_var(locals[usize::from(instruction.operand_u16(1))], value);
                }
                "set_arg" => {
                    let value = use_top(b, &stack, height);
                    b.def_var(arg_vars[usize::from(instruction.operand_u16(1))], value);
                }
                "set_loc" => {
                    let value = use_top(b, &stack, height);
                    b.def_var(locals[usize::from(instruction.operand_u16(1))], value);
                }
                "drop" => {
                    height -= 1;
                }
                "dup" => {
                    let value = use_top(b, &stack, height);
                    push(b, &stack, &mut height, value);
                }
                "object" | "array_from" => {
                    // The helper cannot collect (allocation only requests GC),
                    // so the unrooted register result is safe here; a body that
                    // allocates and contains a safepoint is not eligible.
                    let id = if name == "object" {
                        HELPER_ALLOC_OBJECT
                    } else {
                        HELPER_ALLOC_ARRAY0
                    };
                    let helper =
                        b.ins()
                            .load(types::I64, MemFlags::new(), helpers, (id * 8) as i32);
                    let call = b.ins().call_indirect(helper_sig, helper, &[frame]);
                    let status = b.inst_results(call)[0];
                    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
                    let ok = b.create_block();
                    b.ins().brif(failed, bail, &[], ok, &[]);
                    b.switch_to_block(ok);
                    let value = b.ins().load(types::I64, MemFlags::new(), frame, O_SCRATCH);
                    push(b, &stack, &mut height, value);
                }
                "add" | "sub" | "mul" => {
                    let (a, c) = pop_binary(b, &stack, &mut height, bad);
                    let ai = b.ins().ireduce(types::I32, a);
                    let ci = b.ins().ireduce(types::I32, c);
                    let (result, overflow) = match name {
                        "add" => b.ins().sadd_overflow(ai, ci),
                        "sub" => b.ins().ssub_overflow(ai, ci),
                        _ => b.ins().smul_overflow(ai, ci),
                    };
                    // `0 * -1` is -0, which an Int cannot carry.
                    let overflow = if name == "mul" {
                        let a_zero = b.ins().icmp_imm(IntCC::Equal, ai, 0);
                        let c_zero = b.ins().icmp_imm(IntCC::Equal, ci, 0);
                        let a_neg = b.ins().icmp_imm(IntCC::SignedLessThan, ai, 0);
                        let c_neg = b.ins().icmp_imm(IntCC::SignedLessThan, ci, 0);
                        let neg_zero = {
                            let ac = b.ins().band(a_zero, c_neg);
                            let ca = b.ins().band(c_zero, a_neg);
                            b.ins().bor(ac, ca)
                        };
                        b.ins().bor(overflow, neg_zero)
                    } else {
                        overflow
                    };
                    note_bad(b, bad, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "add_loc" => {
                    let rhs = pop(b, &stack, &mut height);
                    let lhs = b.use_var(locals[usize::from(instruction.operand_u8(1))]);
                    let lhs_int = is_int(b, lhs);
                    let rhs_int = is_int(b, rhs);
                    let both = b.ins().band(lhs_int, rhs_int);
                    let failed = not(b, both);
                    note_bad(b, bad, failed);
                    let ai = b.ins().ireduce(types::I32, lhs);
                    let ci = b.ins().ireduce(types::I32, rhs);
                    let (result, overflow) = b.ins().sadd_overflow(ai, ci);
                    note_bad(b, bad, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    b.def_var(locals[usize::from(instruction.operand_u8(1))], boxed);
                }
                "inc_loc" | "dec_loc" => {
                    let idx = usize::from(instruction.operand_u8(1));
                    let value = b.use_var(locals[idx]);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_bad(b, bad, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    let one = b.ins().iconst(types::I32, 1);
                    let (result, overflow) = if name == "inc_loc" {
                        b.ins().sadd_overflow(value, one)
                    } else {
                        b.ins().ssub_overflow(value, one)
                    };
                    note_bad(b, bad, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    b.def_var(locals[idx], boxed);
                }
                "lt" | "lte" | "gt" | "gte" | "eq" | "neq" | "strict_eq" | "strict_neq" => {
                    let (a, c) = pop_binary(b, &stack, &mut height, bad);
                    let ai = b.ins().ireduce(types::I32, a);
                    let ci = b.ins().ireduce(types::I32, c);
                    let condition = match name {
                        "lt" => b.ins().icmp(IntCC::SignedLessThan, ai, ci),
                        "lte" => b.ins().icmp(IntCC::SignedLessThanOrEqual, ai, ci),
                        "gt" => b.ins().icmp(IntCC::SignedGreaterThan, ai, ci),
                        "gte" => b.ins().icmp(IntCC::SignedGreaterThanOrEqual, ai, ci),
                        "eq" | "strict_eq" => b.ins().icmp(IntCC::Equal, ai, ci),
                        _ => b.ins().icmp(IntCC::NotEqual, ai, ci),
                    };
                    let widened = b.ins().uextend(types::I64, condition);
                    let boxed = b.ins().bor_imm(widened, BOOL_TAG);
                    push(b, &stack, &mut height, boxed);
                }
                "and" | "or" | "xor" | "shl" | "sar" | "shr" => {
                    let (a, c) = pop_binary(b, &stack, &mut height, bad);
                    let ai = b.ins().ireduce(types::I32, a);
                    let ci = b.ins().ireduce(types::I32, c);
                    let count = b.ins().band_imm(ci, 31);
                    let result = match name {
                        "and" => b.ins().band(ai, ci),
                        "or" => b.ins().bor(ai, ci),
                        "xor" => b.ins().bxor(ai, ci),
                        "shl" => b.ins().ishl(ai, count),
                        "shr" => b.ins().ushr(ai, count),
                        _ => b.ins().sshr(ai, count),
                    };
                    let boxed = match name {
                        "shl" | "sar" => b.ins().uextend(types::I64, result),
                        _ => box_bit_result(b, result),
                    };
                    push(b, &stack, &mut height, boxed);
                }
                "plus" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_bad(b, bad, failed);
                    push(b, &stack, &mut height, value);
                }
                "neg" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_bad(b, bad, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    // `-0` is a Float64; an Int 0 cannot carry its sign.
                    let zero = b.ins().icmp_imm(IntCC::Equal, value, 0);
                    note_bad(b, bad, zero);
                    let zero = b.ins().iconst(types::I32, 0);
                    let (result, overflow) = b.ins().ssub_overflow(zero, value);
                    note_bad(b, bad, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "inc" | "dec" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_bad(b, bad, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    let one = b.ins().iconst(types::I32, 1);
                    let (result, overflow) = if name == "inc" {
                        b.ins().sadd_overflow(value, one)
                    } else {
                        b.ins().ssub_overflow(value, one)
                    };
                    note_bad(b, bad, overflow);
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "lnot" => {
                    let value = pop(b, &stack, &mut height);
                    let truthy = truthy(b, value, bad);
                    let inverted = b.ins().icmp_imm(IntCC::Equal, truthy, 0);
                    let widened = b.ins().uextend(types::I64, inverted);
                    let boxed = b.ins().bor_imm(widened, BOOL_TAG);
                    push(b, &stack, &mut height, boxed);
                }
                "not" => {
                    let value = pop(b, &stack, &mut height);
                    let is_num = is_int(b, value);
                    let failed = not(b, is_num);
                    note_bad(b, bad, failed);
                    let value = b.ins().ireduce(types::I32, value);
                    let result = b.ins().bnot(value);
                    // Unary `~` boxes as `From<i32>` (an `Int`, sign kept),
                    // unlike the binary logic ops' `From<u32>`.
                    let boxed = b.ins().uextend(types::I64, result);
                    push(b, &stack, &mut height, boxed);
                }
                "call" | "call_method" => {
                    let argc = usize::from(instruction.operand_u16(1));
                    let method = name == "call_method";
                    let site = plan
                        .sites
                        .get(&instruction.pc())
                        .expect("eligibility only accepts calls with a planned body");
                    let mut call_args = vec![undefined; argc];
                    for index in (0..argc).rev() {
                        call_args[index] = pop(b, &stack, &mut height);
                    }
                    let func = if method {
                        let _receiver = pop(b, &stack, &mut height);
                        pop(b, &stack, &mut height)
                    } else {
                        pop(b, &stack, &mut height)
                    };
                    // The nested site's monomorphic guard. A miss bails the
                    // whole call: nothing observable happened yet.
                    let hit = callee_guard(b, func, site.callee, frame, bail);
                    let nested_merge = b.create_block();
                    b.append_block_param(nested_merge, types::I64);
                    let declared = usize::from(site.body.snapshot().arg_count);
                    let nested_args: Vec<Value> = (0..declared)
                        .map(|index| call_args.get(index).copied().unwrap_or(undefined))
                        .collect();
                    lower(
                        b,
                        site,
                        &nested_args,
                        None,
                        next_tag,
                        hit,
                        bail,
                        nested_merge,
                        frame,
                        helpers,
                        helper_sig,
                    );
                    b.switch_to_block(nested_merge);
                    let result = b.block_params(nested_merge)[0];
                    push(b, &stack, &mut height, result);
                }
                "catch" => {
                    // The interpreter's catch scan reads this sentinel from the
                    // frame after a native exception exit. The inline path
                    // never exits, so it only has to keep the stack height
                    // right (a throw bails and re-runs the callee).
                    let handler = instruction.branch_target().expect("catch target") as u32;
                    let sentinel = b
                        .ins()
                        .iconst(types::I64, (5i64 << 48) | i64::from(handler));
                    push(b, &stack, &mut height, sentinel);
                }
                "nip_catch" => {
                    // `[catch_offset, ..., value, ^] -> [value, ^]`. The
                    // sentinel's depth is fixed by the verifier's height after
                    // this instruction (the stack must be balanced there), so
                    // no runtime scan is needed.
                    let value = pop(b, &stack, &mut height);
                    let after = usize::from(
                        *body
                            .stack_heights()
                            .get(index + 1)
                            .expect("nip_catch is never the last instruction"),
                    );
                    b.def_var(stack[after - 1], value);
                    height = after;
                }
                "throw" => {
                    let _value = pop(b, &stack, &mut height);
                    // Nothing observable happened before the throw: the general
                    // path re-runs the callee and owns the catch scan, so a
                    // handler inside this body still wins exactly as it would.
                    b.ins().jump(bail, &[]);
                    terminated = true;
                }
                "goto" => {
                    let target_pc = instruction.branch_target().expect("goto target") as u32;
                    let target = block_for(b, &mut blocks, &mut pending, target_pc);
                    if target_pc <= instruction.pc() {
                        backedge_safepoint(b, bad, frame, helpers, helper_sig, bail);
                    }
                    b.ins().jump(target, &[]);
                    terminated = true;
                }
                "if_true" | "if_false" => {
                    let value = pop(b, &stack, &mut height);
                    let cond = truthy(b, value, bad);
                    let target_pc = instruction.branch_target().expect("if target") as u32;
                    let target = block_for(b, &mut blocks, &mut pending, target_pc);
                    let fallthrough_pc = instruction.pc() + instruction.size() as u32;
                    let fallthrough = block_for(b, &mut blocks, &mut pending, fallthrough_pc);
                    if target_pc <= instruction.pc() {
                        // Only the taken edge loops: enter a side block so the
                        // poison check and the safepoint run on that edge only.
                        let back = b.create_block();
                        if name == "if_true" {
                            b.ins().brif(cond, back, &[], fallthrough, &[]);
                        } else {
                            b.ins().brif(cond, fallthrough, &[], back, &[]);
                        }
                        b.switch_to_block(back);
                        backedge_safepoint(b, bad, frame, helpers, helper_sig, bail);
                        b.ins().jump(target, &[]);
                    } else if name == "if_true" {
                        b.ins().brif(cond, target, &[], fallthrough, &[]);
                    } else {
                        b.ins().brif(cond, fallthrough, &[], target, &[]);
                    }
                    terminated = true;
                }
                "return" | "return_undef" => {
                    let value = if name == "return" {
                        pop(b, &stack, &mut height)
                    } else {
                        undefined
                    };
                    // One bailout check for the whole body: a poisoned value
                    // may have travelled through branches, but every path ends
                    // here, and nothing was stored.
                    let failed = b.use_var(bad);
                    let failed = b.ins().icmp_imm(IntCC::NotEqual, failed, 0);
                    let good = b.create_block();
                    b.ins().brif(failed, bail, &[], good, &[]);
                    b.switch_to_block(good);
                    let out = if let Some((base, sp_after)) = store {
                        b.ins().store(MemFlags::new(), value, base, 0);
                        sp_after
                    } else {
                        value
                    };
                    b.ins().jump(merge, &[out]);
                    terminated = true;
                }
                _ => unreachable!("inline subset checked before lowering"),
            }
        }

        if !terminated {
            let tail = &instructions[range.end - 1];
            let next = tail.pc() + tail.size() as u32;
            let next = block_for(b, &mut blocks, &mut pending, next);
            b.ins().jump(next, &[]);
        }
    }
}

/// The nested site's guard: the live callee must be a bytecode function whose
/// bytecode node is the recorded one. Mirrors `h_inline_guard`/`callee_is`,
/// but the identity is a compile-time constant (the nested site belongs to the
/// copied body's bytecode, not the caller's feedback table). Returns the block
/// to emit the nested body in; a miss jumps to `bail`.
fn callee_guard(
    b: &mut FunctionBuilder,
    func: Value,
    expected: u64,
    frame: Value,
    bail: Block,
) -> Block {
    let tag = b.ins().ushr_imm(func, 48);
    let is_object = b.ins().icmp_imm(IntCC::Equal, tag, 0xFFFF);
    let object = b.create_block();
    b.ins().brif(is_object, object, &[], bail, &[]);

    b.switch_to_block(object);
    // The object's node pointer: the low 48 bits of the tagged value.
    let node = b.ins().band_imm(func, 0x0000_ffff_ffff_ffff);
    let flags = b.ins().load(
        types::I32,
        MemFlags::new(),
        node,
        super::layout::object::FLAGS_OFFSET,
    );
    let class = b
        .ins()
        .band_imm(flags, super::layout::object::CLASS_ID_MASK);
    let is_function = b.ins().icmp_imm(
        IntCC::Equal,
        class,
        i64::from(crate::JS_CLASS_BYTECODE_FUNCTION),
    );
    let function = b.create_block();
    b.ins().brif(is_function, function, &[], bail, &[]);

    b.switch_to_block(function);
    let payload = b.ins().load(
        types::I64,
        MemFlags::new(),
        node,
        super::layout::object::PAYLOAD_OFFSET,
    );
    let bytecode = b.ins().load(
        types::I64,
        MemFlags::new(),
        payload,
        super::layout::function::BYTECODE_OFFSET,
    );
    let same = b.ins().icmp_imm(IntCC::Equal, bytecode, expected as i64);
    let hit = b.create_block();
    b.ins().brif(same, hit, &[], bail, &[]);
    b.switch_to_block(hit);
    let hits = b
        .ins()
        .load(types::I32, MemFlags::new(), frame, O_NESTED_INLINE_HITS);
    let hits = b.ins().iadd_imm(hits, 1);
    b.ins()
        .store(MemFlags::new(), hits, frame, O_NESTED_INLINE_HITS);
    hit
}

/// Leave the inline body when it is poisoned or when the caller frame asks to
/// stop, before a backward edge. The inline body owns no frame: the caller's
/// call window is the GC root set, and the values in registers are either
/// integer-domain or copies of values in that window.
fn backedge_safepoint(
    b: &mut FunctionBuilder,
    bad: Variable,
    frame: Value,
    helpers: Value,
    helper_sig: SigRef,
    bail: Block,
) {
    // A poisoned condition could otherwise loop where the interpreter would
    // not; bail before the first poisoned iteration sees a second one.
    let poison = b.use_var(bad);
    let failed = b.ins().icmp_imm(IntCC::NotEqual, poison, 0);
    let clean = b.create_block();
    b.ins().brif(failed, bail, &[], clean, &[]);
    b.switch_to_block(clean);

    let safepoint = b.ins().load(
        types::I64,
        MemFlags::new(),
        helpers,
        (HELPER_SAFEPOINT * 8) as i32,
    );
    let call = b.ins().call_indirect(helper_sig, safepoint, &[frame]);
    let status = b.inst_results(call)[0];
    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
    let cont = b.create_block();
    b.ins().brif(failed, bail, &[], cont, &[]);
    b.switch_to_block(cont);
}

fn block_for(
    b: &mut FunctionBuilder,
    blocks: &mut HashMap<u32, Block>,
    pending: &mut Vec<u32>,
    pc: u32,
) -> Block {
    if let Some(block) = blocks.get(&pc) {
        return *block;
    }
    let block = b.create_block();
    blocks.insert(pc, block);
    pending.push(pc);
    block
}

fn push(b: &mut FunctionBuilder, stack: &[Variable], height: &mut usize, value: Value) {
    b.def_var(stack[*height], value);
    *height += 1;
}

fn pop(b: &mut FunctionBuilder, stack: &[Variable], height: &mut usize) -> Value {
    *height -= 1;
    b.use_var(stack[*height])
}

fn use_top(b: &mut FunctionBuilder, stack: &[Variable], height: usize) -> Value {
    b.use_var(stack[height - 1])
}

fn not(b: &mut FunctionBuilder, value: Value) -> Value {
    let zero = b.ins().iconst(types::I8, 0);
    b.ins().icmp(IntCC::Equal, value, zero)
}

/// OR a failure condition into the body's poison flag.
fn note_bad(b: &mut FunctionBuilder, bad: Variable, condition: Value) {
    let previous = b.use_var(bad);
    let merged = b.ins().bor(previous, condition);
    b.def_var(bad, merged);
}

/// Pop two operands; a non-`Int` poisons the body instead of splitting it.
fn pop_binary(
    b: &mut FunctionBuilder,
    stack: &[Variable],
    height: &mut usize,
    bad: Variable,
) -> (Value, Value) {
    let c = pop(b, stack, height);
    let a = pop(b, stack, height);
    let a_int = is_int(b, a);
    let c_int = is_int(b, c);
    let both = b.ins().band(a_int, c_int);
    let failed = not(b, both);
    note_bad(b, bad, failed);
    (a, c)
}

/// `ToBoolean` for the tags this subset carries. Other tags poison the body,
/// so the general path evaluates them (strings, objects, BigInt, …).
fn truthy(b: &mut FunctionBuilder, value: Value, bad: Variable) -> Value {
    let tag = b.ins().ushr_imm(value, 48);
    let simple = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, tag, 3);
    let failed = not(b, simple);
    note_bad(b, bad, failed);
    let low = b.ins().ireduce(types::I32, value);
    let int_true = b.ins().icmp_imm(IntCC::NotEqual, low, 0);
    let low_bit = b.ins().band_imm(value, 1);
    let bool_true = b.ins().icmp_imm(IntCC::Equal, low_bit, 1);
    let is_int_tag = b.ins().icmp_imm(IntCC::Equal, tag, 0);
    let is_bool_tag = b.ins().icmp_imm(IntCC::Equal, tag, 1);
    let int_true = b.ins().band(is_int_tag, int_true);
    let bool_true = b.ins().band(is_bool_tag, bool_true);
    b.ins().bor(int_true, bool_true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::opcodes as op,
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

    /// A backward edge is in the subset now (T7.1). This body's loop arm is
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
        assert_body(binary(op::OP_and, 7, 3), 0, 0, &[], int(3));
        assert_body(binary(op::OP_or, 7, 3), 0, 0, &[], int(7));
        assert_body(binary(op::OP_xor, 7, 3), 0, 0, &[], int(4));
        assert_body(binary(op::OP_shl, 1, 3), 0, 0, &[], int(8));
        assert_body(binary(op::OP_sar, -8, 1), 0, 0, &[], int(-4));
        assert_body(binary(op::OP_shr, -1, 0), 0, 0, &[], float(4294967295.0));
        // `-1 | 0` boxes as `From<u32>`: the interpreter's bit-op quirk.
        assert_body(binary(op::OP_or, -1, 0), 0, 0, &[], float(4294967295.0));

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

        // `shl`/`sar` box as i32; counts are masked with 31.
        assert_body(binary(op::OP_shl, 1, 31), 0, 0, &[], int(i32::MIN));
        assert_body(binary(op::OP_shl, 1, 32), 0, 0, &[], int(1));
        assert_body(binary(op::OP_shl, 1, -1), 0, 0, &[], int(i32::MIN));
        assert_body(binary(op::OP_sar, -1, 0), 0, 0, &[], int(-1));
        assert_body(binary(op::OP_sar, -1, 32), 0, 0, &[], int(-1));
        assert_body(binary(op::OP_shr, -1, 32), 0, 0, &[], float(4294967295.0));
        assert_body(binary(op::OP_lt, i32::MIN, i32::MAX), 0, 0, &[], TRUE_BITS);
        assert_body(binary(op::OP_gt, i32::MIN, i32::MAX), 0, 0, &[], FALSE_BITS);
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

        // A loop is inside the subset now (T7.1): its back edge polls the
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

    /// T7.2: a call inside a copied body emits the nested body too.
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
}
