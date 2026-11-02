//! Emission primitives: exits, helper calls, safepoints and the
//! frame-write helpers every lowering path shares.

use super::*;

fn slots_of(b: &mut FunctionBuilder, fv: &FrameVals, sp: Value) -> Value {
    let diff = b.ins().isub(sp, fv.sp_base);
    let slots = b.ins().ushr_imm(diff, 3);
    b.ins().ireduce(types::I32, slots)
}

pub(super) fn write_cur_sp(b: &mut FunctionBuilder, fv: &FrameVals, sp: Value) {
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.cur_sp, 0);
}

/// Make the operand stack observable: flush the register-resident slots, then
/// publish `sp` (both the frame slot and `cur_sp_offset`), so a helper or the
/// collector sees exactly the stack the interpreter would.
pub(super) fn flush_and_publish(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp: Value,
) {
    state.stack.flush(b, sp);
    b.ins().store(MemFlags::new(), sp, fv.frame, O_SP);
    write_cur_sp(b, fv, sp);
}

/// Write `out_pc`/`out_sp`: where the interpreter resumes and how many slots
/// the operand stack held.
pub(super) fn write_exit_frame(b: &mut FunctionBuilder, fv: &FrameVals, pc: u32, sp: Value) {
    let pcv = b.ins().iconst(types::I32, i64::from(pc));
    b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
}

/// One guard-instrumentation counter bump: load, add one, store back.
pub(super) fn bump_counter(b: &mut FunctionBuilder, fv: &FrameVals, offset: i32) {
    let count = b.ins().load(types::I32, MemFlags::new(), fv.frame, offset);
    let count = b.ins().iadd_imm(count, 1);
    b.ins().store(MemFlags::new(), count, fv.frame, offset);
}

/// The collector's write barrier: only a value that references a GC node
/// needs it. Branches to `bind` for such a value, otherwise straight to
/// `cont`; both paths leave through `cont` with `cont_args`. `bind` must take
/// no arguments.
pub(super) fn emit_write_barrier(
    b: &mut FunctionBuilder,
    state: &State,
    fv: &FrameVals,
    owner: Value,
    value: Value,
    bind: Block,
    cont: Block,
    cont_args: &[Value],
) {
    // The pointer tags are 0xfff8..=0xffff (`Object` .. `Symbol`).
    let value_tag = b.ins().ushr_imm(value, 48);
    let needs_bind = b
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, value_tag, GC_NODE_TAG);
    b.ins().brif(needs_bind, bind, &[], cont, cont_args);
    b.switch_to_block(bind);
    // `h_gc_bind(frame, owner, value)`: the barrier itself is the collector's
    // business, so it stays in Rust.
    let callee = b.ins().load(
        types::I64,
        MemFlags::new(),
        fv.helpers,
        (HELPER_GC_BIND * 8) as i32,
    );
    b.ins()
        .call_indirect(state.barrier_sig, callee, &[fv.frame, owner, value]);
    b.ins().jump(cont, cont_args);
}

/// Write `out_pc`/`out_sp` and return `kind`. Register-resident stack slots
/// are materialized first: the interpreter resumes by reading them.
pub(super) fn exit_const(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    kind: i64,
    pc: u32,
    sp: Value,
) {
    state.stack.flush(b, sp);
    write_exit_frame(b, fv, pc, sp);
    let kind = b.ins().iconst(types::I32, kind);
    b.ins().return_(&[kind]);
}

/// Exit with a status produced at runtime (helper status 1/2 is exactly
/// `JitExitKind::Exception`/`Interrupt`).
///
/// Only called after the cache was flushed and dropped, so the frame already
/// holds every slot this exit exposes to the interpreter.
pub(super) fn exit_with_status(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    status: Value,
    pc: u32,
    sp: Value,
) {
    write_exit_frame(b, fv, pc, sp);
    b.ins().return_(&[status]);
}

