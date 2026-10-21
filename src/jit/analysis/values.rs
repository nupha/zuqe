// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright © 2026 John Ray <996351336@qq.com>

//! Epic S2 (1.5): value numbering for the operand stack, and the redundant
//! numeric expressions it exposes (CSE).
//!
//! The bytecode is a stack machine, so the *identity* of a value is written
//! down nowhere: the frame slot holding it is reused as soon as the next
//! expression needs the space. This pass simulates the operand stack one basic
//! block at a time and gives every value a stable id — a `push_i32` literal, a
//! constant-pool entry, a local/argument read (versioned by the writes to it),
//! the outputs of a stack shuffle, or the result of a pure binary over two ids
//! — so a second occurrence of the same expression over the same values is
//! recognisable.
//!
//! Alongside the ids the pass carries a **numeric** fact: the value is always an
//! `Int` (tag 0) or a `Float64` (tag 8). Only a pair of numeric operands makes a
//! binary operator *pure* — an object operand would make the interpreter run
//! `ToPrimitive`/`ToNumber`, user code a reuse would then skip — so the pass only
//! plans a reuse where both operands are proven numeric (the set it numbers is
//! `compiler::is_cse_expression`, i.e. exactly what the lowering lowers with a
//! fast path), and the lowering needs
//! no runtime guard: it replaces the whole computation with a copy. A local
//! keeps the fact only through a plain store (see [`numeric_result`] and the
//! store handling in [`solve`]); everything ambiguous — an argument, a helper
//! result, a `++`, an uninitialized slot — is not numeric.
//!
//! Per block, like the reference implementation (`jit/src/ir/optimized.rs::
//! rewrite_pure_expressions`). A value cannot cross a block boundary as a
//! register — an edge delivers *data* in the frame — so a cross-block claim
//! would need the ids to agree on every incoming edge, a join this pass does not
//! build yet.

use std::collections::{HashMap, HashSet};

use crate::jit::{
    bytecode::{
        decode::Instruction,
        stack::{SlotKind, copied_stack_values, effective_pop},
        verify::VerifiedFunction,
    },
    compiler::is_cse_expression,
    snapshot::ConstMeta,
};

use super::{LocalRef, frame_slot, writes_slot};

