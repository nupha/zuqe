// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Control flow: `if`/`with` lowering and the conditional branch.

use super::*;

/// Pop the condition, test it (inline fast paths; helper for the rest), then
/// branch. Both the fast and slow paths duplicate the branch, avoiding a phi.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_if(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    truthy_sig: cranelift_codegen::ir::SigRef,
    state: &mut State,
    insn: &Instruction,
    sp: Value,
    is_true: bool,
) {
    let (sp, value) = state.stack.pop(b, sp);
    // The condition is in a register now; everything else must be in memory
    // before any of the branches below (they collect, call helpers or exit).
    state.stack.flush(b, sp);
    let pc_after = insn.pc() + insn.size() as u32;
    if insn
        .branch_target()
        .is_some_and(|t| t <= i64::from(insn.pc()))
    {
        // Back edge: deliver interrupts / run a deferred collection first.
        b.def_var(sp_var, sp);
        let cont = safepoint(state, b, fv, helper_sig, sp, pc_after);
        b.switch_to_block(cont);
    }
    let tag = b.ins().ushr_imm(value, 48);
    // Tags 0..=3 (Int, Bool, Null, Undefined) have statically known truthiness.
    let fast = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, tag, 3);
    let fast_blk = b.create_block();
    let slow_blk = b.create_block();
    b.ins().brif(fast, fast_blk, &[], slow_blk, &[]);

    b.switch_to_block(fast_blk);
    let low = b.ins().ireduce(types::I32, value);
    let nonzero = b.ins().icmp_imm(IntCC::NotEqual, low, 0);
    let int_or_bool = b.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, tag, 1);
    let zero = b.ins().iconst(types::I8, 0);
    let test_fast = b.ins().select(int_or_bool, nonzero, zero);
    emit_cond_branch(b, fv, sp_var, state, insn, sp, test_fast, is_true);

    b.switch_to_block(slow_blk);
    write_cur_sp(b, fv, sp);
    let callee = b.ins().load(
        types::I64,
        MemFlags::new(),
        fv.helpers,
        (HELPER_TO_BOOL * 8) as i32,
    );
    let call = b
        .ins()
        .call_indirect(truthy_sig, callee, &[fv.frame, value]);
    let result = b.inst_results(call)[0];
    let failed = b.ins().icmp_imm(IntCC::SignedLessThan, result, 0);
    let fail = b.create_block();
    let ok = b.create_block();
    b.ins().brif(failed, fail, &[], ok, &[]);
    b.switch_to_block(fail);
    exit_const(state, b, fv, 1, pc_after, sp);
    b.switch_to_block(ok);
    let result = b.ins().ireduce(types::I32, result);
    let test_slow = b.ins().icmp_imm(IntCC::NotEqual, result, 0);
    emit_cond_branch(b, fv, sp_var, state, insn, sp, test_slow, is_true);
}

/// `with_*`: call the resolver helper, then branch to the instruction's label
/// when it handled the name and fall through to the ordinary variable access
/// otherwise. The helper owns both arms' stack shapes (it leaves `sp` at the
/// shape of whichever path it reports), so the code loads `sp` back and hands
/// the same value to either edge; only the status picks the edge.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_with(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    id: usize,
    insn: &Instruction,
    pc_after: u32,
) {
    flush_and_publish(state, b, fv, sp);
    let callee = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.helpers, (id * 8) as i32);
    let call = b.ins().call_indirect(helper_sig, callee, &[fv.frame]);
    let status = b.inst_results(call)[0];
    // Everything above `HELPER_WITH_MISS` (`1`/`2`) is an exception/interrupt
    // and leaves the frame with the exit status as usual.
    let hit = b.ins().icmp_imm(IntCC::Equal, status, i64::from(HELPER_OK));
    let miss = b
        .ins()
        .icmp_imm(IntCC::Equal, status, i64::from(HELPER_WITH_MISS));
    let resolved = b.ins().bor(hit, miss);
    let fail = b.create_block();
    let cont = b.create_block();
    b.ins().brif(resolved, cont, &[], fail, &[]);
    b.switch_to_block(fail);
    exit_with_status(b, fv, status, pc_after, sp);
    b.switch_to_block(cont);
    let sp = b.ins().load(types::I64, MemFlags::new(), fv.frame, O_SP);
    if insn
        .branch_target()
        .is_some_and(|t| t <= i64::from(insn.pc()))
    {
        // A resolved label can land on a back edge (the optimizer follows a
        // `goto` at the label): poll interrupts before taking it.
        b.def_var(sp_var, sp);
        let cont = safepoint(state, b, fv, helper_sig, sp, pc_after);
        b.switch_to_block(cont);
    }
    emit_cond_branch(b, fv, sp_var, state, insn, sp, hit, true);
}

#[allow(clippy::too_many_arguments)]
fn emit_cond_branch(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    state: &mut State,
    insn: &Instruction,
    sp: Value,
    cond: Value,
    is_true: bool,
) {
    let target = insn.branch_target().expect("branch target") as u32;
    let fallthrough = insn.pc() + insn.size() as u32;
    let target_block = state.block_for(b, target);
    let fall_block = state.block_for(b, fallthrough);
    // The shared flush before the branch left the model empty, so the carried
    // slots of both targets arrive as frame loads on this arm.
    let mut args = state.edge_args(
        b,
        fv,
        sp,
        &[(target_block, target), (fall_block, fallthrough)],
    );
    let fall_args = args.pop().expect("two targets");
    let target_args = args.pop().expect("two targets");
    b.def_var(sp_var, sp);
    if is_true {
        b.ins()
            .brif(cond, target_block, &target_args, fall_block, &fall_args);
    } else {
        b.ins()
            .brif(cond, fall_block, &fall_args, target_block, &target_args);
    }
}
