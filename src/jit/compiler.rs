// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Baseline (Tier1) lowering: verified bytecode straight to Cranelift IR.
//!
//! Values live in the frame buffer, exactly where the interpreter keeps them.
//! The operand-stack pointer is an SSA value; it is written back to
//! `StackFrame.cur_sp_offset` before any helper call and before any exit, the
//! only moments a collection can observe it. Anything the baseline cannot
//! translate terminates the native run with `Resume`, and the interpreter picks
//! up at that bytecode offset.
//!
//! There is no separate IR layer: with no optimization passes above it, the
//! lowering reads `VerifiedFunction` directly (C1 folded into C2).

use std::{collections::HashMap, mem::offset_of};

use cranelift_codegen::{
    Context,
    control::ControlPlane,
    ir::{
        AbiParam, Block, InstBuilder, MemFlags, Signature, UserFuncName, Value, condcodes::IntCC,
        types,
    },
    isa::TargetIsa,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::vm_stack::ExecMode;

use super::{
    abi::{
        HELPER_ADD, HELPER_AND, HELPER_DEC, HELPER_DIV, HELPER_EQ, HELPER_GT, HELPER_GTE,
        HELPER_INC, HELPER_LNOT, HELPER_LT, HELPER_LTE, HELPER_MOD, HELPER_MUL, HELPER_NEG,
        HELPER_NEQ, HELPER_NOT, HELPER_OR, HELPER_PLUS, HELPER_SAFEPOINT, HELPER_SAR, HELPER_SHL,
        HELPER_SHR, HELPER_STRICT_EQ, HELPER_STRICT_NEQ, HELPER_SUB, HELPER_TO_BOOL, HELPER_TYPEOF,
        HELPER_XOR, JitExecFrame,
    },
    bytecode::{decode::Instruction, verify::VerifiedFunction},
    error::JitError,
};

/// Exit codes as written by generated code (keep in sync with `JitExitKind`).
const EXIT_DONE: i64 = 0;
const EXIT_RESUME: i64 = 6;

const UNDEFINED_BITS: i64 = 3 << 48;

const O_ARG_BASE: i32 = offset_of!(JitExecFrame, arg_base) as i32;
const O_VAR_BASE: i32 = offset_of!(JitExecFrame, var_base) as i32;
const O_SP_BASE: i32 = offset_of!(JitExecFrame, sp_base) as i32;
const O_SP: i32 = offset_of!(JitExecFrame, sp) as i32;
const O_CPOOLS: i32 = offset_of!(JitExecFrame, cpools) as i32;
const O_CUR_SP: i32 = offset_of!(JitExecFrame, cur_sp_offset) as i32;
const O_HELPERS: i32 = offset_of!(JitExecFrame, helpers) as i32;
const O_THIS: i32 = offset_of!(JitExecFrame, this) as i32;
const O_IMM: i32 = offset_of!(JitExecFrame, imm) as i32;
const O_OUT_VALUE: i32 = offset_of!(JitExecFrame, out_value) as i32;
const O_OUT_PC: i32 = offset_of!(JitExecFrame, out_pc) as i32;
const O_OUT_SP: i32 = offset_of!(JitExecFrame, out_sp) as i32;

/// Whether the baseline can translate this opcode. Everything else is a
/// `Resume` exit: the interpreter runs that instruction and continues.
fn supported(name: &str) -> bool {
    helper_for(name).is_some()
        || shuffle_outputs(name).is_some()
        || matches!(
            name,
            "push_i32"
                | "push_const"
                | "push_atom_value"
                | "undefined"
                | "null"
                | "push_false"
                | "push_true"
                | "push_this"
                | "drop"
                | "get_loc"
                | "put_loc"
                | "set_loc"
                | "get_arg"
                | "put_arg"
                | "set_arg"
                | "inc_loc"
                | "dec_loc"
                | "goto"
                | "if_true"
                | "if_false"
                | "return"
                | "return_undef"
                | "nop"
        )
}

fn helper_for(name: &str) -> Option<usize> {
    Some(match name {
        "add" => HELPER_ADD,
        "sub" => HELPER_SUB,
        "mul" => HELPER_MUL,
        "div" => HELPER_DIV,
        "mod" => HELPER_MOD,
        "lt" => HELPER_LT,
        "lte" => HELPER_LTE,
        "gt" => HELPER_GT,
        "gte" => HELPER_GTE,
        "eq" => HELPER_EQ,
        "neq" => HELPER_NEQ,
        "strict_eq" => HELPER_STRICT_EQ,
        "strict_neq" => HELPER_STRICT_NEQ,
        "and" => HELPER_AND,
        "or" => HELPER_OR,
        "xor" => HELPER_XOR,
        "shl" => HELPER_SHL,
        "sar" => HELPER_SAR,
        "shr" => HELPER_SHR,
        "plus" => HELPER_PLUS,
        "neg" => HELPER_NEG,
        "inc" => HELPER_INC,
        "dec" => HELPER_DEC,
        "lnot" => HELPER_LNOT,
        "not" => HELPER_NOT,
        "typeof" => HELPER_TYPEOF,
        _ => return None,
    })
}

/// Output arrangement for the stack shuffles: each output copies the popped
/// operand at that index. Order matches the interpreter's writes.
fn shuffle_outputs(name: &str) -> Option<(usize, &'static [usize])> {
    Some(match name {
        "nip" => (2, &[1]),
        "nip1" => (3, &[1, 2]),
        "dup" => (1, &[0, 0]),
        "dup1" => (2, &[0, 0, 1]),
        "dup2" => (2, &[0, 1, 0, 1]),
        "dup3" => (3, &[0, 1, 2, 0, 1, 2]),
        "insert2" => (2, &[1, 0, 1]),
        "insert3" => (3, &[2, 0, 1, 2]),
        "insert4" => (4, &[3, 0, 1, 2, 3]),
        "perm3" => (3, &[1, 0, 2]),
        "perm4" => (4, &[1, 0, 2, 3]),
        "perm5" => (5, &[1, 0, 2, 3, 4]),
        "swap" => (2, &[1, 0]),
        "swap2" => (4, &[2, 3, 0, 1]),
        "rot3l" => (3, &[1, 2, 0]),
        "rot3r" => (3, &[2, 0, 1]),
        "rot4l" => (4, &[1, 2, 3, 0]),
        "rot5l" => (5, &[1, 2, 3, 4, 0]),
        _ => return None,
    })
}

/// Values loaded once from the frame in the entry block; they dominate every
/// reachable block, so they can be used anywhere.
struct FrameVals {
    frame: Value,
    sp_base: Value,
    var_base: Value,
    arg_base: Value,
    cpools: Value,
    cur_sp: Value,
    helpers: Value,
    this: Value,
}

struct State {
    blocks: HashMap<u32, Block>,
    pending: Vec<u32>,
}

impl State {
    fn block_for(&mut self, b: &mut FunctionBuilder, pc: u32) -> Block {
        if let Some(block) = self.blocks.get(&pc) {
            return *block;
        }
        let block = b.create_block();
        self.blocks.insert(pc, block);
        self.pending.push(pc);
        block
    }
}

enum Step {
    Continue(Value),
    /// The block was terminated (branch, return, or `Resume` exit).
    Stop,
}

fn slots_of(b: &mut FunctionBuilder, fv: &FrameVals, sp: Value) -> Value {
    let diff = b.ins().isub(sp, fv.sp_base);
    let slots = b.ins().ushr_imm(diff, 3);
    b.ins().ireduce(types::I32, slots)
}

fn write_cur_sp(b: &mut FunctionBuilder, fv: &FrameVals, sp: Value) {
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.cur_sp, 0);
}

