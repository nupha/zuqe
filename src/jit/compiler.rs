// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Baseline (Tier1) lowering: verified bytecode straight to Cranelift IR.
//!
//! Values live in the frame buffer, exactly where the interpreter keeps them —
//! with one exception (epic F4): inside a basic block a stack slot may live
//! only in a register, and is written back before anything can observe the
//! frame (helper calls, exits, branch terminators, end of block). Across a
//! block edge the slots the successor reads in registers travel as block
//! parameters instead of a store/load pair (epic F4 step 2, see
//! [`carried_sets`]). Every observation point still flushes first, so the frame
//! stays authoritative and an exit needs no pc-keyed deopt map — materializing
//! *is* the flush, keyed by the verifier's proven stack heights (D-25 extended
//! to cross-block residency). The operand-stack pointer is an SSA value; it is
//! written back to `StackFrame.cur_sp_offset` before any helper call and before
//! any exit, the only moments a collection can observe it. Anything the
//! baseline cannot translate terminates the native run with `Resume`, and the
//! interpreter picks up at that bytecode offset.
//!
//! There is no separate IR layer: with no optimization passes above it, the
//! lowering reads `VerifiedFunction` directly (C1 folded into C2).

use std::{cell::Cell, collections::HashMap, mem::offset_of};

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
        HELPER_ADD, HELPER_ADD_LOC, HELPER_AND, HELPER_DEC, HELPER_DELETE, HELPER_DIV, HELPER_EQ,
        HELPER_GET_ARRAY_EL, HELPER_GET_ARRAY_EL2, HELPER_GET_ARRAY_EL3, HELPER_GET_FIELD,
        HELPER_GET_FIELD2, HELPER_GET_VAR, HELPER_GET_VAR_UNDEF, HELPER_GT, HELPER_GTE, HELPER_IN,
        HELPER_INC, HELPER_INSTANCEOF, HELPER_LNOT, HELPER_LT, HELPER_LTE, HELPER_MOD, HELPER_MUL,
        HELPER_NEG, HELPER_NEQ, HELPER_NIP_CATCH, HELPER_NOT, HELPER_OR, HELPER_PLUS,
        HELPER_POST_DEC, HELPER_POST_INC, HELPER_POW, HELPER_PUT_ARRAY_EL, HELPER_PUT_FIELD,
        HELPER_PUT_VAR, HELPER_PUT_VAR_INIT, HELPER_SAFEPOINT, HELPER_SAR, HELPER_SHL, HELPER_SHR,
        HELPER_STRICT_EQ, HELPER_STRICT_NEQ, HELPER_SUB, HELPER_TO_BOOL, HELPER_TO_OBJECT,
        HELPER_TO_PROPKEY, HELPER_TYPEOF, HELPER_XOR, JitExecFrame,
    },
    bytecode::{cfg::ControlFlowGraph, decode::Instruction, verify::VerifiedFunction},
    error::JitError,
    feedback::{CLASS_INT, SiteFeedback},
};

/// Exit codes as written by generated code (keep in sync with `JitExitKind`).
const EXIT_DONE: i64 = 0;
const EXIT_RESUME: i64 = 6;
const EXIT_RETRY: i64 = 7;

const UNDEFINED_BITS: i64 = 3 << 48;
const BOOL_TAG: i64 = 1 << 48;
/// `js_new_float64(d) == d.to_bits() - (0xFFF9 << 48)` for a non-NaN value.
const FLOAT64_ADDEND: i64 = 0xFFF9 << 48;

const O_ARG_BASE: i32 = offset_of!(JitExecFrame, arg_base) as i32;
const O_VAR_BASE: i32 = offset_of!(JitExecFrame, var_base) as i32;
const O_SP_BASE: i32 = offset_of!(JitExecFrame, sp_base) as i32;
const O_SP: i32 = offset_of!(JitExecFrame, sp) as i32;
const O_CPOOLS: i32 = offset_of!(JitExecFrame, cpools) as i32;
const O_CUR_SP: i32 = offset_of!(JitExecFrame, cur_sp_offset) as i32;
const O_HELPERS: i32 = offset_of!(JitExecFrame, helpers) as i32;
const O_THIS: i32 = offset_of!(JitExecFrame, this) as i32;
const O_IMM: i32 = offset_of!(JitExecFrame, imm) as i32;
const O_IMM2: i32 = offset_of!(JitExecFrame, imm2) as i32;
const O_PC: i32 = offset_of!(JitExecFrame, pc) as i32;
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
                | "get_var"
                | "get_var_undef"
                | "put_var"
                | "put_var_init"
                | "get_array_el"
                | "get_array_el2"
                | "get_array_el3"
                | "put_array_el"
                | "get_field"
                | "get_field2"
                | "put_field"
                | "add_loc"
                | "post_inc"
                | "post_dec"
                | "catch"
                | "gosub"
                | "ret"
        )
}

fn helper_for(name: &str) -> Option<usize> {
    Some(match name {
        "add" => HELPER_ADD,
        "sub" => HELPER_SUB,
        "mul" => HELPER_MUL,
        "div" => HELPER_DIV,
        "mod" => HELPER_MOD,
        "pow" => HELPER_POW,
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
        "to_object" => HELPER_TO_OBJECT,
        "to_propkey" => HELPER_TO_PROPKEY,
        "in" => HELPER_IN,
        "instanceof" => HELPER_INSTANCEOF,
        "delete" => HELPER_DELETE,
        "nip_catch" => HELPER_NIP_CATCH,
        _ => return None,
    })
}

/// Arithmetic / comparison / bitwise operators with an inline two-int fast
/// path. Every other operand shape falls back to the interpreter helper, which
/// owns the full ToNumber/ToInt32/overflow/string semantics.
#[derive(Clone, Copy)]
enum FastOp {
    Add,
    Sub,
    Mul,
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
    Neq,
    StrictEq,
    StrictNeq,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Sar,
}

