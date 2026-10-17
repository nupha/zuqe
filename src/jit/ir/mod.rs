// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The minimal function-scoped optimization IR (decision D-13).
//!
//! The baseline lowers [`VerifiedFunction`] straight to Cranelift. That is
//! enough for a faithful translation, but the remaining F4 work — cross-block
//! local residency, dead-store elimination, loop-level optimization — needs a
//! *data-flow* view the bytecode does not hand over directly: which block reads
//! or writes which frame slot, which block dominates which, which blocks form a
//! loop, and which slots are live across a block boundary.
//!
//! This module builds exactly that from the verified bytecode — no separate
//! instruction representation, no SSA remapping, no deopt map. The value model
//! stays the frame (D-25): a slot is "live" in the ordinary compiler sense
//! (read before the next write on some path), and the passes that consume the
//! IR answer questions the lowering asks at the point it needs them:
//!
//! * [`Ir::block_index`] / [`Ir::live_out`] — DCE of a local store (`1.2`).
//! * [`Ir::blocks`] `uses` — which locals a block reads before writing, so the
//!   edge machinery can carry them in a register (`1.1`).
//! * [`Ir::loops`] — natural loops, for the loop-level passes still to come
//!   (`1.3` BCE / loop-invariant loads).
//!
//! Liveness is exact only when the function neither raises/catches in-frame nor
//! contains an opcode the baseline would `Resume` at; otherwise a resume can
//! read *every* frame slot, so [`Ir::exact`] is false and consumers fall back
//! to conservative behavior. That gate lives in the caller (it is the same
//! `supported()` / `has_exception_regions()` question the lowering already
//! asks), so this module never has to guess.

use std::{collections::HashMap, ops::Range};

use crate::bytecode::OpcodeFormat;

use super::bytecode::{
    cfg::ControlFlowGraph, decode::Instruction, stack::local_index, verify::VerifiedFunction,
};

mod values;

pub(crate) use values::RedundantExpressions;

/// A frame slot the IR reasons about: a local (excluding arguments) or an
/// argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LocalRef {
    pub(crate) is_arg: bool,
    pub(crate) idx: u32,
}

impl LocalRef {
    pub(crate) const fn local(idx: u32) -> Self {
        Self { is_arg: false, idx }
    }

    pub(crate) const fn arg(idx: u32) -> Self {
        Self { is_arg: true, idx }
    }
}

/// One basic block of the IR: the CFG block's instruction range plus its
/// def/use summary and predecessor/successor indices.
#[derive(Clone, Debug)]
pub(crate) struct BlockIr {
    pub(crate) start_pc: u32,
    pub(crate) range: Range<usize>,
    pub(crate) succ: Vec<usize>,
    pub(crate) pred: Vec<usize>,
    /// Slots read before being written anywhere in this block.
    pub(crate) uses: Vec<LocalRef>,
    /// Slots written anywhere in this block.
    pub(crate) defs: Vec<LocalRef>,
}

/// A natural loop: its header block and the blocks that reach a back edge to
/// the header.
#[derive(Clone, Debug)]
pub(crate) struct LoopIr {
    pub(crate) header: usize,
    pub(crate) members: Vec<usize>,
}

/// The function-scoped IR. All block indices are positions in [`Ir::blocks`].
#[derive(Clone, Debug)]
pub(crate) struct Ir {
    blocks: Vec<BlockIr>,
    block_of_pc: HashMap<u32, usize>,
    /// Immediate dominators, indexed like `blocks` (the entry is its own).
    idom: Vec<Option<usize>>,
    loops: Vec<LoopIr>,
    live_in: Vec<Vec<LocalRef>>,
    live_out: Vec<Vec<LocalRef>>,
    /// Whether [`Self::live_in`] / [`Self::live_out`] are exact (see module
    /// docs). Consumers that skip a write must check this.
    exact: bool,
}

