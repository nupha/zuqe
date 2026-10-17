// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright © 2026 John Ray <996351336@qq.com>

//! Abstract interpretation over operands and locals.
//!
//! The verifier proves, for every reachable instruction, the stack height and
//! (where it can) the representation of each slot. Locals are tagged JSValue
//! cells in the frame; the only representations worth tracking are the ones the
//! generator will later unpack: `Int32`, `Float64`, the `CatchOffset` sentinel,
//! and `Uninitialized`. Everything else is `Tagged`.
//!
//! Two invariants matter for safety: the operand stack never underflows or
//! exceeds the declared `stack_size`, and every control-flow join agrees on the
//! operand-stack height and representation. `Int32`/`Float64`/`Uninitialized`
//! only *prove* a shape of a JSValue the lowering can unpack (tag 0, the float
//! encoding, the uninitialized sentinel), so a join may drop the proof and
//! widen to `Tagged` — the value itself is valid either way. `CatchOffset` is
//! structural (the unwinder truncates the stack to it) and must agree exactly.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::{bytecode::OpcodeFormat, jit::snapshot::CompileSnapshot};

use super::{
    cfg::ControlFlowGraph,
    decode::Instruction,
    verify::{Resource, VerifyError, VerifyErrorKind},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SlotKind {
    Tagged,
    Int32,
    Float64,
    CatchOffset,
    Uninitialized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AbstractState {
    pub(crate) locals: Vec<SlotKind>,
    pub(crate) stack: Vec<SlotKind>,
    /// Live catch offsets, innermost last: the operand-stack index of each
    /// `CatchOffset` slot and its handler (`None` for iterator close offsets,
    /// which the interpreter unwinds through without resuming).
    pub(crate) catches: Vec<(usize, Option<u32>)>,
}

/// Where an exception raised while executing an instruction resumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExceptionHandler {
    catch_index: u16,
    handler_pc: Option<u32>,
    resumes_in_frame: bool,
}

impl ExceptionHandler {
    pub(crate) const fn catch_index(self) -> u16 {
        self.catch_index
    }

    pub(crate) const fn handler_pc(self) -> Option<u32> {
        self.handler_pc
    }

    pub(crate) const fn resumes_in_frame(self) -> bool {
        self.resumes_in_frame
    }
}

impl AbstractState {
    fn innermost_catch(&self) -> Option<ExceptionHandler> {
        self.catches.last().and_then(|(index, handler_pc)| {
            Some(ExceptionHandler {
                catch_index: u16::try_from(*index).ok()?,
                handler_pc: *handler_pc,
                resumes_in_frame: self.innermost_handler().is_some(),
            })
        })
    }

    fn innermost_handler(&self) -> Option<(usize, u32)> {
        self.catches
            .iter()
            .rev()
            .find_map(|(index, handler_pc)| handler_pc.map(|pc| (*index, pc)))
    }

    fn cell_count(&self) -> usize {
        self.locals.len().saturating_add(self.stack.len())
    }

    /// The frame-buffer slots live at `pc` in interpreter order: arguments,
    /// then locals, then the operand stack. This is what an OSR entry must
    /// materialize.
    pub(crate) fn live_slots(&self, snapshot: &CompileSnapshot) -> Vec<SlotKind> {
        let mut result =
            Vec::with_capacity(snapshot.arg_count as usize + self.locals.len() + self.stack.len());
        result.resize(snapshot.arg_count as usize, SlotKind::Tagged);
        result.extend_from_slice(&self.locals);
        result.extend_from_slice(&self.stack);
        result
    }
}

pub(crate) struct StateProof {
    pub(crate) before: BTreeMap<u32, AbstractState>,
    pub(crate) visited: BTreeSet<u32>,
    pub(crate) handlers: BTreeMap<u32, ExceptionHandler>,
    /// Operand-stack height *before* each instruction, indexed by instruction
    /// index (epic F4). Unvisited (unreachable) instructions stay 0 and are
    /// never lowered. Bounded by the u16 `stack_size`, which `check_stack_size`
    /// enforces after every transfer.
    pub(crate) heights: Vec<u16>,
    /// Verifier-proven local kinds *before* each `get_loc`-family instruction,
    /// keyed by pc (epic 1.4 item 3). Only reads record here, so the map stays
    /// small; the lowering feeds `Int32` slots into its guard-elimination fact.
    pub(crate) local_kinds: BTreeMap<u32, Box<[SlotKind]>>,
}

struct WorkBudget {
    used: usize,
    limit: usize,
}

impl WorkBudget {
    fn charge(&mut self, pc: u32, units: usize) -> Result<(), VerifyError> {
        self.used = self.used.saturating_add(units);
        if self.used > self.limit {
            return Err(VerifyError::new(
                pc,
                VerifyErrorKind::ResourceLimit {
                    resource: Resource::WorkUnits,
                },
            ));
        }
        Ok(())
    }
}

/// Operands popped, including any count encoded in the instruction (`call`,
/// `eval`, `array_from`, …).
pub(crate) fn effective_pop(instruction: &Instruction) -> usize {
    let base = instruction.info().n_pop as usize;
    match instruction.format() {
        OpcodeFormat::NPOP | OpcodeFormat::NPOP_U16 => {
            base.saturating_add(instruction.operand_u16(1) as usize)
        }
        OpcodeFormat::NPOPX => instruction
            .name()
            .as_bytes()
            .last()
            .and_then(|value| value.is_ascii_digit().then_some((value - b'0') as usize))
            .map_or(base, |arguments| base.saturating_add(arguments)),
        _ => base,
    }
}

pub(crate) fn local_index(instruction: &Instruction) -> Option<usize> {
    match instruction.format() {
        OpcodeFormat::LOC => Some(instruction.operand_u16(1) as usize),
        OpcodeFormat::LOC8 => Some(instruction.operand_u8(1) as usize),
        OpcodeFormat::NONE_LOC => instruction
            .name()
            .as_bytes()
            .last()
            .and_then(|value| value.is_ascii_digit().then_some((value - b'0') as usize)),
        _ => None,
    }
}

fn pushed_kind(snapshot: &CompileSnapshot, instruction: &Instruction) -> SlotKind {
    match instruction.name() {
        "push_i32" => SlotKind::Int32,
        "push_const" => match snapshot.const_meta.get(instruction.operand_u32(1) as usize) {
            // `Int` is tag 0; `Float64` is the only other numeric tag.
            Some(meta) if meta.tag == 0 => SlotKind::Int32,
            Some(meta) if meta.tag == 8 => SlotKind::Float64,
            _ => SlotKind::Tagged,
        },
        "catch" => SlotKind::CatchOffset,
        _ => SlotKind::Tagged,
    }
}

fn copied_stack_values(name: &str, popped: &[SlotKind]) -> Option<Vec<SlotKind>> {
    let values = match (name, popped) {
        ("nip", &[_, b]) => vec![b],
        ("nip1", &[_, b, c]) => vec![b, c],
        ("dup", &[a]) => vec![a, a],
        ("dup1", &[a, b]) => vec![a, a, b],
        ("dup2", &[a, b]) => vec![a, b, a, b],
        ("dup3", &[a, b, c]) => vec![a, b, c, a, b, c],
        ("insert2", &[object, a]) => vec![a, object, a],
        ("insert3", &[object, property, a]) => vec![a, object, property, a],
        ("insert4", &[this, object, property, a]) => vec![a, this, object, property, a],
        ("perm3", &[object, a, b]) => vec![a, object, b],
        ("perm4", &[object, property, a, b]) => vec![a, object, property, b],
        ("perm5", &[this, object, property, a, b]) => vec![a, this, object, property, b],
        ("swap", &[a, b]) => vec![b, a],
        ("swap2", &[a, b, c, d]) => vec![c, d, a, b],
        ("rot3l", &[a, b, c]) => vec![b, c, a],
        ("rot3r", &[a, b, c]) => vec![c, a, b],
        ("rot4l", &[a, b, c, d]) => vec![b, c, d, a],
        ("rot5l", &[a, b, c, d, e]) => vec![b, c, d, e, a],
        _ => return None,
    };
    Some(values)
}

fn check_stack_size(
    snapshot: &CompileSnapshot,
    instruction: &Instruction,
    state: &AbstractState,
) -> Result<(), VerifyError> {
    if state.stack.len() > snapshot.stack_size as usize {
        return Err(VerifyError::new(
            instruction.pc(),
            VerifyErrorKind::StackSizeExceeded {
                declared: snapshot.stack_size,
                actual: state.stack.len(),
            },
        ));
    }
    Ok(())
}

fn transfer(
    snapshot: &CompileSnapshot,
    instruction: &Instruction,
    state: &mut AbstractState,
) -> Result<(), VerifyError> {
    let pop = effective_pop(instruction);
    if state.stack.len() < pop {
        return Err(VerifyError::new(
            instruction.pc(),
            VerifyErrorKind::StackUnderflow {
                needed: pop,
                available: state.stack.len(),
            },
        ));
    }

    let name = instruction.name();
    if name == "nip_catch" {
        let value = state.stack.pop().ok_or_else(|| {
            VerifyError::new(
                instruction.pc(),
                VerifyErrorKind::StackUnderflow {
                    needed: 1,
                    available: 0,
                },
            )
        })?;
        let Some((index, _)) = state.catches.pop() else {
            return Err(VerifyError::new(
                instruction.pc(),
                VerifyErrorKind::UnsupportedExceptionRegion,
            ));
        };
        if state.stack.get(index) != Some(&SlotKind::CatchOffset) {
            return Err(VerifyError::new(
                instruction.pc(),
                VerifyErrorKind::UnsupportedExceptionRegion,
            ));
        }
        state.stack.truncate(index);
        state.stack.push(value);
        return check_stack_size(snapshot, instruction, state);
    }

    let popped = state.stack.split_off(state.stack.len() - pop);
    let popped_top = popped.last().copied().unwrap_or(SlotKind::Tagged);

    if matches!(
        name,
        "for_of_next" | "for_in_next" | "iterator_next" | "iterator_call"
    ) {
        // The nominal pops are only the window the interpreter reads in place:
        // the iteration record (including its catch offset) and the values
        // below stay unchanged. `for_of_next`/`for_in_next` append `value` and
        // `done`; `iterator_next` keeps the window; `iterator_call` appends its
        // flag above the overwritten argument.
        let extra = (instruction.info().n_push as usize).saturating_sub(pop);
        state.stack.extend(popped);
        state
            .stack
            .extend(std::iter::repeat_n(SlotKind::Tagged, extra));
        return check_stack_size(snapshot, instruction, state);
    }

    // Any catch offset consumed by this instruction is no longer live.
    while state
        .catches
        .last()
        .is_some_and(|(index, _)| *index >= state.stack.len())
    {
        state.catches.pop();
    }

    if name.starts_with("get_loc") {
        let index = local_index(instruction).expect("local format was validated");
        state.stack.push(state.locals[index]);
        return check_stack_size(snapshot, instruction, state);
    }
    if name == "set_loc_uninitialized" {
        let index = local_index(instruction).expect("local format was validated");
        state.locals[index] = SlotKind::Uninitialized;
    } else if name.starts_with("put_loc") || name.starts_with("set_loc") {
        let index = local_index(instruction).expect("local format was validated");
        state.locals[index] = popped_top;
    }

    if matches!(name, "inc_loc" | "dec_loc" | "add_loc") {
        let index = local_index(instruction).expect("local format was validated");
        state.locals[index] = SlotKind::Tagged;
    }

    if matches!(name, "for_of_start" | "for_await_of_start") {
        state
            .stack
            .extend([SlotKind::Tagged, SlotKind::Tagged, SlotKind::CatchOffset]);
        state.catches.push((state.stack.len() - 1, None));
        return check_stack_size(snapshot, instruction, state);
    }

    if name == "catch" {
        let target = instruction
            .branch_target()
            .and_then(|target| u32::try_from(target).ok())
            .ok_or_else(|| {
                VerifyError::new(
                    instruction.pc(),
                    VerifyErrorKind::UnsupportedExceptionRegion,
                )
            })?;
        state.stack.push(SlotKind::CatchOffset);
        state.catches.push((state.stack.len() - 1, Some(target)));
        return check_stack_size(snapshot, instruction, state);
    }

    if name == "gosub" {
        // The return address is an Int32 bytecode offset consumed by `ret`.
        state.stack.push(SlotKind::Int32);
        return check_stack_size(snapshot, instruction, state);
    }

    if let Some(values) = copied_stack_values(name, &popped) {
        state.stack.extend(values);
        return check_stack_size(snapshot, instruction, state);
    }

    if name == "to_propkey" {
        // Property keys already Int32 stay Int32; any other value may become a
        // string. Preserve only that exact fast-path proof.
        state.stack.push(if popped_top == SlotKind::Int32 {
            SlotKind::Int32
        } else {
            SlotKind::Tagged
        });
        return check_stack_size(snapshot, instruction, state);
    }

    if instruction.info().n_push == 1
        && (name.starts_with("set_loc")
            || name.starts_with("set_arg")
            || name.starts_with("set_var_ref"))
    {
        state.stack.push(popped_top);
        return check_stack_size(snapshot, instruction, state);
    }

    let push = instruction.info().n_push as usize;
    state.stack.extend(std::iter::repeat_n(
        pushed_kind(snapshot, instruction),
        push,
    ));
    check_stack_size(snapshot, instruction, state)
}

fn merge_state(
    pc: u32,
    expected: &mut AbstractState,
    actual: &AbstractState,
) -> Result<bool, VerifyError> {
    if expected.stack.len() != actual.stack.len() {
        return Err(VerifyError::new(
            pc,
            VerifyErrorKind::InconsistentMergeHeight {
                expected: expected.stack.len(),
                actual: actual.stack.len(),
            },
        ));
    }
    if expected.catches != actual.catches {
        return Err(VerifyError::new(
            pc,
            VerifyErrorKind::UnsupportedExceptionRegion,
        ));
    }
    let mut changed = false;
    for (slot, (expected, actual)) in expected.stack.iter_mut().zip(&actual.stack).enumerate() {
        let prior = *expected;
        if prior == *actual {
            continue;
        }
        // A merge is not an operation and cannot insert a conversion — but
        // none is needed: the joinable pairs differ only in a proof, so the
        // operand widens to `Tagged` (see the module docs).
        let Some(joined) = join_kinds(prior, *actual) else {
            return Err(incompatible_merge(pc, slot, prior, *actual));
        };
        *expected = joined;
        changed = true;
    }

    let stack_slots = expected.stack.len();
    for (local, (expected, actual)) in expected.locals.iter_mut().zip(&actual.locals).enumerate() {
        let prior = *expected;
        if prior == *actual {
            continue;
        }
        let Some(joined) = join_kinds(prior, *actual) else {
            return Err(incompatible_merge(pc, stack_slots + local, prior, *actual));
        };
        *expected = joined;
        changed = true;
    }
    Ok(changed)
}

/// The join of two representations at a merge point, or `None` when they
/// disagree structurally (a `CatchOffset` meeting a JS value), which no
/// widening can repair.
///
/// Every joinable pair widens to `Tagged`: `Int32` (tag 0), `Float64` and
/// `Uninitialized` are all valid JSValue encodings that differ only in the
/// proof the lowering would have used, and every consumer handles the general
/// case. A local consequently stays an authoritative tagged cell, and an
/// operand a variable a `get_loc_check` would have narrowed. A lexical binding
/// declared in a loop body is uninitialized on entry and initialized on the
/// backedge; its read still runs the TDZ test.
const fn join_kinds(expected: SlotKind, actual: SlotKind) -> Option<SlotKind> {
    Some(match (expected, actual) {
        (SlotKind::Tagged, SlotKind::Int32 | SlotKind::Float64 | SlotKind::Uninitialized)
        | (SlotKind::Int32 | SlotKind::Float64 | SlotKind::Uninitialized, SlotKind::Tagged)
        | (SlotKind::Int32, SlotKind::Float64)
        | (SlotKind::Float64, SlotKind::Int32)
        | (SlotKind::Uninitialized, SlotKind::Int32 | SlotKind::Float64)
        | (SlotKind::Int32 | SlotKind::Float64, SlotKind::Uninitialized) => SlotKind::Tagged,
        _ => return None,
    })
}

const fn incompatible_merge(
    pc: u32,
    slot: usize,
    expected: SlotKind,
    actual: SlotKind,
) -> VerifyError {
    VerifyError::new(
        pc,
        VerifyErrorKind::IncompatibleMergeKind {
            slot,
            expected,
            actual,
        },
    )
}

pub(crate) fn prove(
    snapshot: &CompileSnapshot,
    instructions: &[Instruction],
    cfg: &ControlFlowGraph,
    max_work_units: usize,
) -> Result<StateProof, VerifyError> {
    if instructions.is_empty() {
        return Ok(StateProof {
            before: BTreeMap::new(),
            visited: BTreeSet::new(),
            handlers: BTreeMap::new(),
            heights: Vec::new(),
            local_kinds: BTreeMap::new(),
        });
    }
    let before_points: BTreeSet<u32> = cfg
        .blocks()
        .iter()
        .map(|block| block.start_pc())
        .filter(|pc| cfg.is_loop_header(*pc))
        .collect();
    let mut budget = WorkBudget {
        used: 0,
        limit: max_work_units,
    };
    budget.charge(0, snapshot.var_count as usize)?;
    let mut block_entries = BTreeMap::new();
    block_entries.insert(
        0,
        AbstractState {
            locals: vec![SlotKind::Tagged; snapshot.var_count as usize],
            stack: Vec::new(),
            catches: Vec::new(),
        },
    );
    let mut queue = VecDeque::from([0_u32]);
    let mut before = BTreeMap::new();
    let mut visited = BTreeSet::new();
    let mut handlers = BTreeMap::new();
    let mut heights = vec![0u16; instructions.len()];
    let mut local_kinds = BTreeMap::new();

    while let Some(block_pc) = queue.pop_front() {
        let block = cfg.block(block_pc).expect("CFG successor names a block");
        let entry = &block_entries[&block_pc];
        budget.charge(block_pc, entry.cell_count())?;
        let mut state = entry.clone();
        let range = block.instruction_range();
        for (offset, instruction) in instructions[range.clone()].iter().enumerate() {
            budget.charge(instruction.pc(), 1usize.saturating_add(state.cell_count()))?;
            // Epic F4: the lowering keys its stack cache by this height.
            heights[range.start + offset] = state.stack.len() as u16;
            // Epic 1.4 item 3: record the proven local kinds for reads only.
            if instruction.name().starts_with("get_loc") {
                local_kinds.insert(instruction.pc(), state.locals.clone().into_boxed_slice());
            }
            if before_points.contains(&instruction.pc()) {
                budget.charge(instruction.pc(), state.cell_count())?;
                before.insert(instruction.pc(), state.clone());
            }
            visited.insert(instruction.pc());
            if let Some(handler) = state.innermost_catch() {
                handlers.insert(instruction.pc(), handler);
            }
            if let Some((_, handler_pc)) = state.innermost_handler() {
                budget.charge(instruction.pc(), state.locals.len())?;
                let Some(expected) = block_entries.get_mut(&handler_pc) else {
                    return Err(VerifyError::new(
                        instruction.pc(),
                        VerifyErrorKind::UnsupportedExceptionRegion,
                    ));
                };
                if join_handler_locals(handler_pc, expected, &state.locals)? {
                    queue.push_back(handler_pc);
                }
            }
            transfer(snapshot, instruction, &mut state)?;
        }
        let tail = &instructions[block.instruction_range()]
            .last()
            .expect("CFG blocks are non-empty");
        let catch_target = (tail.name() == "catch")
            .then(|| tail.branch_target())
            .flatten()
            .and_then(|target| u32::try_from(target).ok());
        if catch_target.is_some_and(|target| {
            usize::try_from(target).ok() == (tail.pc() as usize).checked_add(tail.size())
        }) {
            return Err(VerifyError::new(
                tail.pc(),
                VerifyErrorKind::UnsupportedExceptionRegion,
            ));
        }
        for successor in block.successors() {
            if catch_target == Some(*successor) {
                let mut incoming = state.clone();
                let caught = incoming.stack.last_mut().filter(|kind| {
                    **kind == SlotKind::CatchOffset
                        && incoming
                            .catches
                            .last()
                            .is_some_and(|(_, handler)| *handler == Some(*successor))
                });
                let Some(caught) = caught else {
                    return Err(VerifyError::new(
                        tail.pc(),
                        VerifyErrorKind::UnsupportedExceptionRegion,
                    ));
                };
                *caught = SlotKind::Tagged;
                incoming.catches.pop();
                budget.charge(*successor, incoming.cell_count())?;
                if let Some(expected) = block_entries.get_mut(successor) {
                    if expected.stack != incoming.stack || expected.catches != incoming.catches {
                        return Err(VerifyError::new(
                            *successor,
                            VerifyErrorKind::UnsupportedExceptionRegion,
                        ));
                    }
                    if join_handler_locals(*successor, expected, &incoming.locals)? {
                        queue.push_back(*successor);
                    }
                } else {
                    block_entries.insert(*successor, incoming);
                    queue.push_back(*successor);
                }
                continue;
            }
            let mut incoming = state.clone();
            if cfg.is_loop_header(*successor) {
                // Widen locals before publishing into a loop header so a later
                // backedge does not look like an incompatible merge.
                for local in &mut incoming.locals {
                    if matches!(*local, SlotKind::Int32 | SlotKind::Float64) {
                        *local = SlotKind::Tagged;
                    }
                }
            }
            if let Some(expected) = block_entries.get_mut(successor) {
                budget.charge(*successor, incoming.cell_count())?;
                if merge_state(*successor, expected, &incoming)? {
                    queue.push_back(*successor);
                }
            } else {
                budget.charge(*successor, incoming.cell_count())?;
                block_entries.insert(*successor, incoming);
                queue.push_back(*successor);
            }
        }
    }
    Ok(StateProof {
        before,
        visited,
        handlers,
        heights,
        local_kinds,
    })
}

/// Joins the locals observed at one exceptional edge into a handler entry. A
/// handler is reached from every instruction of its try region, so an
/// initialized/uninitialized disagreement conservatively becomes `Tagged`.
fn join_handler_locals(
    pc: u32,
    expected: &mut AbstractState,
    actual: &[SlotKind],
) -> Result<bool, VerifyError> {
    if expected.locals.len() != actual.len() {
        return Err(VerifyError::new(
            pc,
            VerifyErrorKind::UnsupportedExceptionRegion,
        ));
    }
    let mut changed = false;
    for (expected, actual) in expected.locals.iter_mut().zip(actual) {
        if expected == actual {
            continue;
        }
        if matches!(*expected, SlotKind::CatchOffset) || matches!(*actual, SlotKind::CatchOffset) {
            return Err(VerifyError::new(
                pc,
                VerifyErrorKind::UnsupportedExceptionRegion,
            ));
        }
        if *expected != SlotKind::Tagged {
            *expected = SlotKind::Tagged;
            changed = true;
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Locked because it is the whole reason short-circuit/`??`/ternary joins
    /// with a literal arm (`x || 5`, `x ?? 7`, `c ? x : 7`) verify at all: the
    /// pair differs only in a proof, so the join is `Tagged` — no conversion.
    #[test]
    fn join_kinds_widens_proofs_and_rejects_catch_offsets() {
        for kind in [SlotKind::Int32, SlotKind::Float64, SlotKind::Uninitialized] {
            assert_eq!(join_kinds(SlotKind::Tagged, kind), Some(SlotKind::Tagged));
            assert_eq!(join_kinds(kind, SlotKind::Tagged), Some(SlotKind::Tagged));
        }
        assert_eq!(
            join_kinds(SlotKind::Int32, SlotKind::Float64),
            Some(SlotKind::Tagged)
        );
        assert_eq!(
            join_kinds(SlotKind::Float64, SlotKind::Int32),
            Some(SlotKind::Tagged)
        );
        assert_eq!(
            join_kinds(SlotKind::Uninitialized, SlotKind::Int32),
            Some(SlotKind::Tagged)
        );
        assert_eq!(
            join_kinds(SlotKind::Float64, SlotKind::Uninitialized),
            Some(SlotKind::Tagged)
        );
        // Structural: a merge may not move a catch offset or turn one into a
        // JS value (the unwinder truncates the stack to it).
        for kind in [
            SlotKind::Tagged,
            SlotKind::Int32,
            SlotKind::Float64,
            SlotKind::Uninitialized,
        ] {
            assert_eq!(join_kinds(SlotKind::CatchOffset, kind), None);
            assert_eq!(join_kinds(kind, SlotKind::CatchOffset), None);
        }
    }
}