/// Publish `sp` (for GC) and call stack helper `id`; returns the reloaded sp.
/// On helper failure the calling block is terminated with the exit.
///
/// The frame is the helper's window into the operand stack, so every
/// register-resident slot is written back before the call and the cache is
/// dropped — the helper may rewrite the stack (and runs JS, which can throw).
pub(super) fn call_stack_helper(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    id: usize,
    pc_after: u32,
) -> Value {
    flush_and_publish(state, b, fv, sp);
    let callee = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.helpers, (id * 8) as i32);
    let call = b.ins().call_indirect(helper_sig, callee, &[fv.frame]);
    let status = b.inst_results(call)[0];
    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
    let fail = b.create_block();
    let ok = b.create_block();
    b.ins().brif(failed, fail, &[], ok, &[]);
    b.switch_to_block(fail);
    exit_with_status(b, fv, status, pc_after, sp);
    b.switch_to_block(ok);
    b.ins().load(types::I64, MemFlags::new(), fv.frame, O_SP)
}

/// Emit one of the pure value pushes a merged literal window accepts as
/// a field value. Each pushes exactly one slot; the register cache tracks them
/// like any other push, and the merged helper's flush roots them before the
/// one call that builds the literal.
pub(super) fn lower_literal_push(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    insn: &Instruction,
    sp: Value,
) -> Value {
    match insn.name() {
        "push_i32" => {
            let v = b.ins().iconst(types::I64, i64::from(insn.operand_u32(1)));
            state.stack.push_int(b, sp, v)
        }
        "push_const" => {
            let idx = insn.operand_u32(1) as usize;
            let v = b
                .ins()
                .load(types::I64, MemFlags::new(), fv.cpools, (idx * 8) as i32);
            let is_int = state.const_meta.get(idx).is_some_and(|meta| meta.tag == 0);
            if is_int {
                state.stack.push_int(b, sp, v)
            } else {
                state.stack.push(b, sp, v)
            }
        }
        "get_loc" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state
                .locals
                .read(b, fv.var_base, false, idx, loc_offset_u16(insn));
            state.stack.push(b, sp, v)
        }
        "get_arg" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state
                .locals
                .read(b, fv.arg_base, true, idx, loc_offset_u16(insn));
            state.stack.push(b, sp, v)
        }
        "undefined" => {
            let v = b.ins().iconst(types::I64, UNDEFINED_BITS);
            state.stack.push(b, sp, v)
        }
        _ => {
            let v = b.ins().iconst(types::I64, 2i64 << 48); // null
            state.stack.push(b, sp, v)
        }
    }
}

/// Like [`call_stack_helper`], but for a helper that **always** leaves the
/// frame: it reports `HELPER_EXCEPTION` (or `HELPER_INTERRUPT`) whatever the
/// operand was. There is then no continuation to emit, so the instruction is a
/// `Step::Stop` — which also means a `throw` can be the last instruction of a
/// function, as it is in real bytecode (`cfg::is_terminal`).
///
/// `pc_after` follows the same convention as the helper failure path:
/// `cur_pc_offset` is post-instruction in both engines.
pub(super) fn call_raising_helper(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    id: usize,
    pc_after: u32,
) {
    flush_and_publish(state, b, fv, sp);
    let callee = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.helpers, (id * 8) as i32);
    let call = b.ins().call_indirect(helper_sig, callee, &[fv.frame]);
    let status = b.inst_results(call)[0];
    exit_with_status(b, fv, status, pc_after, sp);
}

/// Emit a loop back edge safepoint. Returns the continuation block (not yet
/// switched to); on failure the exit is emitted in a side block.
pub(super) fn safepoint(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    pc_after: u32,
) -> Block {
    // The safepoint can collect: GC scans the frame up to `cur_sp`, so the
    // register-resident slots must be visible in memory first.
    flush_and_publish(state, b, fv, sp);
    let callee = b.ins().load(
        types::I64,
        MemFlags::new(),
        fv.helpers,
        (HELPER_SAFEPOINT * 8) as i32,
    );
    let call = b.ins().call_indirect(helper_sig, callee, &[fv.frame]);
    let status = b.inst_results(call)[0];
    let failed = b.ins().icmp_imm(IntCC::NotEqual, status, 0);
    let fail = b.create_block();
    let cont = b.create_block();
    b.ins().brif(failed, fail, &[], cont, &[]);
    b.switch_to_block(fail);
    exit_with_status(b, fv, status, pc_after, sp);
    cont
}
