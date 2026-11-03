// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Pre-lowering scans over the verified bytecode: OSR points, carried
//! slots, dead stores, call sites and literal windows.

use super::*;

/// Pc values the compiled entry can be started at. 0 is the function entry;
/// loop headers (hot-loop OSR) and the pc right after each call are the OSR
/// entry points. After a call, the trampoline re-enters this frame at the
/// post-call pc, so the function keeps running natively instead of staying in
/// the interpreter.
pub(super) fn collect_osr_pcs(verified: &VerifiedFunction) -> Vec<u32> {
    let mut pcs: Vec<u32> = verified
        .osr_points()
        .iter()
        .map(|point| point.pc())
        .collect();
    let instructions = verified.instructions();
    let cfg = verified.control_flow_graph();
    let mut budget = OSR_TAIL_BUDGET;
    let mut seen_call = false;
    for block in cfg.blocks() {
        let range = block.instruction_range();
        for index in range.start..range.end {
            let instruction = &instructions[index];
            if !matches!(
                instruction.name(),
                "call" | "call_method" | "call_constructor"
            ) {
                continue;
            }
            // A call can sit in dead code (after a `throw`, in a finally chain).
            // Its resume pc has no proven stack height and no lowering range, so
            // `carried_sets` gives it no entry height — an OSR entry for it would
            // reach `edge_args` without one. The interpreter can never resume
            // there, so drop it. (Same two predicates `carried_sets` uses; keep
            // them in sync or the prologue asserts in debug and mis-sizes the
            // edge in release.)
            let resume_pc = instruction.pc() + instruction.size() as u32;
            if !verified.is_reachable(resume_pc)
                || block_range(instructions, cfg, resume_pc).is_none()
            {
                continue;
            }
            // A mid-block entry is not free: `block_range` hands it the whole
            // tail of its block and `block_for` gives that tail a block of its
            // own, so every copy stays in the compiled code. A block with N
            // calls would then cost O(N x block length) — enough to push a long
            // straight-line function past Cranelift's `CodeTooLarge` and cost it
            // native code altogether. The first call in a block always earns an
            // entry (it hands back the most work); the rest only while the tail
            // they duplicate fits the per-function budget. A dropped call site
            // resumes in the interpreter, which `invoke_native` already does for
            // a pc it has no entry for.
            let tail = range.end - index - 1;
            if seen_call && tail > budget {
                break;
            }
            seen_call = true;
            budget = budget.saturating_sub(tail);
            pcs.push(resume_pc);
        }
    }
    pcs.sort_unstable();
    pcs.dedup();
    pcs
}

/// How many instructions post-call OSR entries may duplicate in one function.
///
/// Generous next to what real functions need (a call's tail is usually a handful
/// of instructions) and small enough that the worst case stays linear in the
/// function's length.
const OSR_TAIL_BUDGET: usize = 256;

/// Speculative fast path: did type feedback observe *only* `Int` at this site? Feedback is
/// sorted by pc, so a binary search answers it. `check_rhs` is false for
/// single-value sites (`inc_loc`/`dec_loc`), whose `rhs` slot stays 0.
///
/// Result classes: the *result* class must be `Int` too. The inline int
/// path bails to the helper on `i32` overflow, so a site whose result is always
/// a `Float64` (an accumulator that overflows every trip) would pay that bail
/// every iteration — speculation would lose to the plain helper. The result
/// class lets the gate refuse exactly those sites.
pub(super) fn int_only(feedback: &[(u32, SiteFeedback)], pc: u32, check_rhs: bool) -> bool {
    feedback
        .binary_search_by_key(&pc, |&(site, _)| site)
        .is_ok_and(|i| {
            let site = feedback[i].1;
            site.lhs == CLASS_INT
                && (!check_rhs || site.rhs == CLASS_INT)
                && site.result == CLASS_INT
        })
}