fn push(b: &mut FunctionBuilder, sp: Value, v: Value) -> Value {
    b.ins().store(MemFlags::new(), v, sp, 0);
    b.ins().iadd_imm(sp, 8)
}

fn pop(b: &mut FunctionBuilder, sp: Value) -> (Value, Value) {
    let sp = b.ins().iadd_imm(sp, -8);
    let v = b.ins().load(types::I64, MemFlags::new(), sp, 0);
    (sp, v)
}

/// Write `out_pc`/`out_sp` and return `kind`.
fn exit(b: &mut FunctionBuilder, fv: &FrameVals, kind: i64, pc: u32, sp: Value) {
    let pcv = b.ins().iconst(types::I32, i64::from(pc));
    b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
    let kind = b.ins().iconst(types::I32, kind);
    b.ins().return_(&[kind]);
}

/// Exit with a status produced at runtime (helper status 1/2 is exactly
/// `JitExitKind::Exception`/`Interrupt`).
fn exit_status(b: &mut FunctionBuilder, fv: &FrameVals, status: Value, pc: u32, sp: Value) {
    let pcv = b.ins().iconst(types::I32, i64::from(pc));
    b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
    b.ins().return_(&[status]);
}

/// Publish `sp` (for GC) and call stack helper `id`; returns the reloaded sp.
/// On helper failure the calling block is terminated with the exit.
fn call_stack_helper(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    id: usize,
    pc_after: u32,
) -> Value {
    b.ins().store(MemFlags::new(), sp, fv.frame, O_SP);
    write_cur_sp(b, fv, sp);
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
    exit_status(b, fv, status, pc_after, sp);
    b.switch_to_block(ok);
    b.ins().load(types::I64, MemFlags::new(), fv.frame, O_SP)
}