impl Ir {
    /// Build the IR. `resume_free` is the caller's answer to "does this
    /// function contain an opcode the baseline would `Resume` at?" — the only
    /// fact the IR cannot derive from the verified bytecode alone.
    pub(crate) fn build(verified: &VerifiedFunction, resume_free: bool) -> Self {
        let instructions = verified.instructions();
        let cfg = verified.control_flow_graph();

        let mut blocks: Vec<BlockIr> = Vec::with_capacity(cfg.blocks().len());
        let mut block_of_pc = HashMap::new();
        for block in cfg.blocks() {
            let index = blocks.len();
            block_of_pc.insert(block.start_pc(), index);
            let range = block.instruction_range();
            let (uses, defs) = def_use(instructions, range.clone());
            blocks.push(BlockIr {
                start_pc: block.start_pc(),
                range,
                succ: Vec::new(),
                pred: Vec::new(),
                uses,
                defs,
            });
        }
        for (index, block) in cfg.blocks().iter().enumerate() {
            for &succ_pc in block.successors() {
                if let Some(&succ) = block_of_pc.get(&succ_pc) {
                    if !blocks[index].succ.contains(&succ) {
                        blocks[index].succ.push(succ);
                        blocks[succ].pred.push(index);
                    }
                }
            }
        }

        let idom = dominators(&blocks, &block_of_pc);
        let loops = natural_loops(&blocks, &idom);
        let exact = resume_free && !verified.has_exception_regions();
        let (live_in, live_out) = liveness(&blocks);

        Self {
            blocks,
            block_of_pc,
            idom,
            loops,
            live_in,
            live_out,
            exact,
        }
    }

    pub(crate) fn blocks(&self) -> &[BlockIr] {
        &self.blocks
    }

    pub(crate) fn block_index(&self, start_pc: u32) -> Option<usize> {
        self.block_of_pc.get(&start_pc).copied()
    }

    /// Whether liveness is exact (see module docs). False when the function can
    /// raise/catch in-frame or resume to the interpreter, in which case a
    /// "dead" store can still be read by the interpreter.
    pub(crate) const fn exact(&self) -> bool {
        self.exact
    }

    pub(crate) fn live_out(&self, block: usize) -> &[LocalRef] {
        &self.live_out[block]
    }

    pub(crate) fn live_in(&self, block: usize) -> &[LocalRef] {
        &self.live_in[block]
    }

    pub(crate) fn is_live_out(&self, block: usize, slot: LocalRef) -> bool {
        self.live_out[block].contains(&slot)
    }

    #[allow(dead_code)] // Consumed by the upcoming loop passes (1.3 BCE).
    pub(crate) fn loops(&self) -> &[LoopIr] {
        &self.loops
    }

    #[allow(dead_code)] // Consumed by the upcoming loop passes (1.3 BCE).
    pub(crate) fn immediate_dominator(&self, block: usize) -> Option<usize> {
        self.idom[block]
    }

    /// Does `a` dominate `b`? Walks the dominator tree; both must be reachable.
    #[allow(dead_code)]
    pub(crate) fn dominates(&self, a: usize, b: usize) -> bool {
        let mut node = Some(b);
        while let Some(n) = node {
            if n == a {
                return true;
            }
            if self.idom[n] == Some(n) {
                return false;
            }
            node = self.idom[n];
        }
        false
    }

    /// Epic S2 (1.2): the `put_loc`/`set_loc` pcs whose store is dead — the slot
    /// is not read again before it is redefined on any path (no later read in
    /// the block, and not live out of it). The lowering skips memory writes for
    /// these pcs. `gate` must encode "liveness is trustworthy here" (the
    /// function is resume-free and its local slots are alias-free); otherwise
    /// the result is empty.
    ///
    /// Only `put_loc`/`set_loc` are considered: the TDZ-checked writes also run
    /// a guard the interpreter owns, so dropping their store needs more than a
    /// liveness argument.
    pub(crate) fn dead_local_stores(&self, instructions: &[Instruction], gate: bool) -> Vec<u32> {
        let mut dead = Vec::new();
        if !gate || !self.exact {
            return dead;
        }
        for (block_index, block) in self.blocks.iter().enumerate() {
            for index in block.range.clone() {
                let instruction = &instructions[index];
                let name = instruction.name();
                if name != "put_loc" && name != "set_loc" {
                    continue;
                }
                let Some(slot) = frame_slot(instruction) else {
                    continue;
                };
                if self.is_live_out(block_index, slot) {
                    continue;
                }
                // A later read in the same block (or a read-modify-write that
                // reads it) keeps the value alive until then.
                let later_read = instructions[index + 1..block.range.end]
                    .iter()
                    .any(|later| {
                        reads_slot(later.name(), later.format()) && frame_slot(later) == Some(slot)
                    });
                if !later_read {
                    dead.push(instruction.pc());
                }
            }
        }
        dead
    }
}

/// The frame slot an opcode reads or writes, if any. `LOC`/`ARG` families use
/// the same encodings the verifier's index check does.
fn frame_slot(instruction: &Instruction) -> Option<LocalRef> {
    match instruction.format() {
        OpcodeFormat::LOC | OpcodeFormat::LOC8 | OpcodeFormat::NONE_LOC => {
            local_index(instruction).map(|idx| LocalRef::local(idx as u32))
        }
        OpcodeFormat::ARG => Some(LocalRef::arg(u32::from(instruction.operand_u16(1)))),
        OpcodeFormat::NONE_ARG => short_index(instruction).map(LocalRef::arg),
        _ => None,
    }
}