/// What one instruction does to the slots below its entry height before it can
/// observe the frame (the stack-slot register cache). Mirrors `lower_insn`'s dispatch order; a
/// misclassification only costs the carry, never correctness — every edge
/// materializes what it does not pass (see [`State::edge_args`]).
enum StackEffect {
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
fn stack_effect(
    name: &str,
    h: u32,
    strict: bool,
    feedback: &[(u32, SiteFeedback)],
    pc: u32,
) -> StackEffect {
    if fast_binary(name).is_some() {
        // The operands are read first; `flush_except` then spares them.
        return StackEffect::ReadFlush(vec![h - 1, h]);
    }
    if helper_id(name).is_some() || shuffle_outputs(name).is_some() {
        return StackEffect::Flush;
    }
    match name {
        "nop"
        | "push_i32"
        | "push_const"
        | "undefined"
        | "null"
        | "push_false"
        | "push_true"
        | "catch"
        | "get_loc"
        | "get_arg"
        | "drop"
        | "goto"
        | "gosub"
        | "set_loc_uninitialized" => StackEffect::Pass,
        "push_this" if strict => StackEffect::Pass,
        // Reads the operand in a register and rewrites the slot from it.
        "is_undefined_or_null" => StackEffect::Read(h),
        "put_loc" | "put_arg" | "set_loc" | "set_arg" => StackEffect::Read(h),
        // Specialized `add_loc` reads the operand before `flush_except([top])`.
        "add_loc" if int_only(feedback, pc, true) => StackEffect::ReadFlush(vec![h]),
        // Both variants flush before anything reads the stack (`inc_loc`
        // reads the local out of the frame, not out of a cache entry).
        "add_loc" | "inc_loc" | "dec_loc" => StackEffect::Flush,
        "if_true" | "if_false" | "return" | "ret" => StackEffect::ReadFlush(vec![h]),
        // `rest`/`special_object`/`fclosure` call helpers; `get_loc_check`
        // flushes before its TDZ split. Everything else is a `Resume` exit
        // (nothing survives it either).
        "rest" | "special_object" | "fclosure" | "get_loc_check" => StackEffect::Flush,
        _ => StackEffect::Flush,
    }
}

/// Cross-block carry: for every block, the operand-stack slots its code reads
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
pub(super) fn block_carry_facts(
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
            match stack_effect(instruction.name(), h, strict, feedback, instruction.pc()) {
                StackEffect::Pass => {}
                StackEffect::Read(k) => mark(&mut marked, entry_h, k),
                StackEffect::ReadFlush(slots) => {
                    for k in slots {
                        mark(&mut marked, entry_h, k);
                    }
                    break;
                }
                StackEffect::Flush => break,
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

pub(super) fn mark(marked: &mut [bool], entry_h: u16, k: u32) {
    if (1..=u32::from(entry_h)).contains(&k) {
        marked[(k - 1) as usize] = true;
    }
}

/// Carried locals: the locals/arguments each block reads before writing, sorted
/// so the parameter order is stable. The entry block has no incoming edge, and
/// aliased slots are skipped for the same reason the local cache is disabled for
/// them (`cacheable_slots`: a `VarRef` can rewrite the frame out of sight).
pub(super) fn carried_local_sets(
    analysis: &Analysis,
    verified: &VerifiedFunction,
    locals_cacheable: bool,
    args_cacheable: bool,
) -> HashMap<u32, Vec<LocalRef>> {
    let mut carried = HashMap::new();
    for block in analysis.blocks() {
        if block.start_pc == 0 || !verified.is_reachable(block.start_pc) {
            continue;
        }
        let mut slots: Vec<LocalRef> = block
            .uses
            .iter()
            .copied()
            .filter(|slot| {
                if slot.is_arg {
                    args_cacheable
                } else {
                    locals_cacheable
                }
            })
            .collect();
        slots.sort_unstable();
        slots.dedup();
        if !slots.is_empty() {
            carried.insert(block.start_pc, slots);
        }
    }
    carried
}

/// Dead-store elimination: the set of `put_loc`/`set_loc` pcs whose store the lowering
/// should skip (see [`Analysis::dead_local_stores`]). `gate` is the caller's
/// "the liveness is trustworthy for a skipped write": resume-free,
/// exception-free and with no way to alias a frame slot.
pub(super) fn dead_local_stores(
    analysis: &Analysis,
    instructions: &[Instruction],
    gate: bool,
) -> HashSet<u32> {
    analysis
        .dead_local_stores(instructions, gate)
        .into_iter()
        .collect()
}
#[allow(clippy::too_many_arguments)]
/// The monomorphic call sites (the direct-call path): the opcode offset -> the site id
/// the compiled code carries in `imm2`.
///
/// A site is monomorphic when the interpreter only ever saw one callee there.
/// The site id is the site's index in the snapshot's table — the same index
/// the runtime's table uses, so the helper reads the expected callee back
/// through it.
pub(super) fn monomorphic_call_sites(verified: &VerifiedFunction) -> HashMap<u32, u32> {
    verified
        .snapshot()
        .call_sites
        .iter()
        .enumerate()
        .filter(|(_, site)| !site.polymorphic && site.callee != 0)
        .map(|(id, site)| (site.pc, id as u32))
        .collect()
}

/// The call sites that only ever called a non-bytecode callee. `call_method`
/// there may be `Function.prototype.apply`; its helper decides at run time.
pub(super) fn native_call_sites(verified: &VerifiedFunction) -> HashSet<u32> {
    verified
        .snapshot()
        .call_sites
        .iter()
        .filter(|site| !site.polymorphic && site.callee == 0)
        .map(|site| site.pc)
        .collect()
}

/// The monomorphic sites whose callee the compiler can copy into the caller
/// (the inlined call path). The callee's snapshot was taken while the runtime
/// thread held the bytecode; verify it here, off-thread, exactly like any
/// other function, and keep only bodies inside the inline subset.
pub(super) fn inline_call_sites(
    verified: &VerifiedFunction,
) -> HashMap<u32, crate::jit::inline_call::InlinePlan> {
    let snapshot = verified.snapshot();
    let mut sites = HashMap::new();
    for site in snapshot.call_sites.iter() {
        let Some(index) = site.inline else {
            continue;
        };
        let Some(callee) = snapshot.inline_callees.get(index as usize) else {
            continue;
        };
        if let Some(plan) = build_inline_plan(callee, site.callee) {
            sites.insert(site.pc, plan);
        }
    }
    sites
}

/// Verify one callee and its own monomorphic callees. A nested site
/// that cannot be planned simply stays out of `sites`, which makes the
/// enclosing body ineligible at its `call` opcode.
fn build_inline_plan(
    callee: &InlineCallee,
    identity: u64,
) -> Option<crate::jit::inline_call::InlinePlan> {
    let body = verify(callee.snapshot(identity), VerifyLimits::default()).ok()?;
    let snapshot = body.snapshot();
    let mut sites = HashMap::new();
    for site in snapshot.call_sites.iter() {
        let Some(index) = site.inline else {
            continue;
        };
        let Some(nested) = snapshot.inline_callees.get(index as usize) else {
            continue;
        };
        if let Some(plan) = build_inline_plan(nested, site.callee) {
            sites.insert(site.pc, plan);
        }
    }
    crate::jit::inline_call::eligible(&body, &sites).then_some(
        crate::jit::inline_call::InlinePlan {
            body,
            callee: identity,
            sites,
        },
    )
}

/// The merged object literal: the `object` seed plus the run of
/// (simple value push, `define_field`) pairs that follow it **inside one
/// basic block**, keyed by the `object` pc.
///
/// A window is only merged when every field's value is a pure stack push that
/// can neither throw nor call a helper (`push_i32`/`push_const`/`get_loc`/
/// `get_arg`/`undefined`/`null`) — that is what makes it safe to defer all
/// `define_field`s to the end: nothing between the pushes can observe the
/// partially built literal, and the helper call's flush roots every value.
/// A duplicate atom (`{a:1, a:2}`, an update rather than an append), a window
/// longer than [`LIT_ATOM_MAX`], or a run that would exceed the frame's
/// operand stack stops the merge; the per-op path below then handles it.
/// A merged object-literal window (see [`literal_windows`]).
#[derive(Clone)]
pub(super) struct LiteralWindow {
    /// The field atoms, in source order (`imm` carries the count).
    pub(super) atoms: Vec<u32>,
    /// The instruction indices (into the verified stream) of the N value
    /// pushes, in source order — the merged lowering emits them itself.
    pub(super) value_indices: Vec<usize>,
    /// The pc just past the window's last `define_field` — where the merged
    /// helper reports on failure and where the driver resumes lowering.
    pub(super) end_pc: u32,
}

/// Whether an opcode is one of the pure value pushes a merged literal window
/// accepts as a field value. Each pushes exactly one slot and cannot exit.
fn is_literal_push(name: &str) -> bool {
    matches!(
        name,
        "push_i32" | "push_const" | "get_loc" | "get_arg" | "undefined" | "null"
    )
}

pub(super) fn literal_windows(
    verified: &VerifiedFunction,
    osr_pcs: &[u32],
) -> HashMap<u32, LiteralWindow> {
    let instructions = verified.instructions();
    let heights = verified.stack_heights();
    let stack_size = usize::from(verified.snapshot().stack_size);
    let mut out = HashMap::new();
    for block in verified.control_flow_graph().blocks() {
        let range = block.instruction_range();
        let mut i = range.start;
        while i < range.end {
            if instructions[i].name() != "object" {
                i += 1;
                continue;
            }
            let start_height = usize::from(heights[i]);
            let mut atoms: Vec<u32> = Vec::new();
            let mut value_indices: Vec<usize> = Vec::new();
            let mut j = i + 1;
            while atoms.len() < LIT_ATOM_MAX && j + 1 < range.end {
                let value = &instructions[j];
                let define = &instructions[j + 1];
                if !is_literal_push(value.name()) || define.name() != "define_field" {
                    break;
                }
                // An OSR resume point inside the window would re-enter with the
                // un-merged stack shape, so the run stops before one.
                if osr_pcs.contains(&value.pc()) || osr_pcs.contains(&define.pc()) {
                    break;
                }
                let atom = define.operand_u32(1);
                if atoms.contains(&atom) {
                    break;
                }
                atoms.push(atom);
                value_indices.push(j);
                j += 2;
            }
            // The merged window stacks all N values at once where the original
            // only ever held one, so it needs the frame's headroom for it.
            if !atoms.is_empty() && start_height + atoms.len() <= stack_size {
                let last = &instructions[j - 1];
                out.insert(
                    instructions[i].pc(),
                    LiteralWindow {
                        atoms,
                        value_indices,
                        end_pc: last.pc() + last.size() as u32,
                    },
                );
            }
            // The run (merged or not) is consumed: a later `object` cannot
            // pair with a `define_field` already claimed here.
            i = j;
        }
    }
    out
}
