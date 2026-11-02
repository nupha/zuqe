// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Data-flow facts over the verified bytecode: def/use per block, liveness
//! across block edges, and the dead-store hint the lowering consumes.
//!
//! The value model stays the frame: a slot is "live" in the ordinary compiler
//! sense (read before the next write on some path). Liveness is exact only
//! when the function neither raises/catches in-frame nor contains an opcode
//! the baseline would `Resume` at — a resume can read *every* frame slot, so
//! [`Analysis::exact`] gates every consumer that skips a write. The caller
//! answers that question; this module never guesses.

use std::{collections::HashMap, ops::Range};

use crate::bytecode::OpcodeFormat;

use super::bytecode::{decode::Instruction, stack::local_index, verify::VerifiedFunction};

mod values;

pub(crate) use values::RedundantExpressions;

/// A frame slot the analysis reasons about: a local (excluding arguments) or an
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

/// One basic block of the analysis: the CFG block's instruction range plus its
/// def/use summary and successor indices.
#[derive(Clone, Debug)]
pub(crate) struct BlockInfo {
    pub(crate) start_pc: u32,
    pub(crate) range: Range<usize>,
    pub(crate) succ: Vec<usize>,
    /// Slots read before being written anywhere in this block.
    pub(crate) uses: Vec<LocalRef>,
    /// Slots written anywhere in this block.
    pub(crate) defs: Vec<LocalRef>,
}

/// The function-scoped analysis. Block indices are positions in
/// [`Analysis::blocks`].
#[derive(Clone, Debug)]
pub(crate) struct Analysis {
    blocks: Vec<BlockInfo>,
    live_out: Vec<Vec<LocalRef>>,
    /// Whether [`Self::live_out`] is exact (see module docs). Consumers that
    /// skip a write must check this.
    exact: bool,
}

impl Analysis {
    /// Build the analysis. `resume_free` is the caller's answer to "does this
    /// function contain an opcode the baseline would `Resume` at?" — the only
    /// fact the analysis cannot derive from the verified bytecode alone.
    pub(crate) fn build(verified: &VerifiedFunction, resume_free: bool) -> Self {
        let instructions = verified.instructions();
        let cfg = verified.control_flow_graph();

        let mut blocks: Vec<BlockInfo> = Vec::with_capacity(cfg.blocks().len());
        let mut block_of_pc = HashMap::new();
        for block in cfg.blocks() {
            let index = blocks.len();
            block_of_pc.insert(block.start_pc(), index);
            let range = block.instruction_range();
            let (uses, defs) = def_use(instructions, range.clone());
            blocks.push(BlockInfo {
                start_pc: block.start_pc(),
                range,
                succ: Vec::new(),
                uses,
                defs,
            });
        }
        for (index, block) in cfg.blocks().iter().enumerate() {
            for &succ_pc in block.successors() {
                if let Some(&succ) = block_of_pc.get(&succ_pc)
                    && !blocks[index].succ.contains(&succ)
                {
                    blocks[index].succ.push(succ);
                }
            }
        }

        let exact = resume_free && !verified.has_exception_regions();
        let live_out = liveness(&blocks);

        Self {
            blocks,
            live_out,
            exact,
        }
    }

    pub(crate) fn blocks(&self) -> &[BlockInfo] {
        &self.blocks
    }

    pub(crate) fn is_live_out(&self, block: usize, slot: LocalRef) -> bool {
        self.live_out[block].contains(&slot)
    }

    /// Dead-store elimination: the `put_loc`/`set_loc` pcs whose store is dead — the slot
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

        if reads_slot(name, format)
            && let Some(slot) = frame_slot(instruction)
            && !written.contains(&slot)
            && !uses.contains(&slot)
        {
            uses.push(slot);
        }

        if writes_slot(name, format)
            && let Some(slot) = frame_slot(instruction)
        {
            if !written.contains(&slot) {
                written.push(slot);
            }
            if !defs.contains(&slot) {
                defs.push(slot);
            }
        }
    }

    (uses, defs)
}

/// Classic live-variable analysis: `live_in[b] = uses[b] ∪ (live_out[b] \ defs[b])`,
/// `live_out[b] = ∪ live_in[succ]`. Only the live-out sets survive the fixpoint;
/// unreachable blocks are ignored.
fn liveness(blocks: &[BlockInfo]) -> Vec<Vec<LocalRef>> {
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
    live_out
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

    fn block_of(analysis: &Analysis, pc: u32) -> usize {
        analysis
            .blocks()
            .iter()
            .position(|block| block.start_pc == pc)
            .expect("block")
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
        let analysis = Analysis::build(&verified, true);
        assert_eq!(analysis.blocks().len(), 1);
        let block = &analysis.blocks()[0];
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
        let analysis = Analysis::build(&verified, true);
        assert!(analysis.exact);
        let first = block_of(&analysis, 0);
        assert!(analysis.is_live_out(first, LocalRef::local(0)));
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
        let analysis = Analysis::build(&verified, true);
        let entry = block_of(&analysis, 0);
        assert!(!analysis.is_live_out(entry, LocalRef::local(0)));
        assert!(!analysis.is_live_out(entry, LocalRef::local(1)));
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
        let analysis = Analysis::build(&verified, true);
        assert!(!analysis.exact, "catch region must disable exact liveness");

        // The same shape without a handler is exact.
        let plain = verify_bytes(vec![op::OP_return_undef], 0, 0);
        assert!(Analysis::build(&plain, true).exact);
        assert!(!Analysis::build(&plain, false).exact);
    }
}