/// The digit index of a `NONE_LOC`/`NONE_ARG` opcode (`get_loc0` → 0).
fn short_index(instruction: &Instruction) -> Option<u32> {
    instruction
        .name()
        .as_bytes()
        .last()
        .and_then(|value| value.is_ascii_digit().then_some(u32::from(value - b'0')))
}

/// Whether an opcode reads the slot before writing it (or only reads).
fn reads_slot(name: &str, format: OpcodeFormat) -> bool {
    if !matches!(
        format,
        OpcodeFormat::LOC
            | OpcodeFormat::LOC8
            | OpcodeFormat::NONE_LOC
            | OpcodeFormat::ARG
            | OpcodeFormat::NONE_ARG
    ) {
        return false;
    }
    matches!(
        name,
        "get_loc"
            | "get_loc_check"
            | "get_loc_checkthis"
            | "get_arg"
            | "put_loc_check"
            | "put_loc_check_init"
            | "set_loc_check"
            | "inc_loc"
            | "dec_loc"
            | "add_loc"
    )
}

/// Whether an opcode writes the slot (alone or after reading it).
fn writes_slot(name: &str, format: OpcodeFormat) -> bool {
    if !matches!(
        format,
        OpcodeFormat::LOC
            | OpcodeFormat::LOC8
            | OpcodeFormat::NONE_LOC
            | OpcodeFormat::ARG
            | OpcodeFormat::NONE_ARG
    ) {
        return false;
    }
    matches!(
        name,
        "put_loc"
            | "set_loc"
            | "put_loc_check"
            | "put_loc_check_init"
            | "set_loc_check"
            | "set_loc_uninitialized"
            | "put_arg"
            | "set_arg"
            | "inc_loc"
            | "dec_loc"
            | "add_loc"
    )
}

/// Read-before-write `uses` and all `defs` of one block.
fn def_use(instructions: &[Instruction], range: Range<usize>) -> (Vec<LocalRef>, Vec<LocalRef>) {
    let mut uses: Vec<LocalRef> = Vec::new();
    let mut defs: Vec<LocalRef> = Vec::new();
    let mut written: Vec<LocalRef> = Vec::new();
    for instruction in &instructions[range] {
        let name = instruction.name();
        let format = instruction.format();
        if reads_slot(name, format) {
            if let Some(slot) = frame_slot(instruction) {
                if !written.contains(&slot) && !uses.contains(&slot) {
                    uses.push(slot);
                }
            }
        }
        if writes_slot(name, format) {
            if let Some(slot) = frame_slot(instruction) {
                if !written.contains(&slot) {
                    written.push(slot);
                }
                if !defs.contains(&slot) {
                    defs.push(slot);
                }
            }
        }
    }
    (uses, defs)
}

/// Immediate dominators (Cooper–Harvey–Kennedy iteration over reverse post
/// order). Unreachable blocks keep `None`; the entry block dominates itself.
fn dominators(blocks: &[BlockIr], block_of_pc: &HashMap<u32, usize>) -> Vec<Option<usize>> {
    let n = blocks.len();
    let mut idom: Vec<Option<usize>> = vec![None; n];
    let Some(&entry) = block_of_pc.get(&0) else {
        return idom;
    };
    idom[entry] = Some(entry);

    // Reverse post order from the entry.
    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut seen = vec![false; n];
    let mut stack = vec![(entry, 0usize)];
    seen[entry] = true;
    while let Some((node, next)) = stack.pop() {
        if next < blocks[node].succ.len() {
            stack.push((node, next + 1));
            let child = blocks[node].succ[next];
            if !seen[child] {
                seen[child] = true;
                stack.push((child, 0));
            }
        } else {
            order.push(node);
        }
    }
    order.reverse();
    let mut rpo_index = vec![usize::MAX; n];
    for (i, &node) in order.iter().enumerate() {
        rpo_index[node] = i;
    }

    let intersect = |idom: &[Option<usize>], mut a: usize, mut b: usize| -> usize {
        while a != b {
            while rpo_index[a] > rpo_index[b] {
                a = idom[a].expect("reachable node has an idom");
            }
            while rpo_index[b] > rpo_index[a] {
                b = idom[b].expect("reachable node has an idom");
            }
        }
        a
    };

    let mut changed = true;
    while changed {
        changed = false;
        for &node in &order {
            if node == entry {
                continue;
            }
            let mut new: Option<usize> = None;
            for &pred in &blocks[node].pred {
                if idom[pred].is_none() {
                    continue;
                }
                new = Some(match new {
                    None => pred,
                    Some(current) => intersect(&idom, current, pred),
                });
            }
            if new.is_some() && new != idom[node] {
                idom[node] = new;
                changed = true;
            }
        }
    }
    idom
}