/// One value the pass can name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Key {
    /// A `push_i32` literal.
    Int(i32),
    /// A constant-pool entry; the tag separates an `Int` from a `Float64`.
    Const(u32, i16),
    /// `undefined`/`null`/`push_true`/`push_false`: singletons, so equal names
    /// are equal values. (`push_this` is deliberately absent — sloppy `this`
    /// objectification may hand out a fresh wrapper object.)
    Special(&'static str),
    /// A local/argument read: the slot and the write generation it observes.
    Local(bool, u32, u32),
    /// A pure binary over two value ids.
    Binary(&'static str, u32, u32),
}

/// Whether a binary's result is always an `Int` or a `Float64`.
///
/// The bitwise/shift family boxes through `From<u32>`/`From<i32>` in both
/// engines (`WrappingOp`, `box_bit_result`), so its result is a number whatever
/// the operands were. `+`/`-`/`*` are numeric exactly when *their* operands are
/// (a string or an object makes them produce something else); a comparison
/// produces a `Bool`.
fn numeric_result(name: &str, a: u32, b: u32, interner: &Interner) -> bool {
    match name {
        "and" | "or" | "xor" | "shl" | "shr" | "sar" => true,
        "add" | "sub" | "mul" => interner.is_numeric(a) && interner.is_numeric(b),
        _ => false,
    }
}

/// What the lowering needs to know about the redundancies the pass found.
#[derive(Default)]
pub(crate) struct RedundantExpressions {
    /// pc whose result must stay in a register because a later instruction in
    /// the same block reuses it: pc → the value's id.
    keep: HashMap<u32, u32>,
    /// pc that should reuse an already-computed value instead of recomputing
    /// it: pc → the value's id. The lowering only honors this when it actually
    /// holds that id in a register (see [`Self::reuse`]).
    reuse: HashMap<u32, u32>,
}

impl RedundantExpressions {
    /// Number the values of every block and record the reusable expressions.
    /// `gate` must mean "a frame slot the pass believes is unchanged really is"
    /// — the caller's `cacheable_slots` plus the analysis's exactness (the same gate
    /// dead-store elimination uses); otherwise the result is empty.
    pub(crate) fn build(verified: &VerifiedFunction, gate: bool) -> Self {
        let mut plan = Self::default();
        if gate {
            solve(verified, &mut plan);
        }
        plan
    }

    /// The id this instruction's result should be published as, if a later
    /// instruction reuses it. Every reusable value is a number (the pass only
    /// names `Int`/`Float64` results), so the register never has to be visible
    /// to the collector.
    pub(crate) fn keep(&self, pc: u32) -> Option<u32> {
        self.keep.get(&pc).copied()
    }

    /// The id this instruction should reuse, if it is a repeat of an expression
    /// computed earlier in the same block. The lowering must have published that
    /// id (`keep`) on the path it arrived by — an OSR entry in the middle of the
    /// block starts past the definition and finds no register, which is why the
    /// lowering checks its register map and computes normally when it is
    /// missing.
    pub(crate) fn reuse(&self, pc: u32) -> Option<u32> {
        self.reuse.get(&pc).copied()
    }
}

/// Ids, deduplicated by key, with the numeric fact the pass proved of each.
struct Interner {
    interned: HashMap<Key, u32>,
    /// `numeric[id]`: the value is always an `Int` or `Float64`.
    numeric: Vec<bool>,
}

impl Interner {
    fn new() -> Self {
        Self {
            interned: HashMap::new(),
            // Index 0 is "unknown" and never numeric.
            numeric: vec![false],
        }
    }

    fn is_numeric(&self, id: u32) -> bool {
        self.numeric.get(id as usize).copied().unwrap_or(false)
    }

    /// The id of `key`, creating it on first sight. `numeric` is the caller's
    /// fact about the value; two sightings of one key always agree, because the
    /// fact is derived from the key itself.
    fn intern(&mut self, key: Key, numeric: bool) -> u32 {
        if let Some(&id) = self.interned.get(&key) {
            return id;
        }
        let id = self.fresh(numeric);
        self.interned.insert(key, id);
        id
    }

    /// A value this pass cannot name — never equal to any other value.
    fn fresh(&mut self, numeric: bool) -> u32 {
        self.numeric.push(numeric);
        (self.numeric.len() - 1) as u32
    }
}

fn solve(verified: &VerifiedFunction, plan: &mut RedundantExpressions) {
    let instructions = verified.instructions();
    let heights = verified.stack_heights();
    let cfg = verified.control_flow_graph();
    let const_meta = &verified.snapshot().const_meta;
    // Slot indices are 1-based (`stack::StackCache`); 0 is never addressed.
    let slot_count = verified.snapshot().stack_size as usize + 1;

    for block in cfg.blocks() {
        let range = block.instruction_range();
        let end = range.end;
        let mut slots = vec![0u32; slot_count];
        let mut versions: HashMap<LocalRef, u32> = HashMap::new();
        // Whether a local/argument's *current* value is a number.
        let mut numeric_local: HashMap<LocalRef, bool> = HashMap::new();
        let mut interner = Interner::new();
        // The first computation of each expression in this block.
        let mut expressions: HashMap<Key, (u32, u32)> = HashMap::new();
        let mut reuses: Vec<(u32, u32)> = Vec::new();

        for index in range {
            let insn = &instructions[index];
            let name = insn.name();
            let before = heights[index] as usize;
            let pop = effective_pop(insn);
            let push = insn.info().n_push as usize;
            if before < pop || pop > slot_count {
                break; // unreachable/malformed; the verifier owns that verdict
            }
            let operands: Vec<u32> = (0..pop).map(|i| slots[before - pop + 1 + i]).collect();

            let mut outputs: Vec<u32> = Vec::with_capacity(push);
            if let Some(id) = value_id(
                insn,
                name,
                &versions,
                &numeric_local,
                &mut interner,
                const_meta,
            ) {
                outputs.push(id);
            } else if is_cse_expression(name) && pop == 2 && push == 1 {
                let both_numeric =
                    interner.is_numeric(operands[0]) && interner.is_numeric(operands[1]);
                if operands[0] != 0 && operands[1] != 0 {
                    let key = Key::Binary(name, operands[0], operands[1]);
                    let numeric = numeric_result(name, operands[0], operands[1], &interner);
                    let id = interner.intern(key, numeric);
                    let repeat = expressions.get(&key).copied();
                    // Only a pair of proven numbers makes the operation pure:
                    // reusing the earlier result then cannot skip a
                    // `ToPrimitive`/`ToNumber` the interpreter would have run.
                    if both_numeric && repeat.is_some() {
                        reuses.push((insn.pc(), id));
                    } else if repeat.is_none() {
                        expressions.insert(key, (insn.pc(), id));
                    }
                    outputs.push(id);
                } else {
                    // An operand the pass cannot name: the result is a value of
                    // its own, and no later occurrence can be proved equal.
                    outputs.push(interner.fresh(false));
                }
            } else if let Some(sources) = shuffle_output_sources(name, pop, push) {
                outputs.extend(sources.iter().map(|&i| operands[i]));
            } else {
                outputs.extend((0..push).map(|_| interner.fresh(false)));
            }

            // The instruction's real effect, from the verifier's proven heights
            // (the metadata is not the real effect for the in-place window ops).
            let nominal_after = before - pop + push;
            let after = if index + 1 < end {
                heights[index + 1] as usize
            } else {
                nominal_after
            };
            let keep = before.min(after);
            for slot in slots.iter_mut().take(before.max(after) + 1).skip(keep + 1) {
                *slot = 0;
            }
            // Only the shapes whose operands are exactly `(pop, push)` got their
            // outputs placed above; a window op leaves the region unknown.
            if after == nominal_after {
                for (i, &id) in outputs.iter().enumerate() {
                    slots[before - pop + 1 + i] = id;
                }
            }

            if let Some(slot) = writes_slot(name, insn.format())
                .then(|| frame_slot(insn))
                .flatten()
            {
                let version = versions.entry(slot).or_default();
                *version = version.saturating_add(1);
                // A plain store carries the stored value's fact. The
                // read-modify-write ops (`inc_loc`/`add_loc`) can produce a
                // BigInt or a string too, so they only ever clear it, as does
                // `set_loc_uninitialized` (the uninitialized sentinel).
                let stored = matches!(
                    name,
                    "put_loc"
                        | "set_loc"
                        | "put_loc_check"
                        | "put_loc_check_init"
                        | "set_loc_check"
                        | "put_arg"
                        | "set_arg"
                );
                let numeric = stored && pop == 1 && interner.is_numeric(operands[0]);
                numeric_local.insert(slot, numeric);
            }
        }

        // A definition only needs a register if something reuses it.
        let reused: HashSet<u32> = reuses.iter().map(|&(_, id)| id).collect();
        for &(pc, id) in expressions.values() {
            if reused.contains(&id) {
                plan.keep.insert(pc, id);
            }
        }
        for (pc, id) in reuses {
            plan.reuse.insert(pc, id);
        }
    }
}

/// The id of a value the instruction materialises from nothing — a literal, a
/// pool constant or a local/argument read. Everything else (a helper result, an
/// object or array literal, `push_this`, …) is not nameable.
fn value_id(
    insn: &Instruction,
    name: &'static str,
    versions: &HashMap<LocalRef, u32>,
    numeric_local: &HashMap<LocalRef, bool>,
    interner: &mut Interner,
    const_meta: &[ConstMeta],
) -> Option<u32> {
    match name {
        "get_loc" | "get_loc_check" | "get_loc_checkthis" | "get_arg" => {
            let slot = frame_slot(insn)?;
            let version = versions.get(&slot).copied().unwrap_or(0);
            let numeric = numeric_local.get(&slot).copied().unwrap_or(false);
            Some(interner.intern(Key::Local(slot.is_arg, slot.idx, version), numeric))
        }
        "push_i32" => Some(interner.intern(Key::Int(insn.operand_i32(1)), true)),
        "push_const" => {
            let index = insn.operand_u32(1);
            let tag = const_meta.get(index as usize).map_or(-1, |meta| meta.tag);
            let numeric = matches!(tag, 0 | 8);
            Some(interner.intern(Key::Const(index, tag), numeric))
        }
        "undefined" | "null" | "push_true" | "push_false" => {
            Some(interner.intern(Key::Special(name), false))
        }
        _ => None,
    }
}

/// The popped slot each output of a stack shuffle comes from. The shared table
/// (`copied_stack_values`) reports the outputs' *values*, so five distinct
/// marker kinds recover their positions.
fn shuffle_output_sources(name: &str, pop: usize, push: usize) -> Option<Vec<usize>> {
    const MARKERS: [SlotKind; 5] = [
        SlotKind::Tagged,
        SlotKind::Int32,
        SlotKind::Float64,
        SlotKind::CatchOffset,
        SlotKind::Uninitialized,
    ];
    if pop > MARKERS.len() {
        return None;
    }
    let outputs = copied_stack_values(name, &MARKERS[..pop])?;
    if outputs.len() != push {
        return None;
    }
    outputs
        .iter()
        .map(|kind| MARKERS.iter().position(|marker| marker == kind))
        .collect()
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

    fn plan(bytes: Vec<u8>, args: u16, vars: u16) -> RedundantExpressions {
        let snapshot = CompileSnapshot::from_raw(bytes, args, vars, 0, 16);
        let verified = verify(snapshot, VerifyLimits::default()).expect("verify");
        RedundantExpressions::build(&verified, true)
    }

    /// The one planned pair: the definition's pc/register id and the pc that
    /// reuses them. Panics if the plan is not exactly one reuse of exactly one
    /// definition.
    fn pair(plan: &RedundantExpressions) -> (u32, u32, u32, u32) {
        assert_eq!(plan.keep.len(), 1, "one definition expected");
        assert_eq!(plan.reuse.len(), 1, "one reuse expected");
        let (&define_pc, &id) = plan.keep.iter().next().unwrap();
        let (&reuse_pc, &reuse_id) = plan.reuse.iter().next().unwrap();
        assert_eq!(id, reuse_id, "the reuse names the definition's value");
        assert!(define_pc < reuse_pc, "the definition comes first");
        (define_pc, reuse_pc, id, id)
    }

    /// A literal store makes a local numeric, which is what lets a repeat of an
    /// expression over it be reused: the first computation publishes the value
    /// and the second copies it.
    #[test]
    fn a_repeated_numeric_expression_reuses_the_first_result() {
        // push_i32 5; put_loc 0; get_loc 0; push_i32 1; add; get_loc 0; push_i32 1; add; …
        let bytes = [
            &push_i32(5)[..],
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]
        .concat();
        pair(&plan(bytes, 0, 2));
    }

    /// A write to an operand invalidates the expression.
    #[test]
    fn a_write_to_an_operand_breaks_the_match() {
        let bytes = [
            &push_i32(5)[..],
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &push_i32(9),
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]
        .concat();
        let plan = plan(bytes, 0, 2);
        assert!(plan.reuse.is_empty(), "x changed, so x+1 is not repeatable");
        assert!(plan.keep.is_empty(), "nothing reuses the first result");
    }

    /// An operand the pass cannot prove numeric (an argument) is never reused:
    /// recomputing it would run the interpreter's `ToPrimitive` a second time.
    #[test]
    fn an_unproven_operand_is_not_reused() {
        // get_arg 0; push_i32 1; add; get_arg 0; push_i32 1; add; …
        let bytes = [
            &loc(op::OP_get_arg, 0)[..],
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_get_arg, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]
        .concat();
        let plan = plan(bytes, 1, 1);
        assert!(plan.reuse.is_empty());
        assert!(plan.keep.is_empty());
    }

    /// A shuffle carries ids: `dup` of a numeric local then `add` is the same
    /// expression as `get_loc; get_loc; add` over the same value.
    #[test]
    fn duplicated_operands_keep_their_identity() {
        // push_i32 5; put_loc 0; get_loc 0; dup; add; get_loc 0; dup; add; …
        let bytes = [
            &push_i32(5)[..],
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &[op::OP_dup, op::OP_add],
            &loc(op::OP_get_loc, 0),
            &[op::OP_dup, op::OP_add],
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]
        .concat();
        pair(&plan(bytes, 0, 2));
    }

    /// A bitwise result is a number whatever its operands were, so a local that
    /// holds one is numeric — the shape a `x = i & 7` loop body has.
    #[test]
    fn a_bitwise_result_makes_a_local_numeric() {
        // get_arg 0; push_i32 7; and; put_loc 0; get_loc 0; push_i32 1; add;
        // get_loc 0; push_i32 1; add; …
        let bytes = [
            &loc(op::OP_get_arg, 0)[..],
            &push_i32(7),
            &[op::OP_and],
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]
        .concat();
        pair(&plan(bytes, 1, 2));
    }

    /// A comparison produces a `Bool`, so its result is *not* numeric and a
    /// repeat of an expression over it is left alone.
    #[test]
    fn a_boolean_result_is_not_numeric() {
        // get_arg 0; push_i32 1; lt; get_arg 0; push_i32 1; lt; add; add; …
        let bytes = [
            &loc(op::OP_get_arg, 0)[..],
            &push_i32(1),
            &[op::OP_lt],
            &loc(op::OP_get_arg, 0),
            &push_i32(1),
            &[op::OP_lt],
            &[op::OP_add],
            &[op::OP_drop, op::OP_return_undef],
        ]
        .concat();
        let plan = plan(bytes, 1, 0);
        assert!(
            plan.reuse.is_empty(),
            "a Bool operand is not proven numeric"
        );
    }

    /// The pass is per block: the same expression in a later block is not a
    /// repeat (an edge delivers data, not registers).
    #[test]
    fn expressions_do_not_cross_a_block_boundary() {
        // entry: push_i32 5; put_loc 0; get_loc 0; get_loc 0; add; drop;
        //        push_false; if_false L; return_undef
        // L:     get_loc 0; get_loc 0; add; put_loc 1; return_undef
        let bytes = [
            &push_i32(5)[..],
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &loc(op::OP_get_loc, 0),
            &[op::OP_add, op::OP_drop, op::OP_push_false],
            &branch(op::OP_if_false, 5),
            &[op::OP_return_undef],
            &loc(op::OP_get_loc, 0),
            &loc(op::OP_get_loc, 0),
            &[op::OP_add],
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]
        .concat();
        let plan = plan(bytes, 0, 2);
        assert!(plan.reuse.is_empty(), "the second add is another block");
    }

    /// Nothing is planned when the caller's gate is closed.
    #[test]
    fn the_gate_disables_every_hint() {
        let bytes = [
            &push_i32(5)[..],
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add],
            &loc(op::OP_get_loc, 0),
            &push_i32(1),
            &[op::OP_add, op::OP_drop, op::OP_return_undef],
        ]
        .concat();
        let snapshot = CompileSnapshot::from_raw(bytes, 0, 2, 0, 16);
        let verified = verify(snapshot, VerifyLimits::default()).expect("verify");
        let plan = RedundantExpressions::build(&verified, false);
        assert!(plan.keep.is_empty());
        assert!(plan.reuse.is_empty());
    }
}