/// Emit a loop back edge safepoint. Returns the continuation block (not yet
/// switched to); on failure the exit is emitted in a side block.
fn safepoint(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    pc_after: u32,
) -> Block {
    b.ins().store(MemFlags::new(), sp, fv.frame, O_SP);
    write_cur_sp(b, fv, sp);
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
    exit_status(b, fv, status, pc_after, sp);
    cont
}

/// Compile one verified function. Returns code bytes for the caller to publish.
pub(crate) fn compile(
    verified: &VerifiedFunction,
    isa: &dyn TargetIsa,
) -> Result<Box<[u8]>, JitError> {
    let instructions = verified.instructions();
    let Some(first) = instructions.first() else {
        return Err(JitError::Cranelift("empty function".into()));
    };
    // A function whose very first op is unsupported would resume on every
    // entry; don't install it.
    if !supported(first.name()) {
        return Err(JitError::Cranelift(format!(
            "entry opcode unsupported: {}",
            first.name()
        )));
    }

    let mut sig = Signature::new(isa.default_call_conv());
    sig.params.push(AbiParam::new(types::I64));
    sig.returns.push(AbiParam::new(types::I32));

    let mut ctx = Context::new();
    ctx.func.signature = sig;
    ctx.func.name = UserFuncName::user(0, 1);

    let mut fbctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);

    let mut helper_sig = Signature::new(isa.default_call_conv());
    helper_sig.params.push(AbiParam::new(types::I64));
    helper_sig.returns.push(AbiParam::new(types::I32));
    let helper_sig = b.import_signature(helper_sig);

    let mut truthy_sig = Signature::new(isa.default_call_conv());
    truthy_sig.params.push(AbiParam::new(types::I64));
    truthy_sig.params.push(AbiParam::new(types::I64));
    truthy_sig.returns.push(AbiParam::new(types::I64));
    let truthy_sig = b.import_signature(truthy_sig);

    let entry = b.create_block();
    b.append_block_params_for_function_params(entry);
    b.switch_to_block(entry);
    let frame = b.block_params(entry)[0];

    let sp_var = Variable::from_u32(0);
    b.declare_var(sp_var, types::I64);

    let fv = FrameVals {
        frame,
        sp_base: b.ins().load(types::I64, MemFlags::new(), frame, O_SP_BASE),
        var_base: b.ins().load(types::I64, MemFlags::new(), frame, O_VAR_BASE),
        arg_base: b.ins().load(types::I64, MemFlags::new(), frame, O_ARG_BASE),
        cpools: b.ins().load(types::I64, MemFlags::new(), frame, O_CPOOLS),
        cur_sp: b.ins().load(types::I64, MemFlags::new(), frame, O_CUR_SP),
        helpers: b.ins().load(types::I64, MemFlags::new(), frame, O_HELPERS),
        this: b.ins().load(types::I64, MemFlags::new(), frame, O_THIS),
    };
    b.def_var(sp_var, fv.sp_base);

    let strict = verified.snapshot().mode.contains(ExecMode::STRICT);
    lower(
        &mut b, &fv, sp_var, helper_sig, truthy_sig, verified, strict,
    );

    b.seal_all_blocks();
    b.finalize();

    let mut ctrl = ControlPlane::default();
    let compiled = ctx
        .compile(isa, &mut ctrl)
        .map_err(|e| JitError::Cranelift(format!("{e:?}")))?;
    if !compiled.buffer.relocs().is_empty() {
        // No external symbol is referenced, so a reloc means codegen emitted
        // something we cannot relocate: fail closed.
        return Err(JitError::Cranelift("unexpected relocations".into()));
    }
    Ok(compiled.code_buffer().into())
}