/// Natural loops: for every edge into a block that dominates the source (a back
/// edge), the body is `source` plus every predecessor reachable without passing
/// the header.
fn natural_loops(blocks: &[BlockIr], idom: &[Option<usize>]) -> Vec<LoopIr> {
    let mut loops: Vec<LoopIr> = Vec::new();
    for (source, block) in blocks.iter().enumerate() {
        if idom[source].is_none() {
            continue;
        }
        for &header in &block.succ {
            if idom[header].is_none() {
                continue;
            }
            let is_back_edge = header == source || dominates(idom, header, source);
            if !is_back_edge {
                continue;
            }
            let mut members: Vec<usize> = vec![header];
            let mut stack = vec![source];
            while let Some(node) = stack.pop() {
                if members.contains(&node) {
                    continue;
                }
                members.push(node);
                for &pred in &blocks[node].pred {
                    if pred != header {
                        stack.push(pred);
                    }
                }
            }
            members.sort_unstable();
            if let Some(existing) = loops.iter_mut().find(|l| l.header == header) {
                for &member in &members {
                    if !existing.members.contains(&member) {
                        existing.members.push(member);
                    }
                }
                existing.members.sort_unstable();
            } else {
                loops.push(LoopIr { header, members });
            }
        }
    }
    loops
}

fn dominates(idom: &[Option<usize>], a: usize, b: usize) -> bool {
    let mut node = Some(b);
    while let Some(n) = node {
        if n == a {
            return true;
        }
        if idom[n] == Some(n) {
            return false;
        }
        node = idom[n];
    }
    false
}