/// Maps an opcode name to its fast-path form and the fallback helper id.
fn fast_op(name: &str) -> Option<(FastOp, usize)> {
    use FastOp::*;
    Some(match name {
        "add" => (Add, HELPER_ADD),
        "sub" => (Sub, HELPER_SUB),
        "mul" => (Mul, HELPER_MUL),
        "lt" => (Lt, HELPER_LT),
        "lte" => (Lte, HELPER_LTE),
        "gt" => (Gt, HELPER_GT),
        "gte" => (Gte, HELPER_GTE),
        "eq" => (Eq, HELPER_EQ),
        "neq" => (Neq, HELPER_NEQ),
        "strict_eq" => (StrictEq, HELPER_STRICT_EQ),
        "strict_neq" => (StrictNeq, HELPER_STRICT_NEQ),
        "and" => (And, HELPER_AND),
        "or" => (Or, HELPER_OR),
        "xor" => (Xor, HELPER_XOR),
        "shl" => (Shl, HELPER_SHL),
        "shr" => (Shr, HELPER_SHR),
        "sar" => (Sar, HELPER_SAR),
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

struct State<'a> {
    blocks: HashMap<u32, Block>,
    pending: Vec<u32>,
    feedback: &'a [(u32, SiteFeedback)],
    specialized: &'a Cell<u64>,
    /// Block-local write-back cache for operand-stack slots (epic F4).
    stack: StackCache,
    /// Epic F4 step 2: per block start pc, the slots its code reads in
    /// registers before it can observe the frame. Each is a block parameter
    /// that every incoming edge supplies (see [`State::edge_args`]).
    carried: HashMap<u32, Vec<u32>>,
    /// Proven entry stack height per block start pc. An edge's `sp` equals it,
    /// so it addresses the frame fallback loads even when the model's own
    /// height is stale (a helper-terminated block never re-anchors).
    entry_heights: HashMap<u32, u16>,
}

/// Write-back cache for operand-stack slots (epic F4 steps 1 and 2).
///
/// A slot index is 1-based from the stack bottom and is re-anchored from the
/// verifier's proven height at the start of every instruction, so entries
/// cannot drift across a helper call — whose net stack effect is only known at
/// run time. Entries hold values that exist **only in registers**: they must be
/// written back before anything can observe the frame, so the flush points are
/// helper calls, exits, branch terminators and the end of the block. Producers
/// that already store to memory (arithmetic fast paths, helpers, shuffles)
/// insert nothing, which keeps memory authoritative for them and means a flush
/// never has to reconcile two copies.
///
/// Step 2 extends the lifetime across a block edge: the successor's carried
/// slots arrive as block parameters, which [`lower`] seeds into the model, and
/// the incoming edge passes them on instead of storing them (the flush points
/// above still apply — a carried value is flushed like any other before the
/// frame is observable).
#[derive(Default)]
struct StackCache {
    height: u32,
    entries: HashMap<u32, Value>,
    /// Reused scratch for flush ordering: a flush runs before every helper
    /// call, so it must not allocate.
    scratch: Vec<u32>,
    /// Whether the current instruction changed `height` through *this* cache
    /// (push/pop/drop/adjust). Helper arms leave `height` stale — the next
    /// instruction re-anchors it — so only tracked effects are worth the
    /// per-instruction cross-check against the verifier.
    tracked: bool,
}

impl StackCache {
    /// Re-anchor to the verified height at the start of an instruction.
    const fn anchor(&mut self, height: u32) {
        self.height = height;
        self.tracked = false;
    }

    /// Byte offset of slot `k` from the current `sp`. At every point this
    /// cache is consulted, `sp == sp_base + height * 8`; the offset goes
    /// straight into the memory instruction, so a flush costs no address math.
    const fn slot_offset(height: u32, k: u32) -> i32 {
        ((k as i64 - 1 - height as i64) * 8) as i32
    }

    /// Keep `v` in a register instead of storing it; the store happens at the
    /// next flush, or never if the slot dies first (DCE).
    fn push(&mut self, b: &mut FunctionBuilder, sp: Value, v: Value) -> Value {
        self.height = self.height.saturating_add(1);
        self.tracked = true;
        self.entries.insert(self.height, v);
        b.ins().iadd_imm(sp, 8)
    }

    /// Pop the top slot: reuse the register-resident value when there is one,
    /// otherwise load it from the frame.
    fn pop(&mut self, b: &mut FunctionBuilder, sp: Value) -> (Value, Value) {
        debug_assert!(self.height > 0, "verified code cannot underflow");
        let k = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        match self.entries.remove(&k) {
            Some(v) => (b.ins().iadd_imm(sp, -8), v),
            None => {
                let sp = b.ins().iadd_imm(sp, -8);
                let v = b.ins().load(types::I64, MemFlags::new(), sp, 0);
                (sp, v)
            }
        }
    }

    /// Read slot `k` without consuming it.
    fn read(&self, b: &mut FunctionBuilder, sp: Value, k: u32) -> Value {
        match self.entries.get(&k) {
            Some(&v) => v,
            None => b.ins().load(
                types::I64,
                MemFlags::new(),
                sp,
                Self::slot_offset(self.height, k),
            ),
        }
    }

    /// Read the top slot without consuming it.
    fn read_top(&self, b: &mut FunctionBuilder, sp: Value) -> Value {
        self.read(b, sp, self.height)
    }

    /// Forget the top slot without materializing it: the value is dead.
    fn drop_top(&mut self) {
        let k = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        self.entries.remove(&k);
    }

    /// A binary op popped two operands and pushed one result. Both operand
    /// entries die here and the result is *not* cached: the fast path and the
    /// helper each store it, so memory stays authoritative for slot `height`.
    fn consumed_binary(&mut self) {
        let top = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        self.entries.remove(&top);
        self.entries.remove(&(top - 1));
    }

    /// Apply a raw stack effect the lowering computes itself — shuffles are
    /// the only ops that pop `n` and push `k` without touching cached values.
    fn adjust(&mut self, delta: i32) {
        let height = i64::from(self.height) + i64::from(delta);
        debug_assert!(height >= 0, "verified code cannot underflow");
        self.height = height.max(0) as u32;
        self.tracked = true;
    }

    /// Write register-resident slots back to the frame, except `keep` — the
    /// slots a *sibling* branch still needs in registers (they die on the path
    /// that skips this flush). Keys are sorted so the emitted store order
    /// (and with it the generated code) is stable.
    ///
    /// A flush must be emitted in a block that dominates every path whose
    /// model it changes: clearing the model at compile time is only correct if
    /// the stores really execute on all those paths.
    fn flush_except(&mut self, b: &mut FunctionBuilder, sp: Value, keep: &[u32]) {
        if self.entries.is_empty() {
            return;
        }
        self.scratch.clear();
        self.scratch
            .extend(self.entries.keys().copied().filter(|k| !keep.contains(k)));
        self.scratch.sort_unstable();
        let height = self.height;
        for &k in &self.scratch {
            let Some(&v) = self.entries.get(&k) else {
                continue;
            };
            b.ins()
                .store(MemFlags::new(), v, sp, Self::slot_offset(height, k));
            self.entries.remove(&k);
        }
        self.scratch.clear();
    }

    /// Write every register-resident slot back to the frame.
    fn flush(&mut self, b: &mut FunctionBuilder, sp: Value) {
        self.flush_except(b, sp, &[]);
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'a> State<'a> {
    fn block_for(&mut self, b: &mut FunctionBuilder, pc: u32) -> Block {
        if let Some(block) = self.blocks.get(&pc) {
            return *block;
        }
        let block = b.create_block();
        // Epic F4 step 2: the slots this block consumes in registers are block
        // parameters. They are appended here, before any edge exists, so every
        // edge to this block sees the same signature (`edge_args` supplies it).
        for _ in 0..self.carried_for(pc).len() {
            b.append_block_param(block, types::I64);
        }
        self.blocks.insert(pc, block);
        self.pending.push(pc);
        block
    }

    /// Slots block `pc` reads from registers before it can observe the frame —
    /// empty when it reads none, or for a pc the walk never saw.
    fn carried_for(&self, pc: u32) -> &[u32] {
        self.carried.get(&pc).map_or(&[], Vec::as_slice)
    }

    /// Sorted, deduplicated union of several successors' carried sets: the
    /// `keep` a shared flush has to spare.
    fn carried_union(&self, pcs: &[u32]) -> Vec<u32> {
        let mut keep = Vec::new();
        for &pc in pcs {
            keep.extend_from_slice(self.carried_for(pc));
        }
        keep.sort_unstable();
        keep.dedup();
        keep
    }

    /// Leave on edges to every `(block, pc)` in `targets`: write back what none
    /// of them carries, then read each carried slot as a block argument — the
    /// register value when this path still holds one, the frame otherwise. The
    /// model is delivered here (cleared); the successor re-seeds it from the
    /// parameters (see [`lower`]), so a carried slot crosses the edge with
    /// neither a store nor a reload.
    ///
    /// `flush_except` only emits stores for slots outside the union, so it must
    /// run where those stores dominate every path this edge can be reached
    /// from: the fallthrough and a forward `goto` are single-path, and the
    /// conditional arms are reached after `lower_if`'s shared full flush, which
    /// already left the model empty. Never call it once per arm of an
    /// unflushed split — the stores would exist on one path only.
    fn edge_args(
        &mut self,
        b: &mut FunctionBuilder,
        sp: Value,
        targets: &[(Block, u32)],
    ) -> Vec<Vec<Value>> {
        let pcs: Vec<u32> = targets.iter().map(|&(_, pc)| pc).collect();
        let keep = self.carried_union(&pcs);
        self.stack.flush_except(b, sp, &keep);
        let model_height = self.stack.height;
        let mut out = Vec::with_capacity(targets.len());
        for &(_, pc) in targets {
            debug_assert!(
                self.entry_heights.contains_key(&pc),
                "edge target pc {pc} was never walked"
            );
            let entry_h = self
                .entry_heights
                .get(&pc)
                .map_or(model_height, |&h| u32::from(h));
            // A stale model height implies a flushed (empty) model — helpers
            // leave `height` at the anchor and clear every entry.
            debug_assert!(self.stack.entries.is_empty() || model_height == entry_h);
            let mut args = Vec::with_capacity(self.carried_for(pc).len());
            for &k in self.carried_for(pc) {
                let value = match self.stack.entries.get(&k) {
                    Some(&v) => v,
                    None => b.ins().load(
                        types::I64,
                        MemFlags::new(),
                        sp,
                        StackCache::slot_offset(entry_h, k),
                    ),
                };
                args.push(value);
            }
            out.push(args);
        }
        // Delivered as block arguments; the successor seeds its own model.
        self.stack.entries.clear();
        out
    }

    /// Epic F2: did F1 feedback observe *only* `Int` at this site? Feedback is
    /// sorted by pc, so a binary search answers it. `both` is false for
    /// single-value sites (`inc_loc`/`dec_loc`), whose `rhs` slot stays 0.
    fn int_only(&self, pc: u32, both: bool) -> bool {
        int_only(self.feedback, pc, both)
    }

    fn note_specialized(&self) {
        self.specialized.set(self.specialized.get() + 1);
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

/// Write `out_pc`/`out_sp` and return `kind`. Register-resident stack slots
/// are materialized first: the interpreter resumes by reading them.
fn exit(state: &mut State, b: &mut FunctionBuilder, fv: &FrameVals, kind: i64, pc: u32, sp: Value) {
    state.stack.flush(b, sp);
    let pcv = b.ins().iconst(types::I32, i64::from(pc));
    b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
    let kind = b.ins().iconst(types::I32, kind);
    b.ins().return_(&[kind]);
}

/// Exit with a status produced at runtime (helper status 1/2 is exactly
/// `JitExitKind::Exception`/`Interrupt`).
///
/// Only called from `call_stack_helper`/`safepoint` failure blocks, i.e. after
/// the cache was flushed and dropped — so the frame already holds every slot
/// this exit exposes to the interpreter.
fn exit_status(b: &mut FunctionBuilder, fv: &FrameVals, status: Value, pc: u32, sp: Value) {
    let pcv = b.ins().iconst(types::I32, i64::from(pc));
    b.ins().store(MemFlags::new(), pcv, fv.frame, O_OUT_PC);
    let slots = slots_of(b, fv, sp);
    b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
    b.ins().return_(&[status]);
}

/// Publish `sp` (for GC) and call stack helper `id`; returns the reloaded sp.
/// On helper failure the calling block is terminated with the exit.
///
/// The frame is the helper's window into the operand stack, so every
/// register-resident slot is written back before the call and the cache is
/// dropped — the helper may rewrite the stack (and runs JS, which can throw).
fn call_stack_helper(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    id: usize,
    pc_after: u32,
) -> Value {
    state.stack.flush(b, sp);
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
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    pc_after: u32,
) -> Block {
    // The safepoint can collect: GC scans the frame up to `cur_sp`, so the
    // register-resident slots must be visible in memory first.
    state.stack.flush(b, sp);
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

/// Whether the baseline can translate `verified`'s first opcode. When false,
/// [`compile`] refuses deterministically, so callers should settle the function
/// fail-closed rather than retry.
pub(crate) fn entry_is_translatable(verified: &VerifiedFunction) -> bool {
    verified
        .instructions()
        .first()
        .is_some_and(|instruction| supported(instruction.name()))
}

/// Compile one verified function. Returns code bytes for the caller to publish.
///
/// `specialized_ops` accumulates how many sites got a tier-2 speculative fast
/// path (epic F2), so the caller can report it in metrics.
pub(crate) fn compile(
    verified: &VerifiedFunction,
    isa: &dyn TargetIsa,
    specialized_ops: &Cell<u64>,
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

    let mut cgen = Context::new();
    cgen.func.signature = sig;
    cgen.func.name = UserFuncName::user(0, 1);

    let mut fbctx = FunctionBuilderContext::new();
    let mut b = FunctionBuilder::new(&mut cgen.func, &mut fbctx);

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
    let osr_pcs = collect_osr_pcs(verified);
    lower(
        &mut b,
        &fv,
        sp_var,
        helper_sig,
        truthy_sig,
        verified,
        strict,
        &osr_pcs,
        specialized_ops,
    );

    b.seal_all_blocks();
    b.finalize();

    let mut ctrl = ControlPlane::default();
    let compiled = cgen
        .compile(isa, &mut ctrl)
        .map_err(|e| JitError::Cranelift(format!("{e:?}")))?;
    if !compiled.buffer.relocs().is_empty() {
        // No external symbol is referenced, so a reloc means codegen emitted
        // something we cannot relocate: fail closed.
        return Err(JitError::Cranelift("unexpected relocations".into()));
    }

    Ok(compiled.code_buffer().into())
}

/// Pc values the compiled entry can be started at. 0 is the function entry;
/// loop headers (hot-loop OSR) and the pc right after each call are the OSR
/// entry points. After a call, the trampoline re-enters this frame at the
/// post-call pc, so the function keeps running natively instead of staying in
/// the interpreter.
fn collect_osr_pcs(verified: &VerifiedFunction) -> Vec<u32> {
    let mut pcs: Vec<u32> = verified
        .osr_points()
        .iter()
        .map(|point| point.pc())
        .collect();
    for instruction in verified.instructions() {
        if matches!(
            instruction.name(),
            "call" | "call_method" | "call_constructor"
        ) {
            pcs.push(instruction.pc() + instruction.size() as u32);
        }
    }
    pcs.sort_unstable();
    pcs.dedup();
    pcs
}

/// Epic F2: did F1 feedback observe *only* `Int` at this site? Feedback is
/// sorted by pc, so a binary search answers it. `both` is false for
/// single-value sites (`inc_loc`/`dec_loc`), whose `rhs` slot stays 0.
fn int_only(feedback: &[(u32, SiteFeedback)], pc: u32, both: bool) -> bool {
    feedback
        .binary_search_by_key(&pc, |&(site, _)| site)
        .is_ok_and(|i| {
            let site = feedback[i].1;
            site.lhs == CLASS_INT && (!both || site.rhs == CLASS_INT)
        })
}

/// What one instruction does to the slots below its entry height before it can
/// observe the frame (epic F4 step 2). Mirrors `lower_insn`'s dispatch order; a
/// misclassification only costs the carry, never correctness — every edge
/// materializes what it does not pass (see [`State::edge_args`]).
enum Touch {
    /// Nothing the cache holds: pushes, `get_loc`, `drop`, `nop`, `goto`, …
    Pass,
    /// Reads slot `k` in a register; the block keeps being scanned.
    Read(u32),
    /// Reads these slots in registers, then flushes the rest: only they can
    /// still be carried in.
    ReadFlush(Vec<u32>),
    /// Flushes before touching a cached value (helper call, shuffle, `Resume`
    /// exit): nothing the block entered with survives this instruction.
    Flush,
}

/// [`lower_insn`] in miniature: which stack slots does `name` read from
/// registers, and does it flush before reading any? `h` is the proven height
/// before the instruction.
fn touch(name: &str, h: u32, strict: bool, feedback: &[(u32, SiteFeedback)], pc: u32) -> Touch {
    if fast_op(name).is_some() {
        // The operands are read first; `flush_except` then spares them.
        return Touch::ReadFlush(vec![h - 1, h]);
    }
    if helper_for(name).is_some() || shuffle_outputs(name).is_some() {
        return Touch::Flush;
    }
    match name {
        "nop" | "push_i32" | "push_const" | "undefined" | "null" | "push_false" | "push_true"
        | "catch" | "get_loc" | "get_arg" | "drop" | "goto" | "gosub" => Touch::Pass,
        "push_this" if strict => Touch::Pass,
        "put_loc" | "put_arg" | "set_loc" | "set_arg" => Touch::Read(h),
        // Specialized `add_loc` reads the operand before `flush_except([top])`.
        "add_loc" if int_only(feedback, pc, true) => Touch::ReadFlush(vec![h]),
        // Both variants flush before anything reads the stack (`inc_loc`
        // reads the local out of the frame, not out of a cache entry).
        "add_loc" | "inc_loc" | "dec_loc" => Touch::Flush,
        "if_true" | "if_false" | "return" | "ret" => Touch::ReadFlush(vec![h]),
        // Not lowered (`Resume` exit) or lowered as a helper call above.
        _ => Touch::Flush,
    }
}

/// Epic F4 step 2: for every block, the operand-stack slots its code reads
/// from registers *before* the first instruction that can observe the frame.
/// Those cross each incoming edge as block parameters instead of a store/load
/// pair; everything else is written back on the edge exactly as before.
///
/// The walk is straight-line and stops at the first flush — past that point
/// the slots are memory-side, so carrying them would buy nothing (a helper
/// flushes them again anyway) or cost a load. Slots above the entry height were
/// pushed inside the block and cannot be incoming arguments.
///
/// Returns `(carried, entry_heights)` keyed by block start pc: every CFG block
/// and every OSR resume point, the pcs [`lower`] can create a block for.
fn carried_sets(
    verified: &VerifiedFunction,
    osr_pcs: &[u32],
    strict: bool,
) -> (HashMap<u32, Vec<u32>>, HashMap<u32, u16>) {
    let instructions = verified.instructions();
    let cfg = verified.control_flow_graph();
    let heights = verified.stack_heights();
    let feedback = &verified.snapshot().feedback;
    let mut starts: Vec<u32> = cfg.blocks().iter().map(|block| block.start_pc()).collect();
    starts.extend_from_slice(osr_pcs);
    starts.sort_unstable();
    starts.dedup();
    let mut carried: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut entry_heights = HashMap::new();
    for pc in starts {
        // Unreachable blocks are never lowered (their heights would read 0).
        if !verified.is_reachable(pc) {
            continue;
        }
        // Same range `lower` will walk, including OSR points mid-block.
        let Some((range, _, _)) = block_range(instructions, cfg, pc) else {
            continue;
        };
        let entry_h = heights[range.start];
        entry_heights.insert(pc, entry_h);
        let mut marked = vec![false; entry_h as usize];
        for index in range {
            let h = heights[index] as u32;
            let instruction = &instructions[index];
            match touch(instruction.name(), h, strict, feedback, instruction.pc()) {
                Touch::Pass => {}
                Touch::Read(k) => mark(&mut marked, entry_h, k),
                Touch::ReadFlush(slots) => {
                    for k in slots {
                        mark(&mut marked, entry_h, k);
                    }
                    break;
                }
                Touch::Flush => break,
            }
        }
        let slots: Vec<u32> = marked
            .iter()
            .enumerate()
            .filter(|&(_, &is_read)| is_read)
            .map(|(index, _)| index as u32 + 1)
            .collect();
        if !slots.is_empty() {
            carried.insert(pc, slots);
        }
    }
    (carried, entry_heights)
}

fn mark(marked: &mut [bool], entry_h: u16, k: u32) {
    if (1..=u32::from(entry_h)).contains(&k) {
        marked[(k - 1) as usize] = true;
    }
}

/// Entry dispatch. `frame.pc == 0` runs the function entry; an OSR pc starts at
/// its block with `sp` taken from the frame's current stack top; an unknown pc
/// exits `Retry` so the interpreter runs from there.
fn emit_osr_prologue(
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    state: &mut State,
    entry_block: Block,
    osr_pcs: &[u32],
) {
    let pc = b.ins().load(types::I32, MemFlags::new(), fv.frame, O_PC);
    let is_entry = b.ins().icmp_imm(IntCC::Equal, pc, 0);
    // The entry block carries nothing: the function entry proves height 0.
    debug_assert!(state.carried_for(0).is_empty());
    let dispatch = b.create_block();
    b.ins().brif(is_entry, entry_block, &[], dispatch, &[]);
    b.switch_to_block(dispatch);

    for &pc_val in osr_pcs {
        let target = state.block_for(b, pc_val);
        let osr_block = b.create_block();
        let next = b.create_block();
        let matches = b.ins().icmp_imm(IntCC::Equal, pc, i64::from(pc_val));
        b.ins().brif(matches, osr_block, &[], next, &[]);
        // An OSR pc resumes with the frame's current operand-stack top
        // (`fv.cur_sp` points at `StackFrame.cur_sp_offset`).
        b.switch_to_block(osr_block);
        let slots = b.ins().load(types::I32, MemFlags::new(), fv.cur_sp, 0);
        let slots64 = b.ins().uextend(types::I64, slots);
        let byte_off = b.ins().ishl_imm(slots64, 3);
        let sp_osr = b.ins().iadd(fv.sp_base, byte_off);
        b.def_var(sp_var, sp_osr);
        // Epic F4 step 2: the interpreter left every slot in the frame, so the
        // carried parameters are frame loads at the proven entry height.
        let args = state
            .edge_args(b, sp_osr, &[(target, pc_val)])
            .pop()
            .expect("one target");
        b.ins().jump(target, &args);
        b.switch_to_block(next);
    }

    // Unknown pc: report it (with the current sp) and let the interpreter run.
    let slots = b.ins().load(types::I32, MemFlags::new(), fv.cur_sp, 0);
    b.ins().store(MemFlags::new(), pc, fv.frame, O_OUT_PC);
    b.ins().store(MemFlags::new(), slots, fv.frame, O_OUT_SP);
    let retry = b.ins().iconst(types::I32, EXIT_RETRY);
    b.ins().return_(&[retry]);
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
    osr_pcs: &[u32],
    specialized_ops: &Cell<u64>,
) {
    let instructions = verified.instructions();
    let cfg = verified.control_flow_graph();
    let (carried, entry_heights) = carried_sets(verified, osr_pcs, strict);
    let mut state = State {
        blocks: HashMap::new(),
        pending: Vec::new(),
        feedback: &verified.snapshot().feedback,
        specialized: specialized_ops,
        stack: StackCache::default(),
        carried,
        entry_heights,
    };
    let entry_block = state.block_for(b, 0);
    if osr_pcs.is_empty() {
        // The entry stack is empty, so the entry block has no parameters.
        debug_assert!(state.carried_for(0).is_empty());
        b.ins().jump(entry_block, &[]);
    } else {
        emit_osr_prologue(b, fv, sp_var, &mut state, entry_block, osr_pcs);
    }

    while let Some(start_pc) = state.pending.pop() {
        // A block either starts at a CFG boundary, or — for an OSR resume point
        // after a call — in the middle of a CFG block. In the latter case lower
        // from `start_pc` to the end of the containing block, keeping the same
        // tail for the fallthrough.
        let Some((range, tail_index, successors)) = block_range(instructions, cfg, start_pc) else {
            continue;
        };
        let cranelift_block = state.blocks[&start_pc];
        b.switch_to_block(cranelift_block);
        // Epic F4 step 2: slots carried across the incoming edges arrive as
        // block parameters — re-seed the model from them. The model itself
        // never crosses a block; every edge delivers it through `edge_args`.
        state.stack.entries.clear();
        let carried: Vec<u32> = state.carried_for(start_pc).to_vec();
        let params = b.block_params(cranelift_block);
        debug_assert_eq!(params.len(), carried.len(), "block {start_pc} parameters");
        for (&k, &v) in carried.iter().zip(params) {
            state.stack.entries.insert(k, v);
        }
        let mut sp = b.use_var(sp_var);
        let mut stopped = false;

        let base = range.start;
        let total = range.end - base;
        for (offset, insn) in instructions[range].iter().enumerate() {
            // Anchor the stack cache to the verified entry height of this
            // instruction: pushes/pops inside it are tracked incrementally.
            let index = base + offset;
            let start_height = u32::from(verified.stack_heights()[index]);
            match lower_insn(
                b,
                fv,
                sp_var,
                helper_sig,
                truthy_sig,
                strict,
                &mut state,
                insn,
                start_height,
                sp,
                successors,
            ) {
                Step::Continue(new_sp) => {
                    sp = new_sp;
                    // The lowering's per-instruction stack effect must agree
                    // with the verifier's proven heights: every cache address
                    // is derived from them. Catches a divergence at the opcode
                    // that introduced it instead of at a later load.
                    if offset + 1 < total && state.stack.tracked {
                        debug_assert_eq!(
                            state.stack.height,
                            u32::from(verified.stack_heights()[base + offset + 1]),
                            "stack effect diverged from the verifier at pc {}",
                            insn.pc()
                        );
                    }
                }
                Step::Stop => {
                    stopped = true;
                    break;
                }
            }
        }

        if !stopped {
            // Only reached for blocks ending in a non-branch instruction: hand
            // the carried slots to the successor as parameters and materialize
            // the rest (the successor seeds its model from the parameters).
            let tail = &instructions[tail_index];
            let next = tail.pc() + tail.size() as u32;
            let next_block = state.block_for(b, next);
            let args = state
                .edge_args(b, sp, &[(next_block, next)])
                .pop()
                .expect("one target");
            b.def_var(sp_var, sp);
            b.ins().jump(next_block, &args);
        }
        // Every terminator delivers its model on the way out (see `Step::Stop`);
        // catch a path that forgot, otherwise a slot would silently stay in a
        // register the successor block cannot see.
        debug_assert!(
            state.stack.is_empty(),
            "a block ended with register-resident stack slots"
        );
    }
}

/// Instruction range a block starting at `start_pc` must lower, the index of
/// that range's tail (for the fallthrough), and the containing CFG block's
/// successors (for the dynamic `ret`). Handles both CFG boundaries and OSR
/// resume points that sit inside a CFG block.
fn block_range<'a>(
    instructions: &[Instruction],
    cfg: &'a ControlFlowGraph,
    start_pc: u32,
) -> Option<(std::ops::Range<usize>, usize, &'a [u32])> {
    if let Some(block) = cfg.block(start_pc) {
        let range = block.instruction_range();
        let tail = range.end - 1;
        return Some((range, tail, block.successors()));
    }
    let block = cfg
        .blocks()
        .iter()
        .find(|block| block.start_pc() <= start_pc && start_pc < block.end_pc())?;
    let range = block.instruction_range();
    let first = (range.start..range.end).find(|&index| instructions[index].pc() == start_pc)?;
    Some((first..range.end, range.end - 1, block.successors()))
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
    start_height: u32,
    sp: Value,
    ret_targets: &[u32],
) -> Step {
    let name = insn.name();
    let pc = insn.pc();
    let size = insn.size() as u32;
    // Verified entry height of *this* instruction: push/pop below track it
    // incrementally and the next instruction re-anchors it.
    state.stack.anchor(start_height);

    if let Some((op, helper_id)) = fast_op(name) {
        return lower_fast_binary(state, b, fv, sp_var, helper_sig, sp, insn, op, helper_id);
    }
    if let Some(id) = helper_for(name) {
        let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
        return Step::Continue(sp);
    }
    if let Some((pop, outputs)) = shuffle_outputs(name) {
        // Shuffles read *and* rewrite slots: materialize first, then work on
        // memory and leave the cache empty (memory stays authoritative).
        state.stack.flush(b, sp);
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
        state.stack.adjust(outputs.len() as i32 - pop as i32);
        let sp = b.ins().iadd_imm(base, (outputs.len() * 8) as i64);
        return Step::Continue(sp);
    }

    match name {
        "nop" => Step::Continue(sp),
        "push_i32" => {
            let v = b.ins().iconst(types::I64, i64::from(insn.operand_u32(1)));
            Step::Continue(state.stack.push(b, sp, v))
        }
        "push_const" => {
            let idx = insn.operand_u32(1) as i32;
            let v = b
                .ins()
                .load(types::I64, MemFlags::new(), fv.cpools, idx * 8);
            Step::Continue(state.stack.push(b, sp, v))
        }
        "push_atom_value" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
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
            Step::Continue(state.stack.push(b, sp, v))
        }
        "null" => {
            let v = b.ins().iconst(types::I64, 2i64 << 48);
            Step::Continue(state.stack.push(b, sp, v))
        }
        "get_var" | "get_var_undef" | "put_var" | "put_var_init" => {
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
            let id = match name {
                "get_var" => HELPER_GET_VAR,
                "get_var_undef" => HELPER_GET_VAR_UNDEF,
                "put_var" => HELPER_PUT_VAR,
                _ => HELPER_PUT_VAR_INIT,
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "get_array_el" | "get_array_el2" | "get_array_el3" | "put_array_el" => {
            let id = match name {
                "get_array_el" => HELPER_GET_ARRAY_EL,
                "get_array_el2" => HELPER_GET_ARRAY_EL2,
                "get_array_el3" => HELPER_GET_ARRAY_EL3,
                _ => HELPER_PUT_ARRAY_EL,
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "get_field" | "get_field2" | "put_field" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let ic = b.ins().iconst(types::I32, i64::from(ic_index(insn)));
            b.ins().store(MemFlags::new(), ic, fv.frame, O_IMM2);
            let id = match name {
                "get_field" => HELPER_GET_FIELD,
                "get_field2" => HELPER_GET_FIELD2,
                _ => HELPER_PUT_FIELD,
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "add_loc" => {
            // Epic F2: F1 proved this site int-monomorphic, so speculate an
            // inline int add. A non-int operand or overflow hands the op back
            // to the helper (still in native code).
            if state.int_only(pc, true) {
                state.note_specialized();
                return lower_add_loc_int(state, b, fv, sp_var, helper_sig, sp, insn);
            }
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_ADD_LOC, pc + size);
            Step::Continue(sp)
        }
        "post_inc" | "post_dec" => {
            let id = if name == "post_inc" {
                HELPER_POST_INC
            } else {
                HELPER_POST_DEC
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "push_false" => {
            let v = b.ins().iconst(types::I64, 1i64 << 48);
            Step::Continue(state.stack.push(b, sp, v))
        }
        "push_true" => {
            let v = b.ins().iconst(types::I64, (1i64 << 48) | 1);
            Step::Continue(state.stack.push(b, sp, v))
        }
        "push_this" if strict => Step::Continue(state.stack.push(b, sp, fv.this)),
        "drop" => {
            state.stack.drop_top();
            Step::Continue(b.ins().iadd_imm(sp, -8))
        }
        "get_loc" => {
            let v = b.ins().load(
                types::I64,
                MemFlags::new(),
                fv.var_base,
                loc_offset_u16(insn),
            );
            Step::Continue(state.stack.push(b, sp, v))
        }
        "get_arg" => {
            let v = b.ins().load(
                types::I64,
                MemFlags::new(),
                fv.arg_base,
                loc_offset_u16(insn),
            );
            Step::Continue(state.stack.push(b, sp, v))
        }
        "put_loc" => {
            let (sp, v) = state.stack.pop(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        "put_arg" => {
            let (sp, v) = state.stack.pop(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.arg_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        "set_loc" => {
            let v = state.stack.read_top(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        "set_arg" => {
            let v = state.stack.read_top(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.arg_base, loc_offset_u16(insn));
            Step::Continue(sp)
        }
        // Local `++`/`--`: the helper updates the slot in place, so an exception
        // exit leaves `sp` untouched (matching the interpreter).
        "inc_loc" | "dec_loc" => {
            let id = if name == "inc_loc" {
                super::abi::HELPER_INC_LOCAL
            } else {
                super::abi::HELPER_DEC_LOCAL
            };
            // Epic F2: F1 proved this site int-monomorphic, so speculate an
            // inline int increment. A non-int local or overflow hands the op
            // back to the helper (still in native code).
            if state.int_only(pc, false) {
                state.note_specialized();
                return lower_incdec_loc_int(
                    state,
                    b,
                    fv,
                    sp_var,
                    helper_sig,
                    sp,
                    insn,
                    name == "inc_loc",
                    id,
                );
            }
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "goto" => {
            // The successor takes over what it carries and re-reads the rest
            // (and a back edge collects first, materializing everything).
            let target = insn.branch_target().expect("goto target") as u32;
            let dest = state.block_for(b, target);
            b.def_var(sp_var, sp);
            if target <= pc {
                // Back edge: deliver interrupts / run a deferred collection.
                let cont = safepoint(state, b, fv, helper_sig, sp, pc + size);
                b.switch_to_block(cont);
            }
            let args = state
                .edge_args(b, sp, &[(dest, target)])
                .pop()
                .expect("one target");
            b.ins().jump(dest, &args);
            Step::Stop
        }
        // `catch` pushes the handler sentinel; the interpreter's catch scan
        // reads it after a native exception exit.
        "catch" => {
            let handler = insn.branch_target().expect("catch target") as u32;
            let sentinel = (5i64 << 48) | i64::from(handler);
            let v = b.ins().iconst(types::I64, sentinel);
            Step::Continue(state.stack.push(b, sp, v))
        }
        // `gosub` pushes the return address and jumps to the `finally` body.
        "gosub" => {
            let target = insn.branch_target().expect("gosub target") as u32;
            let ret = b.ins().iconst(types::I64, i64::from(pc + size));
            let sp = state.stack.push(b, sp, ret);
            // The finally body (and its unwinder) reads the return address out
            // of the frame, so it cannot stay in a register across the jump.
            state.stack.flush(b, sp);
            let dest = state.block_for(b, target);
            let args = state
                .edge_args(b, sp, &[(dest, target)])
                .pop()
                .expect("one target");
            b.def_var(sp_var, sp);
            b.ins().jump(dest, &args);
            Step::Stop
        }
        // `ret` returns from a `finally` body to the `gosub` site. The return
        // address is dynamic; the CFG knows the possible return points, so hop
        // through a small dispatch (an unmatched address resumes to the
        // interpreter, which validates and reports it).
        "ret" => {
            let (sp, target) = state.stack.pop(b, sp);
            // Dispatch targets are separate blocks; the arguments are computed
            // here, where they dominate every hit, after a flush that leaves
            // the frame authoritative for them.
            state.stack.flush(b, sp);
            let t = b.ins().ireduce(types::I32, target);
            let dests: Vec<Block> = ret_targets
                .iter()
                .map(|&return_pc| state.block_for(b, return_pc))
                .collect();
            let targets: Vec<(Block, u32)> = dests
                .iter()
                .copied()
                .zip(ret_targets.iter().copied())
                .collect();
            let all_args = state.edge_args(b, sp, &targets);
            for index in 0..ret_targets.len() {
                let dest = dests[index];
                let hit = b.create_block();
                let miss = b.create_block();
                let matches = b
                    .ins()
                    .icmp_imm(IntCC::Equal, t, i64::from(ret_targets[index]));
                b.ins().brif(matches, hit, &[], miss, &[]);
                b.switch_to_block(hit);
                b.def_var(sp_var, sp);
                b.ins().jump(dest, &all_args[index]);
                b.switch_to_block(miss);
            }
            exit(state, b, fv, EXIT_RESUME, pc, sp);
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
            let (sp, v) = state.stack.pop(b, sp);
            // The frame is torn down after this exit; materialize anyway so no
            // register-resident slot can be observed half-written.
            state.stack.flush(b, sp);
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
            state.stack.flush(b, sp);
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
            exit(state, b, fv, EXIT_RESUME, pc, sp);
            Step::Stop
        }
    }
}

fn loc_offset_u16(insn: &Instruction) -> i32 {
    i32::from(insn.operand_u16(1)) * 8
}

/// Inline-cache index of a `get_field`/`put_field` instruction. Without the
/// `inline_cache` feature the opcode carries only the atom, so there is no
/// cache to index.
#[cfg(feature = "inline_cache")]
fn ic_index(insn: &Instruction) -> u32 {
    u32::from(insn.operand_u16(5))
}
#[cfg(not(feature = "inline_cache"))]
fn ic_index(_insn: &Instruction) -> u32 {
    0
}

/// Box a 32-bit bit-operation result exactly like `JSValue::from(u32)`: an `Int`
/// when it fits in `i32`, otherwise a `Float64`. (The interpreter really does
/// return `4294967295` for `-1 | 0`; the baseline matches it bit-for-bit.)
fn box_bit_result(b: &mut FunctionBuilder, result: Value) -> Value {
    let nonneg = b.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, result, 0);
    let as_int = b.ins().uextend(types::I64, result);
    let as_float = b.ins().fcvt_from_uint(types::F64, result);
    let bits = b.ins().bitcast(types::I64, MemFlags::new(), as_float);
    let addend = b.ins().iconst(types::I64, FLOAT64_ADDEND);
    let as_float = b.ins().isub(bits, addend);
    b.ins().select(nonneg, as_int, as_float)
}

/// Two-int fast path for a binary operator, with the interpreter helper as the
/// slow path (mixed types, overflow, string `+`, …). Operands stay in the frame
/// buffer the whole time, so the helper sees exactly what the interpreter would.
#[allow(clippy::too_many_arguments)]
fn lower_fast_binary(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
    op: FastOp,
    helper_id: usize,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    // Operand slots: `(top-1)` is the left operand, `top` the right one. Both
    // may live only in registers; the slow path flushes them before the helper.
    let top = state.stack.height;
    let a = state.stack.read(b, sp, top - 1);
    let c = state.stack.read(b, sp, top);
    let ta = b.ins().ushr_imm(a, 48);
    let tc = b.ins().ushr_imm(c, 48);
    let int_a = b.ins().icmp_imm(IntCC::Equal, ta, 0);
    let int_c = b.ins().icmp_imm(IntCC::Equal, tc, 0);
    let both_int = b.ins().band(int_a, int_c);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // Both paths must agree about memory. Everything except the two operands
    // materializes here, where the flush dominates the split; the operands are
    // dead on the fast path (the result overwrites one, the other lies above
    // the new `sp`) and are flushed by the helper's own preamble on the slow
    // path, which is the only path that still reads them.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    b.ins().brif(both_int, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    let ai = b.ins().ireduce(types::I32, a);
    let ci = b.ins().ireduce(types::I32, c);
    match op {
        FastOp::Add | FastOp::Sub | FastOp::Mul => {
            let (result, overflow) = match op {
                FastOp::Add => b.ins().sadd_overflow(ai, ci),
                FastOp::Sub => b.ins().ssub_overflow(ai, ci),
                _ => b.ins().smul_overflow(ai, ci),
            };
            // `0 * -1` / `-1 * 0` are -0, which an Int box cannot hold: bail to
            // the helper, which owns the Int-vs-Float64 sign rules.
            let bail = match op {
                FastOp::Mul => {
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
                }
                _ => overflow,
            };
            // Overflow escapes the i32 domain: the helper produces a Float64.
            let ok = b.create_block();
            b.ins().brif(bail, slow, &[], ok, &[]);
            b.switch_to_block(ok);
            let boxed = b.ins().uextend(types::I64, result);
            b.ins().store(MemFlags::new(), boxed, sp, -16);
            let sp_next = b.ins().iadd_imm(sp, -8);
            b.def_var(sp_var, sp_next);
            b.ins().jump(cont, &[]);
        }
        FastOp::Lt
        | FastOp::Lte
        | FastOp::Gt
        | FastOp::Gte
        | FastOp::Eq
        | FastOp::Neq
        | FastOp::StrictEq
        | FastOp::StrictNeq => {
            let cond = match op {
                FastOp::Lt => b.ins().icmp(IntCC::SignedLessThan, ai, ci),
                FastOp::Lte => b.ins().icmp(IntCC::SignedLessThanOrEqual, ai, ci),
                FastOp::Gt => b.ins().icmp(IntCC::SignedGreaterThan, ai, ci),
                FastOp::Gte => b.ins().icmp(IntCC::SignedGreaterThanOrEqual, ai, ci),
                FastOp::Eq | FastOp::StrictEq => b.ins().icmp(IntCC::Equal, ai, ci),
                _ => b.ins().icmp(IntCC::NotEqual, ai, ci),
            };
            let widened = b.ins().uextend(types::I64, cond);
            let boxed = b.ins().bor_imm(widened, BOOL_TAG);
            b.ins().store(MemFlags::new(), boxed, sp, -16);
            let sp_next = b.ins().iadd_imm(sp, -8);
            b.def_var(sp_var, sp_next);
            b.ins().jump(cont, &[]);
        }
        _ => {
            // ToInt32 of an int is itself; shifts use the low 5 bits of the
            // count, exactly like `WrappingOp::shl/shr/sar`.
            let count = b.ins().band_imm(ci, 31);
            let result = match op {
                FastOp::And => b.ins().band(ai, ci),
                FastOp::Or => b.ins().bor(ai, ci),
                FastOp::Xor => b.ins().bxor(ai, ci),
                FastOp::Shl => b.ins().ishl(ai, count),
                FastOp::Shr => b.ins().ushr(ai, count),
                _ => b.ins().sshr(ai, count),
            };
            // `shl`/`sar` are `From<i32>` (always an Int); `and`/`or`/`xor` and
            // `shr` are `From<u32>` — Int, or a Float64 once the sign bit is
            // set. Match the interpreter exactly, quirk included.
            let boxed = match op {
                FastOp::Shl | FastOp::Sar => b.ins().uextend(types::I64, result),
                _ => box_bit_result(b, result),
            };
            b.ins().store(MemFlags::new(), boxed, sp, -16);
            let sp_next = b.ins().iadd_imm(sp, -8);
            b.def_var(sp_var, sp_next);
            b.ins().jump(cont, &[]);
        }
    }

    b.switch_to_block(slow);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper_id, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
    // Both paths store the result into slot `top-1`; the operand entries are
    // now stale (a flush would overwrite that result), so drop them.
    state.stack.consumed_binary();
    Step::Continue(b.use_var(sp_var))
}

/// Epic F2: speculative inline `int` fast path for `local += operand`, emitted
/// only when F1 feedback proved the site int-monomorphic.
///
/// A non-int operand or an `i32` overflow falls back to the `add_loc` helper —
/// which is the interpreter's own `arith_add` operating on the same frame — so
/// the result stays bit-identical **without leaving native code**. That is the
/// architectural difference from quickjs-jit, whose optimized tier must exit to
/// the interpreter on a guard miss because its helpers cannot read the frame;
/// zhuque's helpers *are* the interpreter, so the "deopt" here is a helper call.
fn lower_add_loc_int(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let idx = insn.operand_u8(1) as i32;
    let byte_off = idx * 8;

    let local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    let top = state.stack.height;
    let operand = state.stack.read_top(b, sp);
    let tag_local = b.ins().ushr_imm(local, 48);
    let tag_operand = b.ins().ushr_imm(operand, 48);
    let int_local = b.ins().icmp_imm(IntCC::Equal, tag_local, 0);
    let int_operand = b.ins().icmp_imm(IntCC::Equal, tag_operand, 0);
    let both_int = b.ins().band(int_local, int_operand);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // Like `lower_fast_binary`: materialize everything but the operand, which
    // only the slow path's helper still needs in memory.
    state.stack.flush_except(b, sp, &[top]);
    b.ins().brif(both_int, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    let li = b.ins().ireduce(types::I32, local);
    let oi = b.ins().ireduce(types::I32, operand);
    let (sum, overflow) = b.ins().sadd_overflow(li, oi);
    // Overflow escapes the i32 domain; the helper boxes it as a Float64.
    let ok = b.create_block();
    b.ins().brif(overflow, slow, &[], ok, &[]);
    b.switch_to_block(ok);
    let boxed = b.ins().uextend(types::I64, sum);
    b.ins().store(MemFlags::new(), boxed, fv.var_base, byte_off);
    let sp_next = b.ins().iadd_imm(sp, -8);
    b.def_var(sp_var, sp_next);
    b.ins().jump(cont, &[]);

    b.switch_to_block(slow);
    // Leave the operand on the stack and set `imm`; the helper pops it and
    // re-derives the full `arith_add` semantics (string concat, ToPrimitive, …).
    let imm = b.ins().iconst(types::I32, i64::from(idx));
    b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_ADD_LOC, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
    // Pop2/push1 would be wrong here: this op only pops the operand — the
    // result went into the *local* (fast path stores it, helper writes it).
    state.stack.drop_top();
    Step::Continue(b.use_var(sp_var))
}

/// Epic F2: speculative inline `int` fast path for `local++`/`local--`, emitted
/// only when F1 feedback proved the site int-monomorphic. The stack is
/// untouched; a non-int local or an `i32` overflow falls back to the
/// `inc_loc`/`dec_loc` helper (see [`lower_add_loc_int`] for why that is a
/// helper call rather than an interpreter exit).
fn lower_incdec_loc_int(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
    is_inc: bool,
    helper_id: usize,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let idx = insn.operand_u8(1) as i32;
    let byte_off = idx * 8;

    let local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    let tag = b.ins().ushr_imm(local, 48);
    let is_int = b.ins().icmp_imm(IntCC::Equal, tag, 0);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // No stack operand here, but the model still has to match both paths:
    // materialize before the split, where the stores dominate it.
    state.stack.flush(b, sp);
    b.ins().brif(is_int, fast, &[], slow, &[]);

    b.switch_to_block(fast);
    let li = b.ins().ireduce(types::I32, local);
    let one = b.ins().iconst(types::I32, 1);
    let (res, overflow) = if is_inc {
        b.ins().sadd_overflow(li, one)
    } else {
        b.ins().ssub_overflow(li, one)
    };
    // `i32::MAX`/`MIN` promote to a Float64 in the interpreter; the helper
    // owns that boundary, so hand the op back rather than duplicating it.
    let ok = b.create_block();
    b.ins().brif(overflow, slow, &[], ok, &[]);
    b.switch_to_block(ok);
    let boxed = b.ins().uextend(types::I64, res);
    b.ins().store(MemFlags::new(), boxed, fv.var_base, byte_off);
    b.def_var(sp_var, sp);
    b.ins().jump(cont, &[]);

    b.switch_to_block(slow);
    let imm = b.ins().iconst(types::I32, i64::from(idx));
    b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper_id, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
    Step::Continue(b.use_var(sp_var))
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
    exit(state, b, fv, 1, pc_after, sp);
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
    // The shared flush before the branch left the model empty, so the carried
    // slots of both targets arrive as frame loads on this arm.
    let mut args = state.edge_args(b, sp, &[(target_block, target), (fall_block, fallthrough)]);
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
        compile(&verified, isa.as_ref(), &Cell::new(0)).map(|_| ())
    }

    /// Like `compile_bytes`, but with `gv` global-variable references so
    /// `get_var`/`put_var` index checks pass.
    fn compile_bytes_gv(bytes: Vec<u8>, args: u16, vars: u16, gv: u16) -> Result<(), JitError> {
        let isa = crate::jit::build_host_isa().unwrap();
        let snap = CompileSnapshot::from_raw(bytes, args, vars, gv, 16);
        let verified = verify(snap, VerifyLimits::default())
            .unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
        compile(&verified, isa.as_ref(), &Cell::new(0)).map(|_| ())
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

    /// `get_field`/`put_field` encoding: `[op, atom:u32]`, plus `[ic_idx:u16]`
    /// when the inline cache is compiled in.
    fn field(opcode: u8, ic: u16) -> Vec<u8> {
        let mut out = vec![opcode];
        out.extend_from_slice(&u32b(0));
        #[cfg(feature = "inline_cache")]
        out.extend_from_slice(&u16b(ic));
        let _ = ic;
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

        // Epic E3: array-element helpers. Stack effects differ per opcode.
        cases.push((
            "get_array_el",
            seq(&[&push_i32(0), &push_i32(0), &[op::OP_get_array_el], &ret]),
            0,
            0,
        ));
        cases.push((
            "get_array_el2",
            seq(&[&push_i32(0), &push_i32(0), &[op::OP_get_array_el2], &ret]),
            0,
            0,
        ));
        cases.push((
            "get_array_el3",
            seq(&[&push_i32(0), &push_i32(0), &[op::OP_get_array_el3], &ret]),
            0,
            0,
        ));
        cases.push((
            "put_array_el",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &push_i32(0),
                &[op::OP_put_array_el, op::OP_return_undef],
            ]),
            0,
            0,
        ));

        // Epic E2: property helpers (`get_field`/`get_field2`/`put_field`).
        // Stack effects differ; the operand is atom + (inline_cache) ic index.
        let get_field = field(op::OP_get_field, 0);
        let get_field2 = field(op::OP_get_field2, 0);
        let put_field = field(op::OP_put_field, 0);
        cases.push(("get_field", seq(&[&push_i32(0), &get_field, &ret]), 0, 0));
        cases.push(("get_field2", seq(&[&push_i32(0), &get_field2, &ret]), 0, 0));
        cases.push((
            "put_field",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &put_field,
                &[op::OP_return_undef],
            ]),
            0,
            0,
        ));

        // Completing E1: the remaining stack ops.
        cases.push((
            "pow",
            seq(&[&push_i32(2), &push_i32(3), &[op::OP_pow], &ret]),
            0,
            0,
        ));
        for (name, code) in [
            ("to_object", op::OP_to_object),
            ("to_propkey", op::OP_to_propkey),
        ] {
            cases.push((name, seq(&[&push_i32(1), &[code], &ret]), 0, 0));
        }
        for (name, code) in [
            ("in", op::OP_in),
            ("instanceof", op::OP_instanceof),
            ("delete", op::OP_delete),
        ] {
            cases.push((
                name,
                seq(&[&push_i32(0), &push_i32(0), &[code], &ret]),
                0,
                0,
            ));
        }
        cases.push((
            "post_inc",
            seq(&[&push_i32(1), &[op::OP_post_inc], &ret]),
            0,
            0,
        ));
        cases.push((
            "post_dec",
            seq(&[&push_i32(1), &[op::OP_post_dec], &ret]),
            0,
            0,
        ));
        cases.push((
            "add_loc",
            seq(&[
                &push_i32(0),
                &loc(op::OP_put_loc, 0),
                &push_i32(0),
                &loc8(op::OP_add_loc, 0),
                &[op::OP_return_undef],
            ]),
            0,
            1,
        ));

        // Epic E5: exception regions. `catch` pushes the handler sentinel;
        // `gosub`/`ret` are the `finally` subroutine.
        let gosub_ret = seq(&[
            &branch(op::OP_gosub, 5),
            &[op::OP_return_undef],
            &[op::OP_ret],
        ]);
        cases.push(("gosub_ret", gosub_ret, 0, 0));
        let catch_body = seq(&[
            &branch(op::OP_catch, 5),
            &[op::OP_return_undef],
            &[op::OP_return_undef],
        ]);
        cases.push(("catch", catch_body, 0, 0));

        for (name, bytes, args, vars) in cases {
            compile_bytes(bytes, args, vars)
                .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }

        // Epic E3: global-variable helpers need a non-zero global-var-ref count,
        // so they use their own snapshot shape.
        let globals: &[(&str, u8)] = &[
            ("get_var", op::OP_get_var),
            ("get_var_undef", op::OP_get_var_undef),
        ];
        for (name, code) in globals {
            compile_bytes_gv(seq(&[&loc(*code, 0), &ret]), 0, 0, 1)
                .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }
        for (name, code) in [
            ("put_var", op::OP_put_var),
            ("put_var_init", op::OP_put_var_init),
        ] {
            compile_bytes_gv(
                seq(&[&push_i32(0), &loc(code, 0), &[op::OP_return_undef]]),
                0,
                0,
                1,
            )
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
        assert!(compile(&verified, isa.as_ref(), &Cell::new(0)).is_ok());
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

    // ── Epic F4: the block-local operand-stack cache (unit level) ──

    /// Emit one block through `f` and hand back the printed IR, so a test can
    /// assert on what the cache emitted — or deliberately did not emit.
    fn ir_of(f: impl FnOnce(&mut FunctionBuilder)) -> String {
        let mut ctx = Context::new();
        let mut fbctx = FunctionBuilderContext::new();
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbctx);
        let entry = b.create_block();
        b.switch_to_block(entry);
        f(&mut b);
        // Terminate the block so `finalize` accepts it; the terminator is
        // irrelevant to what these tests count.
        b.ins().return_(&[]);
        b.seal_all_blocks();
        b.finalize(); // consumes the builder, ending its borrow of `ctx.func`
        format!("{}", ctx.func.display())
    }

    fn count(haystack: &str, needle: &str) -> usize {
        haystack.matches(needle).count()
    }

    /// The `store` lines of printed IR, in emission order.
    fn stores(ir: &str) -> Vec<&str> {
        ir.lines()
            .map(str::trim_start)
            .filter(|line| line.starts_with("store"))
            .collect()
    }

    /// A value pushed and popped again never reaches memory: both sides hold
    /// the same SSA value, so the store/load pair is never emitted (DCE), and
    /// a later flush has nothing left to write.
    #[test]
    fn stack_cache_pop_dces_the_push_store() {
        let ir = ir_of(|b| {
            let sp = b.ins().iconst(types::I64, 4096);
            let v = b.ins().iconst(types::I64, 42);
            let mut cache = StackCache::default();
            cache.anchor(0);
            let sp = cache.push(b, sp, v);
            let (sp, popped) = cache.pop(b, sp);
            assert_eq!(
                popped, v,
                "the register-resident value comes back unchanged"
            );
            assert_eq!(cache.height, 0, "pop accounts for the slot");
            assert!(cache.is_empty(), "the popped slot died with its value");
            cache.flush(b, sp);
        });
        assert_eq!(
            stores(&ir).len(),
            0,
            "push→pop must not touch memory:\n{ir}"
        );
    }

    /// A pop with no cached value falls back to a load — the counter-case to
    /// the DCE above (this is what happens after a flush cleared the cache).
    #[test]
    fn stack_cache_pop_loads_when_the_slot_is_in_memory() {
        let ir = ir_of(|b| {
            let sp = b.ins().iconst(types::I64, 4096);
            let mut cache = StackCache::default();
            cache.anchor(1); // one slot already on the stack, never cached
            let (sp, loaded) = cache.pop(b, sp);
            assert_ne!(loaded, sp, "a fresh Value, not the stack pointer");
            assert!(cache.is_empty());
            let _ = sp;
        });
        assert_eq!(
            count(&ir, "load"),
            1,
            "the missing slot must be loaded:\n{ir}"
        );
    }

    /// Flush materializes every register-resident slot at the offset its
    /// verified height implies: slot k sits at `sp + (k - 1 - height) * 8`,
    /// so at height 2 that is `sp-16` and `sp-8`.
    #[test]
    fn stack_cache_flush_writes_each_dirty_slot_at_its_offset() {
        let ir = ir_of(|b| {
            let sp = b.ins().iconst(types::I64, 4096);
            let mut cache = StackCache::default();
            cache.anchor(0);
            let a = b.ins().iconst(types::I64, 1);
            let sp = cache.push(b, sp, a);
            let c = b.ins().iconst(types::I64, 2);
            let sp = cache.push(b, sp, c);
            cache.flush(b, sp);
            assert!(cache.is_empty(), "flush forgets what it wrote");
        });
        let emitted = stores(&ir);
        assert_eq!(emitted.len(), 2, "one store per dirty slot:\n{ir}");
        assert!(
            emitted[0].contains("-16"),
            "slot 1 at height 2 sits at sp-16:\n{ir}"
        );
        assert!(emitted[1].contains("-8"), "slot 2 sits at sp-8:\n{ir}");
        // The offsets ride on the memory instruction itself: no address math.
        assert_eq!(
            count(&ir, "iadd_imm"),
            2,
            "only the two pushes adjust sp:\n{ir}"
        );
    }

    /// `flush_except` writes everything *except* the slots a sibling branch
    /// still needs in registers; those survive for a later flush, and the
    /// stack itself never moves.
    #[test]
    fn stack_cache_flush_except_keeps_the_named_slots_dirty() {
        let ir = ir_of(|b| {
            let sp = b.ins().iconst(types::I64, 4096);
            let mut cache = StackCache::default();
            cache.anchor(0);
            let a = b.ins().iconst(types::I64, 1);
            let sp = cache.push(b, sp, a);
            let c = b.ins().iconst(types::I64, 2);
            let sp = cache.push(b, sp, c);

            cache.flush_except(b, sp, &[2]);
            let mid = format!("{}", b.func.display());
            assert_eq!(
                stores(&mid).len(),
                1,
                "only the unkept slot is written:\n{mid}"
            );
            assert_eq!(cache.height, 2, "a flush never moves the stack");
            assert!(!cache.is_empty(), "the kept slot stays register-resident");

            cache.flush(b, sp);
            assert!(cache.is_empty());
        });
        assert_eq!(
            stores(&ir).len(),
            2,
            "the kept slot is written by the second flush:\n{ir}"
        );
    }

    /// Height bookkeeping is what makes every slot address correct: each
    /// instruction re-anchors to the verifier's height, and only the effects
    /// the lowering itself models (`tracked`) are worth the cross-check.
    #[test]
    fn stack_cache_height_follows_the_verifier_contract() {
        ir_of(|b| {
            let sp = b.ins().iconst(types::I64, 4096);
            let mut cache = StackCache::default();

            cache.anchor(3);
            assert_eq!(cache.height, 3);
            assert!(!cache.tracked, "an anchored instruction starts untracked");

            let v = b.ins().iconst(types::I64, 7);
            let sp = cache.push(b, sp, v);
            assert_eq!(cache.height, 4);
            assert!(cache.tracked, "push is a tracked effect");

            let sp = cache.pop(b, sp).0;
            assert_eq!(cache.height, 3);
            cache.drop_top();
            assert_eq!(cache.height, 2, "drop discards a slot without reading it");

            // A binary op pops two and pushes one, and caches neither operand:
            // two base slots + two pushes − one net = three slots left.
            cache.anchor(2);
            let x = b.ins().iconst(types::I64, 1);
            let sp = cache.push(b, sp, x);
            let y = b.ins().iconst(types::I64, 2);
            let sp = cache.push(b, sp, y);
            cache.consumed_binary();
            assert_eq!(cache.height, 3, "pop two, push one");
            assert!(
                cache.is_empty(),
                "both operands are dead, result is in memory"
            );

            cache.anchor(2);
            cache.adjust(-1);
            assert_eq!(cache.height, 1, "a shuffle's raw effect is applied");

            cache.anchor(9);
            assert_eq!(cache.height, 9, "the next instruction re-anchors");
            assert!(!cache.tracked, "and clears the mark");
            let _ = sp;
        });
    }

    // ── Epic F4 step 2: cross-block carry (unit level) ──

    /// Verify a hand-built function and run the carry walk over it.
    fn carried_of(
        bytes: Vec<u8>,
        args: u16,
        vars: u16,
    ) -> (
        std::collections::HashMap<u32, Vec<u32>>,
        std::collections::HashMap<u32, u16>,
    ) {
        let verified = verify(snapshot(bytes, args, vars), VerifyLimits::default())
            .unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
        let osr_pcs = collect_osr_pcs(&verified);
        let strict = verified.snapshot().mode.contains(ExecMode::STRICT);
        carried_sets(&verified, &osr_pcs, strict)
    }

    /// A join block whose first instruction reads the merged slot in a register
    /// (`put_loc`) carries it: one store/load pair per edge disappears.
    /// Bytecode: `get_loc0; dup; if_false L; drop; get_loc1; L: put_loc0;
    /// get_loc0; get_loc1; add; put_loc0; return_undef`.
    #[test]
    fn carried_sets_marks_a_slot_read_before_the_first_flush() {
        let bytes = seq(&[
            &loc(op::OP_get_loc, 0),
            &[op::OP_dup],
            &branch(op::OP_if_false, 8), // pc 4 → 4+1+8 = 13
            &[op::OP_drop],
            &loc(op::OP_get_loc, 1),
            &loc(op::OP_put_loc, 0), // L = 13, entry height 1
            &loc(op::OP_get_loc, 0),
            &loc(op::OP_get_loc, 1),
            &[op::OP_add],
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]);
        let (carried, heights) = carried_of(bytes.clone(), 0, 2);
        assert_eq!(
            carried.get(&13).map(Vec::as_slice),
            Some([1].as_slice()),
            "L reads slot 1 with `put_loc` before any flush"
        );
        // The fallthrough block only drops and pushes — nothing to carry.
        assert!(carried.get(&9).is_none(), "drop/push read no entry slot");
        assert_eq!(heights.get(&13), Some(&1), "both edges merge at height 1");
        // And the plumbing lowers: parameters on the join, args on both edges.
        compile_bytes(bytes, 0, 2).expect("join with carried parameters compiles");
    }

    /// A block that starts with a helper call flushes before it reads anything,
    /// so carrying into it would buy nothing (and cost a reload on the edge):
    /// the walk stops at the first instruction.
    #[test]
    fn carried_sets_stops_at_the_first_flush() {
        let bytes = seq(&[
            &loc(op::OP_get_loc, 0),
            &[op::OP_dup],
            &branch(op::OP_if_false, 8), // pc 4 → 13
            &[op::OP_drop],
            &loc(op::OP_get_loc, 1),
            &[op::OP_plus], // L = 13: a helper call, entry height 1
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]);
        let (carried, _) = carried_of(bytes, 0, 2);
        assert!(
            carried.get(&13).is_none(),
            "the helper's frame flush ends the walk: {carried:?}"
        );
    }

    /// The two-int fast path reads its operands from registers before it splits
    /// (its `flush_except` spares exactly them), so a block entered with both
    /// slots on the stack carries them:
    /// `push1; push2; get_loc0; if_false L; push9; drop; goto L;
    /// L: add; put_loc0; return_undef`.
    #[test]
    fn carried_sets_marks_both_fast_path_operands() {
        let bytes = seq(&[
            &push_i32(1),
            &push_i32(2),
            &loc(op::OP_get_loc, 0),
            &branch(op::OP_if_false, 15), // pc 13 → 14+15 = 29
            &push_i32(9),
            &[op::OP_drop],
            &branch(op::OP_goto, 4), // pc 24 → 25+4 = 29
            &[op::OP_add],           // L = 29, entry height 2
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]);
        let (carried, heights) = carried_of(bytes, 0, 1);
        assert_eq!(
            carried.get(&29).map(Vec::as_slice),
            Some([1, 2].as_slice()),
            "the fast path reads both entry slots before flushing"
        );
        assert!(carried.get(&18).is_none(), "the middle block only shuffles");
        assert_eq!(heights.get(&29), Some(&2));
    }

    /// The verifier merges operand kinds *exactly* at a join: a short-circuit
    /// label whose arms disagree — a loop-widened `Tagged` local against an
    /// `Int32` literal (`x || 5`) — rejects the whole function, fail-closed.
    /// Unrelated to epic F4 itself; it is why the F4 carry cases in the test
    /// suites cross every join with same-kind operands (locals on both sides).
    #[test]
    fn join_of_tagged_and_int32_operand_is_rejected() {
        // Layout: get_loc0(0..3) dup(3..4) if_true(4..9) drop(9..10)
        // push5(10..15) return_undef(15..16); branch offset 15-5 = 10.
        let bytes = seq(&[
            &loc(op::OP_get_loc, 0),
            &[op::OP_dup],
            &branch(op::OP_if_true, 10),
            &[op::OP_drop],
            &push_i32(5),
            &[op::OP_return_undef],
        ]);
        let err = verify(snapshot(bytes, 0, 1), VerifyLimits::default())
            .expect_err("Tagged vs Int32 at the label must not verify");
        assert!(
            matches!(
                err.kind(),
                crate::jit::bytecode::verify::VerifyErrorKind::IncompatibleMergeKind { .. }
            ),
            "expected the exact-kind merge to fail, got {err:?}"
        );
    }
}