#[allow(clippy::too_many_arguments)]
fn lower(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    truthy_sig: cranelift_codegen::ir::SigRef,
    verified: &VerifiedFunction,
    strict: bool,
) {
    let instructions = verified.instructions();
    let cfg = verified.control_flow_graph();
    let mut state = State {
        blocks: HashMap::new(),
        pending: Vec::new(),
    };
    let entry_block = state.block_for(b, 0);
    b.ins().jump(entry_block, &[]);

    while let Some(start_pc) = state.pending.pop() {
        let Some(block) = cfg.block(start_pc) else {
            continue;
        };
        let cranelift_block = state.blocks[&start_pc];
        b.switch_to_block(cranelift_block);
        let mut sp = b.use_var(sp_var);
        let mut stopped = false;

        for insn in &instructions[block.instruction_range()] {
            match lower_insn(
                b, fv, sp_var, helper_sig, truthy_sig, strict, &mut state, insn, sp,
            ) {
                Step::Continue(new_sp) => sp = new_sp,
                Step::Stop => {
                    stopped = true;
                    break;
                }
            }
        }

        if !stopped {
            // Only reached for blocks ending in a non-branch instruction.
            let tail = &instructions[block.instruction_range().end - 1];
            let next = tail.pc() + tail.size() as u32;
            let next_block = state.block_for(b, next);
            b.def_var(sp_var, sp);
            b.ins().jump(next_block, &[]);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_insn(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    truthy_sig: cranelift_codegen::ir::SigRef,
    strict: bool,
    state: &mut State,
    insn: &Instruction,
    sp: Value,
) -> Step {
    let name = insn.name();
    let pc = insn.pc();
    let size = insn.size() as u32;

    if let Some(id) = helper_for(name) {
        let sp = call_stack_helper(b, fv, helper_sig, sp, id, pc + size);
        return Step::Continue(sp);
    }
    if let Some((pop, outputs)) = shuffle_outputs(name) {
        let base = b.ins().iadd_imm(sp, -((pop * 8) as i64));
        let mut vals = Vec::with_capacity(pop);
        for i in 0..pop {
            vals.push(
                b.ins()
                    .load(types::I64, MemFlags::new(), base, (i * 8) as i32),
            );
        }
        for (j, src) in outputs.iter().enumerate() {
            b.ins()
                .store(MemFlags::new(), vals[*src], base, (j * 8) as i32);
        }
        let sp = b.ins().iadd_imm(base, (outputs.len() * 8) as i64);
        return Step::Continue(sp);
    }

    match name {
        "nop" => Step::Continue(sp),
        "push_i32" => {
            let v = b.ins().iconst(types::I64, i64::from(insn.operand_u32(1)));
            Step::Continue(push(b, sp, v))
        }
        "push_const" => {
            let idx = insn.operand_u32(1) as i32;
            let v = b
                .ins()
                .load(types::I64, MemFlags::new(), fv.cpools, idx * 8);
            Step::Continue(push(b, sp, v))
        }
        "push_atom_value" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp = call_stack_helper(
                b,
                fv,
                helper_sig,
                sp,
                super::abi::HELPER_PUSH_ATOM,
                pc + size,
            );
            Step::Continue(sp)
        }
        "undefined" => {
            let v = b.ins().iconst(types::I64, UNDEFINED_BITS);
            Step::Continue(push(b, sp, v))
        }
        "null" => {
            let v = b.ins().iconst(types::I64, 2i64 << 48);
            Step::Continue(push(b, sp, v))
        }
        "push_false" => {
            let v = b.ins().iconst(types::I64, 1i64 << 48);
            Step::Continue(push(b, sp, v))
        }
        "push_true" => {
            let v = b.ins().iconst(types::I64, (1i64 << 48) | 1);
            Step::Continue(push(b, sp, v))
        }
        "push_this" if strict => Step::Continue(push(b, sp, fv.this)),
        "drop" => Step::Continue(b.ins().iadd_imm(sp, -8)),
        "get_loc" => {
            let v = b.ins().load(
                types::I64,
                MemFlags::new(),
                fv.var_base,
                loc_offset_u16(insn),
            );
            Step::Continue(push(b, sp, v))
        }
        "get_arg" => {
            let v = b.ins().load(
                types::I64,
                MemFlags::new(),
                fv.arg_base,
                loc_offset_u16(insn),
            );
            Step::Continue(push(b, sp, v))
        }
        "put_loc" => {
            let (sp, v) = pop(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        "put_arg" => {
            let (sp, v) = pop(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.arg_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        "set_loc" => {
            let v = b.ins().load(types::I64, MemFlags::new(), sp, -8);
            b.ins()
                .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        "set_arg" => {
            let v = b.ins().load(types::I64, MemFlags::new(), sp, -8);
            b.ins()
                .store(MemFlags::new(), v, fv.arg_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        // Local `++`/`--`: the helper updates the slot in place, so an exception
        // exit leaves `sp` untouched (matching the interpreter).
        "inc_loc" | "dec_loc" => {
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
            let id = if name == "inc_loc" {
                super::abi::HELPER_INC_LOCAL
            } else {
                super::abi::HELPER_DEC_LOCAL
            };
            let sp = call_stack_helper(b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "goto" => {
            let target = insn.branch_target().expect("goto target") as u32;
            let dest = state.block_for(b, target);
            b.def_var(sp_var, sp);
            if target <= pc {
                // Back edge: deliver interrupts / run a deferred collection.
                let cont = safepoint(b, fv, helper_sig, sp, pc + size);
                b.switch_to_block(cont);
            }
            b.ins().jump(dest, &[]);
            Step::Stop
        }
        "if_true" | "if_false" => {
            lower_if(
                b,
                fv,
                sp_var,
                helper_sig,
                truthy_sig,
                state,
                insn,
                sp,
                name == "if_true",
            );
            Step::Stop
        }
        "return" => {
            let (sp, v) = pop(b, sp);
            let pcv = b.ins().iconst(types::I32, i64::from(pc + size));
            b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
            let slots = slots_of(b, fv, sp);
            b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
            b.ins().store(MemFlags::new(), v, fv.frame, O_OUT_VALUE);
            let kind = b.ins().iconst(types::I32, EXIT_DONE);
            b.ins().return_(&[kind]);
            Step::Stop
        }
        "return_undef" => {
            let pcv = b.ins().iconst(types::I32, i64::from(pc + size));
            b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
            let slots = slots_of(b, fv, sp);
            b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
            let v = b.ins().iconst(types::I64, UNDEFINED_BITS);
            b.ins().store(MemFlags::new(), v, fv.frame, O_OUT_VALUE);
            let kind = b.ins().iconst(types::I32, EXIT_DONE);
            b.ins().return_(&[kind]);
            Step::Stop
        }
        // Unsupported: hand this instruction to the interpreter.
        _ => {
            exit(b, fv, EXIT_RESUME, pc, sp);
            Step::Stop
        }
    }
}

fn loc_offset_u16(insn: &Instruction) -> i32 {
    i32::from(insn.operand_u16(1)) * 8
}

/// Pop the condition, test it (inline fast paths; helper for the rest), then
/// branch. Both the fast and slow paths duplicate the branch, avoiding a phi.
#[allow(clippy::too_many_arguments)]
fn lower_if(
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
    let (sp, value) = pop(b, sp);
    let pc_after = insn.pc() + insn.size() as u32;
    if insn
        .branch_target()
        .is_some_and(|t| t <= i64::from(insn.pc()))
    {
        // Back edge: deliver interrupts / run a deferred collection first.
        b.def_var(sp_var, sp);
        let cont = safepoint(b, fv, helper_sig, sp, pc_after);
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
    exit(b, fv, 1, pc_after, sp);
    b.switch_to_block(ok);
    let result = b.ins().ireduce(types::I32, result);
    let test_slow = b.ins().icmp_imm(IntCC::NotEqual, result, 0);
    emit_cond_branch(b, fv, sp_var, state, insn, sp, test_slow, is_true);
}

fn emit_cond_branch(
    b: &mut FunctionBuilder,
    _fv: &FrameVals,
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
    b.def_var(sp_var, sp);
    if is_true {
        b.ins().brif(cond, target_block, &[], fall_block, &[]);
    } else {
        b.ins().brif(cond, fall_block, &[], target_block, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::opcodes as op,
        jit::{
            bytecode::{VerifyLimits, verify},
            snapshot::{CompileSnapshot, ConstMeta},
        },
    };

    /// Hand-built snapshot: `stack_size` is generous; index checks use `args`/
    /// `vars`; no global var refs or constants unless a test adds them.
    fn snapshot(bytes: Vec<u8>, args: u16, vars: u16) -> CompileSnapshot {
        CompileSnapshot::from_raw(bytes, args, vars, 0, 16)
    }

    /// `Some(Ok)` = verified and compiled; panics if verification rejects (a
    /// test case being malformed is a bug in the test, not the compiler).
    fn compile_bytes(bytes: Vec<u8>, args: u16, vars: u16) -> Result<(), JitError> {
        let isa = crate::jit::build_host_isa().unwrap();
        let verified = verify(snapshot(bytes, args, vars), VerifyLimits::default())
            .unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
        compile(&verified, isa.as_ref()).map(|_| ())
    }

    fn seq(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }
    fn u16b(x: u16) -> [u8; 2] {
        x.to_le_bytes()
    }
    fn i32b(x: i32) -> [u8; 4] {
        x.to_le_bytes()
    }
    fn u32b(x: u32) -> [u8; 4] {
        x.to_le_bytes()
    }
    fn push_i32(x: i32) -> [u8; 5] {
        let mut out = [0u8; 5];
        out[0] = op::OP_push_i32;
        out[1..].copy_from_slice(&i32b(x));
        out
    }

    /// Local-indexed opcode: `[op, idx:u16]`.
    fn loc(opcode: u8, idx: u16) -> [u8; 3] {
        let mut out = [0u8; 3];
        out[0] = opcode;
        out[1..].copy_from_slice(&u16b(idx));
        out
    }
    /// `[op, idx:u8]` (LOC8).
    fn loc8(opcode: u8, idx: u8) -> [u8; 2] {
        [opcode, idx]
    }
    /// `[op, off:i32]`.
    fn branch(opcode: u8, off: i32) -> [u8; 5] {
        let mut out = [0u8; 5];
        out[0] = opcode;
        out[1..].copy_from_slice(&i32b(off));
        out
    }

    /// Every opcode the baseline claims to support must lower without a
    /// Cranelift error. This is the coverage net for the lowering `match`.
    #[test]
    fn every_supported_opcode_lowers() {
        let ret = [op::OP_return];
        let mut cases: Vec<(&str, Vec<u8>, u16, u16)> = Vec::new();

        // Pushes / constants.
        cases.push(("push_i32", seq(&[&push_i32(7), &ret]), 0, 0));
        cases.push((
            "push_atom_value",
            seq(&[&[op::OP_push_atom_value], &u32b(0), &ret]),
            0,
            0,
        ));
        for name in ["undefined", "null", "push_false", "push_true", "push_this"] {
            let code = match name {
                "undefined" => op::OP_undefined,
                "null" => op::OP_null,
                "push_false" => op::OP_push_false,
                "push_true" => op::OP_push_true,
                _ => op::OP_push_this,
            };
            cases.push((name, seq(&[&[code], &ret]), 0, 0));
        }

        // Stack shuffles: five operands is more than any shuffle pops.
        let shuffles: &[(&str, u8)] = &[
            ("drop", op::OP_drop),
            ("nip", op::OP_nip),
            ("nip1", op::OP_nip1),
            ("dup", op::OP_dup),
            ("dup1", op::OP_dup1),
            ("dup2", op::OP_dup2),
            ("dup3", op::OP_dup3),
            ("insert2", op::OP_insert2),
            ("insert3", op::OP_insert3),
            ("insert4", op::OP_insert4),
            ("perm3", op::OP_perm3),
            ("perm4", op::OP_perm4),
            ("perm5", op::OP_perm5),
            ("swap", op::OP_swap),
            ("swap2", op::OP_swap2),
            ("rot3l", op::OP_rot3l),
            ("rot3r", op::OP_rot3r),
            ("rot4l", op::OP_rot4l),
            ("rot5l", op::OP_rot5l),
        ];
        for (name, code) in shuffles {
            let mut bytes = Vec::new();
            for i in 0..5 {
                bytes.extend_from_slice(&push_i32(i));
            }
            bytes.extend_from_slice(&[*code]);
            bytes.extend_from_slice(&ret);
            cases.push((name, bytes, 0, 0));
        }

        // Locals and arguments.
        cases.push(("get_loc", seq(&[&loc(op::OP_get_loc, 0), &ret]), 0, 1));
        cases.push((
            "put_loc",
            seq(&[&push_i32(0), &loc(op::OP_put_loc, 0), &push_i32(1), &ret]),
            0,
            1,
        ));
        cases.push((
            "set_loc",
            seq(&[&push_i32(0), &loc(op::OP_set_loc, 0), &ret]),
            0,
            1,
        ));
        cases.push(("get_arg", seq(&[&loc(op::OP_get_arg, 0), &ret]), 1, 0));
        cases.push((
            "put_arg",
            seq(&[&push_i32(0), &loc(op::OP_put_arg, 0), &push_i32(1), &ret]),
            1,
            0,
        ));
        cases.push((
            "set_arg",
            seq(&[&push_i32(0), &loc(op::OP_set_arg, 0), &ret]),
            1,
            0,
        ));
        cases.push((
            "inc_loc",
            seq(&[
                &push_i32(0),
                &loc(op::OP_put_loc, 0),
                &loc8(op::OP_inc_loc, 0),
                &push_i32(1),
                &ret,
            ]),
            0,
            1,
        ));
        cases.push((
            "dec_loc",
            seq(&[
                &push_i32(0),
                &loc(op::OP_put_loc, 0),
                &loc8(op::OP_dec_loc, 0),
                &push_i32(1),
                &ret,
            ]),
            0,
            1,
        ));

        // Terminators / no-ops.
        cases.push(("return_undef", vec![op::OP_return_undef], 0, 0));
        cases.push(("nop", seq(&[&[op::OP_nop], &[op::OP_return_undef]]), 0, 0));
        cases.push((
            "goto",
            seq(&[&branch(op::OP_goto, 4), &[op::OP_return_undef]]),
            0,
            0,
        ));
        // push 1; if_false L(16); push 2; return; L: return_undef
        let if_body = seq(&[
            &push_i32(1),
            &branch(op::OP_if_false, 10),
            &push_i32(2),
            &[op::OP_return, op::OP_return_undef],
        ]);
        cases.push(("if_false", if_body.clone(), 0, 0));
        let if_true_body = {
            let mut b = if_body.clone();
            b[5] = op::OP_if_true;
            b
        };
        cases.push(("if_true", if_true_body, 0, 0));

        // Helpers: binary then unary.
        let binary: &[(&str, u8)] = &[
            ("add", op::OP_add),
            ("sub", op::OP_sub),
            ("mul", op::OP_mul),
            ("div", op::OP_div),
            ("mod", op::OP_mod),
            ("lt", op::OP_lt),
            ("lte", op::OP_lte),
            ("gt", op::OP_gt),
            ("gte", op::OP_gte),
            ("eq", op::OP_eq),
            ("neq", op::OP_neq),
            ("strict_eq", op::OP_strict_eq),
            ("strict_neq", op::OP_strict_neq),
            ("and", op::OP_and),
            ("or", op::OP_or),
            ("xor", op::OP_xor),
            ("shl", op::OP_shl),
            ("sar", op::OP_sar),
            ("shr", op::OP_shr),
        ];
        for (name, code) in binary {
            cases.push((
                name,
                seq(&[&push_i32(1), &push_i32(2), &[*code], &ret]),
                0,
                0,
            ));
        }
        let unary: &[(&str, u8)] = &[
            ("plus", op::OP_plus),
            ("neg", op::OP_neg),
            ("inc", op::OP_inc),
            ("dec", op::OP_dec),
            ("lnot", op::OP_lnot),
            ("not", op::OP_not),
            ("typeof", op::OP_typeof),
        ];
        for (name, code) in unary {
            cases.push((name, seq(&[&push_i32(1), &[*code], &ret]), 0, 0));
        }

        for (name, bytes, args, vars) in cases {
            compile_bytes(bytes, args, vars)
                .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }
    }

    #[test]
    fn push_const_lowers_against_the_constant_pool() {
        let mut snap = snapshot(
            seq(&[&[op::OP_push_const], &u32b(0), &[op::OP_return]]),
            0,
            0,
        );
        snap.const_meta = Box::new([ConstMeta { tag: 0 }]);
        let isa = crate::jit::build_host_isa().unwrap();
        let verified = verify(snap, VerifyLimits::default()).unwrap();
        assert!(compile(&verified, isa.as_ref()).is_ok());
    }

    #[test]
    fn entry_opcode_unsupported_fails_closed() {
        // `object` is verifier-accepted (pushes one value) but not lowered; a
        // function that starts with it must not be installed.
        let result = compile_bytes(seq(&[&[op::OP_object], &[op::OP_return]]), 0, 0);
        assert!(result.is_err());
    }

    #[test]
    fn unsupported_op_mid_block_still_compiles_to_a_resume() {
        // First op is supported, so the function is installed; the unsupported
        // `object` lowers to a `Resume` exit and the interpreter finishes it.
        let result = compile_bytes(
            seq(&[&push_i32(1), &[op::OP_object], &[op::OP_return]]),
            0,
            0,
        );
        assert!(result.is_ok());
    }
}