/// Classic live-variable analysis: `live_in[b] = uses[b] ∪ (live_out[b] \ defs[b])`,
/// `live_out[b] = ∪ live_in[succ]`. Unreachable blocks are ignored.
fn liveness(blocks: &[BlockIr]) -> (Vec<Vec<LocalRef>>, Vec<Vec<LocalRef>>) {
    let n = blocks.len();
    let mut live_in: Vec<Vec<LocalRef>> = vec![Vec::new(); n];
    let mut live_out: Vec<Vec<LocalRef>> = vec![Vec::new(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for block in 0..n {
            let mut out: Vec<LocalRef> = Vec::new();
            for &succ in &blocks[block].succ {
                for &slot in &live_in[succ] {
                    if !out.contains(&slot) {
                        out.push(slot);
                    }
                }
            }
            let mut input = blocks[block].uses.clone();
            for &slot in &out {
                if !blocks[block].defs.contains(&slot) && !input.contains(&slot) {
                    input.push(slot);
                }
            }
            input.sort_unstable();
            out.sort_unstable();
            if input != live_in[block] || out != live_out[block] {
                live_in[block] = input;
                live_out[block] = out;
                changed = true;
            }
        }
    }
    (live_in, live_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        bytecode::opcodes as op,
        jit::{
            bytecode::{VerifyLimits, verify},
            snapshot::CompileSnapshot,
        },
    };

    fn verify_bytes(bytes: Vec<u8>, args: u16, vars: u16) -> VerifiedFunction {
        let snapshot = CompileSnapshot::from_raw(bytes, args, vars, 0, 16);
        verify(snapshot, VerifyLimits::default()).expect("verify")
    }

    fn u16b(x: u16) -> [u8; 2] {
        x.to_le_bytes()
    }
    fn i32b(x: i32) -> [u8; 4] {
        x.to_le_bytes()
    }
    fn push_i32(x: i32) -> [u8; 5] {
        let mut out = [0u8; 5];
        out[0] = op::OP_push_i32;
        out[1..].copy_from_slice(&i32b(x));
        out
    }
    fn loc(opcode: u8, idx: u16) -> [u8; 3] {
        let mut out = [0u8; 3];
        out[0] = opcode;
        out[1..].copy_from_slice(&u16b(idx));
        out
    }
    fn branch(opcode: u8, off: i32) -> [u8; 5] {
        let mut out = [0u8; 5];
        out[0] = opcode;
        out[1..].copy_from_slice(&i32b(off));
        out
    }

    #[test]
    fn def_use_splits_reads_from_writes() {
        // get_loc 0; put_loc 1; push_i32 9; put_loc 0; return_undef
        let bytes = [
            &loc(op::OP_get_loc, 0)[..],
            &loc(op::OP_put_loc, 1),
            &push_i32(9),
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]
        .concat();
        let verified = verify_bytes(bytes, 0, 2);
        let ir = Ir::build(&verified, true);
        assert_eq!(ir.blocks().len(), 1);
        let block = &ir.blocks()[0];
        assert_eq!(block.uses, vec![LocalRef::local(0)]);
        assert_eq!(block.defs, vec![LocalRef::local(1), LocalRef::local(0)]);
    }

    #[test]
    fn a_store_read_only_by_the_next_block_is_live_out() {
        // A: push_i32 1; put_loc 0; push_false; if_false L
        // dead: return_undef
        // L: get_loc 0; return
        let bytes = [
            &push_i32(1)[..],
            &loc(op::OP_put_loc, 0),
            &[op::OP_push_false],
            &branch(op::OP_if_false, 5), // target 15 = L
            &[op::OP_return_undef],
            &loc(op::OP_get_loc, 0),
            &[op::OP_return],
        ]
        .concat();
        let verified = verify_bytes(bytes, 0, 1);
        let ir = Ir::build(&verified, true);
        assert!(ir.exact());
        let first = ir.block_index(0).expect("entry block");
        assert!(ir.is_live_out(first, LocalRef::local(0)));
    }

    #[test]
    fn a_dead_store_is_not_live_out() {
        // push_i32 1; put_loc 0; push_i32 2; put_loc 1; return
        let bytes = [
            &push_i32(1)[..],
            &loc(op::OP_put_loc, 0),
            &push_i32(2),
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]
        .concat();
        let verified = verify_bytes(bytes, 0, 2);
        let ir = Ir::build(&verified, true);
        let entry = ir.block_index(0).expect("entry block");
        assert!(!ir.is_live_out(entry, LocalRef::local(0)));
        assert!(!ir.is_live_out(entry, LocalRef::local(1)));
    }

    #[test]
    fn a_loop_header_dominates_its_body() {
        // Entry: push_i32 0; put_loc 0; goto header
        // header: get_loc 0; push_i32 3; lt; if_false end
        // body: inc_loc 0; goto header
        // end: return_undef
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&push_i32(0)); // 0..5
        bytes.extend_from_slice(&loc(op::OP_put_loc, 0)); // 5..8
        bytes.extend_from_slice(&branch(op::OP_goto, 4)); // 8..13 -> header
        // header at pc 13
        let header_pc = bytes.len() as i32;
        bytes.extend_from_slice(&loc(op::OP_get_loc, 0)); // 13..16
        bytes.extend_from_slice(&push_i32(3)); // 16..21
        bytes.extend_from_slice(&[op::OP_lt]); // 21..22
        // if_false -> end (34): off = 34 - 23 = 11
        bytes.extend_from_slice(&branch(op::OP_if_false, 11)); // 22..27
        // body at pc 27
        bytes.extend_from_slice(&[op::OP_inc_loc, 0]); // 27..29
        // goto header (13): off = 13 - 30 = -17
        bytes.extend_from_slice(&branch(op::OP_goto, -17)); // 29..34
        // end at pc 34
        bytes.extend_from_slice(&[op::OP_return_undef]);
        let verified = verify_bytes(bytes, 0, 1);
        let ir = Ir::build(&verified, true);
        assert_eq!(ir.loops().len(), 1, "one natural loop");
        let header = ir.block_index(header_pc as u32).expect("header block");
        assert_eq!(ir.loops()[0].header, header);
        assert!(ir.loops()[0].members.len() >= 2, "header + body");
    }

    #[test]
    fn exception_regions_make_liveness_inexact() {
        // catch 0; return_undef; return_undef — a handler makes it inexact.
        let bytes = [
            &branch(op::OP_catch, 5)[..],
            &[op::OP_return_undef],
            &[op::OP_return_undef],
        ]
        .concat();
        let verified = verify_bytes(bytes.clone(), 0, 0);
        let ir = Ir::build(&verified, true);
        assert!(!ir.exact(), "catch region must disable exact liveness");

        // The same shape without a handler is exact.
        let plain = verify_bytes(vec![op::OP_return_undef], 0, 0);
        assert!(Ir::build(&plain, true).exact());
        assert!(!Ir::build(&plain, false).exact());
    }
}
