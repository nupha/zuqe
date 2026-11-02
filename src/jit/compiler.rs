// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Baseline (Tier1) lowering: verified bytecode straight to Cranelift IR.
//! There is no separate IR layer: the lowering reads `VerifiedFunction`
//! directly, and the function-scoped facts it consults live in
//! [`super::analysis`] — which only answers questions, never carries values.
//!
//! Values live in the frame buffer, exactly where the interpreter keeps them —
//! with one exception, the stack-slot register cache: inside a basic block a
//! stack slot may live only in a register, written back before anything can
//! observe the frame (helper calls, exits, branch terminators, end of block).
//! Across a block edge the successor's carried slots travel as block
//! parameters instead of a store/load pair ([`block_carry_facts`]). Every
//! observation point flushes first, so the frame stays authoritative and an
//! exit needs no pc-keyed deopt map — materializing *is* the flush, keyed by
//! the verifier's proven stack heights. The operand-stack pointer is an SSA
//! value written back to `StackFrame.cur_sp_offset` before any helper call or
//! exit, the only moments a collection can observe it. Anything the baseline
//! cannot translate terminates the native run with `Resume`, and the
//! interpreter picks up at that bytecode offset.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    mem::offset_of,
};

use cranelift_codegen::{
    Context,
    control::ControlPlane,
    ir::JumpTableData,
    ir::{
        AbiParam, Block, InstBuilder, MemFlags, Signature, UserFuncName, Value,
        condcodes::{FloatCC, IntCC},
        types,
    },
    isa::TargetIsa,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_FLOAT64_ARRAY, JS_CLASS_UINT8C_ARRAY,
    jit::snapshot::{ConstMeta, InlineCallee},
    vm_stack::ExecMode,
};

/// The inline-cache site's layout: the inline property guard reads
/// the site's first way.
#[cfg(feature = "inline_cache")]
use crate::function::jit_layout as ic_layout;
use crate::object::{
    JS_PROP_LENGTH, JS_PROP_NORMAL, JS_PROP_TMASK, JS_PROP_WRITABLE, JSProperty, PROP_TAG_NORMAL,
};

use super::{
    abi::{
        EXIT_DONE, EXIT_RESUME, EXIT_RETRY, FLOAT64_ADDEND, GLOBAL_CACHE_LEXICAL, HELPER_ADD,
        HELPER_ADD_BRAND, HELPER_ADD_LOC, HELPER_AND, HELPER_APPEND, HELPER_APPLY,
        HELPER_ARRAY_FROM, HELPER_CALL, HELPER_CALL_APPLY, HELPER_CALL_CONSTRUCTOR,
        HELPER_CALL_METHOD, HELPER_CALL_METHOD_MONO, HELPER_CALL_MONO, HELPER_CHECK_BRAND,
        HELPER_CHECK_CTOR, HELPER_CHECK_CTOR_RETURN, HELPER_CLOSE_LOC, HELPER_COPY_DATA_PROPERTIES,
        HELPER_DEC, HELPER_DEFINE_ARRAY_EL, HELPER_DEFINE_CLASS, HELPER_DEFINE_CLASS_COMPUTED,
        HELPER_DEFINE_FIELD, HELPER_DEFINE_METHOD, HELPER_DEFINE_METHOD_COMPUTED,
        HELPER_DEFINE_PRIVATE_FIELD, HELPER_DELETE, HELPER_DELETE_VAR, HELPER_DIV, HELPER_EQ,
        HELPER_FOR_IN_NEXT, HELPER_FOR_IN_START, HELPER_FOR_OF_NEXT, HELPER_FOR_OF_START,
        HELPER_GC_BIND, HELPER_GET_ARRAY_EL, HELPER_GET_ARRAY_EL2, HELPER_GET_ARRAY_EL3,
        HELPER_GET_FIELD, HELPER_GET_FIELD2, HELPER_GET_PRIVATE_FIELD, HELPER_GET_REF_VALUE,
        HELPER_GET_SUPER, HELPER_GET_SUPER_VALUE, HELPER_GET_VAR, HELPER_GET_VAR_REF,
        HELPER_GET_VAR_REF_CHECK, HELPER_GET_VAR_UNDEF, HELPER_GT, HELPER_GTE, HELPER_IN,
        HELPER_INC, HELPER_INIT_CTOR, HELPER_INLINE_GUARD, HELPER_INSTANCEOF, HELPER_ITERATOR_CALL,
        HELPER_ITERATOR_CHECK_OBJECT, HELPER_ITERATOR_CLOSE, HELPER_ITERATOR_GET_VALUE_DONE,
        HELPER_ITERATOR_NEXT, HELPER_LNOT, HELPER_LT, HELPER_LTE, HELPER_MAKE_ARG_REF,
        HELPER_MAKE_LOC_REF, HELPER_MAKE_VAR_REF, HELPER_MAKE_VAR_REF_REF, HELPER_MOD, HELPER_MUL,
        HELPER_NEG, HELPER_NEQ, HELPER_NIP_CATCH, HELPER_NOT, HELPER_OBJECT, HELPER_OBJECT_LITERAL,
        HELPER_OK, HELPER_OR, HELPER_PLUS, HELPER_POST_DEC, HELPER_POST_INC, HELPER_POW,
        HELPER_PRIVATE_IN, HELPER_PRIVATE_SYMBOL, HELPER_PUSH_BIGINT_I32, HELPER_PUT_ARRAY_EL,
        HELPER_PUT_FIELD, HELPER_PUT_PRIVATE_FIELD, HELPER_PUT_REF_VALUE, HELPER_PUT_SUPER_VALUE,
        HELPER_PUT_VAR, HELPER_PUT_VAR_INIT, HELPER_PUT_VAR_REF, HELPER_PUT_VAR_REF_CHECK,
        HELPER_PUT_VAR_REF_CHECK_INIT, HELPER_REGEXP, HELPER_SAFEPOINT, HELPER_SAR,
        HELPER_SET_HOME_OBJECT, HELPER_SET_NAME_COMPUTED, HELPER_SET_PROTO, HELPER_SET_VAR_REF,
        HELPER_SHL, HELPER_SHR, HELPER_STRICT_EQ, HELPER_STRICT_NEQ, HELPER_SUB, HELPER_THROW,
        HELPER_THROW_ERROR, HELPER_TO_BOOL, HELPER_TO_OBJECT, HELPER_TO_PROPKEY, HELPER_TYPEOF,
        HELPER_WITH_DELETE_VAR, HELPER_WITH_GET_REF, HELPER_WITH_GET_VAR, HELPER_WITH_MAKE_REF,
        HELPER_WITH_MISS, HELPER_WITH_PUT_VAR, HELPER_XOR, INLINE_METHOD_BIT, JitExecFrame,
        LIT_ATOM_MAX, SPEC_MISS_IMM2,
    },
    analysis::{Analysis, LocalRef, RedundantExpressions},
    bytecode::{
        VerifyLimits, cfg::ControlFlowGraph, decode::Instruction, stack::SlotKind, verify,
        verify::VerifiedFunction,
    },
    error::JitError,
    feedback::{CLASS_INT, SiteFeedback},
    layout::{
        array as array_layout, array_buffer as buffer_layout, global_var as gv_layout,
        object as jit_layout, typed_array as ta_layout,
    },
    runtime::Tier,
};

/// The IR/code dump, only under `jit_dump` (see [`super::dump`]).
#[cfg(feature = "jit_dump")]
use super::dump;
mod opcode_tables;

pub(crate) use opcode_tables::is_cse_expression;
pub use opcode_tables::shuffle_opcodes;
use opcode_tables::{FastOp, fast_binary, helper_id, lowers, shuffle_outputs};
mod state;
use state::*;

mod emit;
use emit::*;

mod prep;
use prep::*;

mod numeric;
pub(in crate::jit) use numeric::*;

mod control;
use control::*;

mod guards;
use guards::*;

const UNDEFINED_BITS: i64 = 3 << 48;
/// `JSValueTag::Uninitialized` (the TDZ sentinel written by
/// `set_loc_uninitialized`).
const UNINITIALIZED_BITS: i64 = 4 << 48;
const BOOL_TAG: i64 = 1 << 48;
/// `JSValueTag::Null`/`Undefined`, i.e. the high 16 bits of `NULL`/`UNDEFINED`.
const NULL_TAG: i64 = 2;
const UNDEFINED_TAG: i64 = 3;
/// `JS_NAN_U64` (`0x7ff8… - (0xFFF9 << 48)`): the canonical quiet NaN
/// `js_new_float64` substitutes for every NaN payload. The float fast path
/// must box through it, because the differential suite compares bit-for-bit.
const JS_NAN_U64_BITS: i64 = 0x7FFF_0000_0000_0000;
/// The lowest raw tag that carries a GC node pointer (`Symbol` = -8 ..
/// `Object` = -1, i.e. 0xfff8..=0xffff). A value with such a tag needs the
/// collector's write barrier when it is stored (the property inline guard).
const GC_NODE_TAG: i64 = 0xFFF8;
/// The write guard reads the class id and the fast-array bit in one word
/// (the property inline guard): a fast plain `Array` is `fast | Array`, a fast `arguments` object
/// is `fast | Arguments`.
const WRITE_CLASS_MASK: i64 = jit_layout::CLASS_ID_MASK | jit_layout::FAST_ARRAY_MASK;
/// The masked flags word of a fast plain `Array`.
const WRITE_ARRAY_FLAGS: i64 = JS_CLASS_ARRAY as i64 | jit_layout::FAST_ARRAY_MASK;
/// The masked flags word of a fast `arguments` object.
const WRITE_ARGUMENTS_FLAGS: i64 = JS_CLASS_ARGUMENTS as i64 | jit_layout::FAST_ARRAY_MASK;
/// The bit the extensible flag sits at (the guard shifts it out).
const WRITE_EXTENSIBLE_SHIFT: i32 = jit_layout::EXTENSIBLE_MASK.trailing_zeros() as i32;

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
/// The merged object literal's atoms, stored one `u32` each right before
/// the `h_object_literal` call.
const O_LIT_ATOMS: i32 = offset_of!(JitExecFrame, lit_atoms) as i32;
const O_PC: i32 = offset_of!(JitExecFrame, pc) as i32;
const O_OUT_VALUE: i32 = offset_of!(JitExecFrame, out_value) as i32;
const O_OUT_PC: i32 = offset_of!(JitExecFrame, out_pc) as i32;
const O_OUT_SP: i32 = offset_of!(JitExecFrame, out_sp) as i32;
/// The inline-cache sites. Only with the `inline_cache` feature.
#[cfg(feature = "inline_cache")]
const O_IC_SITES: i32 = offset_of!(JitExecFrame, ic_sites) as i32;
/// The property guard's hit counter.
#[cfg(feature = "inline_cache")]
const O_PROPERTY_HITS: i32 = offset_of!(JitExecFrame, property_guard_hits) as i32;
/// The property guard's miss counter.
#[cfg(feature = "inline_cache")]
const O_PROPERTY_MISSES: i32 = offset_of!(JitExecFrame, property_guard_misses) as i32;
/// The global-variable guard's counters.
const O_GLOBAL_GUARD_HITS: i32 = offset_of!(JitExecFrame, global_guard_hits) as i32;
const O_GLOBAL_GUARD_MISSES: i32 = offset_of!(JitExecFrame, global_guard_misses) as i32;
/// The global objects and the global-variable site caches.
const O_GLOBAL_OBJ: i32 = offset_of!(JitExecFrame, global_obj) as i32;
const O_GLOBAL_VAR_OBJ: i32 = offset_of!(JitExecFrame, global_var_obj) as i32;
const O_GLOBAL_CACHE: i32 = offset_of!(JitExecFrame, global_var_cache) as i32;

/// Whether the baseline can translate `verified`'s first opcode. When false,
/// [`compile`] refuses deterministically, so callers should settle the function
/// fail-closed rather than retry.
///
/// Every opcode a *verified* function can contain is lowered today — the
/// only unsupported names are policy `Reject`, which `verify` refuses before
/// compilation. The check stays as the fail-closed net for a future `Helper`
/// with no lowering.
pub(crate) fn entry_is_translatable(verified: &VerifiedFunction) -> bool {
    verified
        .instructions()
        .first()
        .is_some_and(|instruction| lowers(instruction.name()))
}

/// Compile one verified function. Returns code bytes for the caller to publish.
///
/// `specialized_ops` accumulates how many sites got a tier-2 speculative fast
/// path (the speculative tier), so the caller can report it in metrics.
///
/// `tier` names the tier being compiled; it only feeds the `jit_dump` file
/// names (`<function-id>-<tier>.clif/.bin`).
pub(crate) fn compile(
    verified: &VerifiedFunction,
    isa: &dyn TargetIsa,
    specialized_ops: &Cell<u64>,
    #[cfg_attr(not(feature = "jit_dump"), allow(unused_variables))] tier: Tier,
) -> Result<Box<[u8]>, JitError> {
    let instructions = verified.instructions();
    let Some(first) = instructions.first() else {
        return Err(JitError::Cranelift("empty function".into()));
    };
    // A function whose very first op is unsupported would resume on every
    // entry; don't install it.
    if !lowers(first.name()) {
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
        global_obj: b
            .ins()
            .load(types::I64, MemFlags::new(), frame, O_GLOBAL_OBJ),
        global_var_obj: b
            .ins()
            .load(types::I64, MemFlags::new(), frame, O_GLOBAL_VAR_OBJ),
        global_cache: b
            .ins()
            .load(types::I64, MemFlags::new(), frame, O_GLOBAL_CACHE),
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

    // Format the IR now: codegen may rewrite the function. Compiles to nothing
    // unless `jit_dump` is on *and* `JIT_DUMP` was set at attach.
    #[cfg(feature = "jit_dump")]
    let ir_dump = dump::Dump::begin(verified.snapshot().function_id, tier, &cgen.func);

    let mut ctrl = ControlPlane::default();
    let compiled = cgen
        .compile(isa, &mut ctrl)
        .map_err(|e| JitError::Cranelift(format!("{e:?}")))?;
    if !compiled.buffer.relocs().is_empty() {
        // No external symbol is referenced, so a reloc means codegen emitted
        // something we cannot relocate: fail closed.
        return Err(JitError::Cranelift("unexpected relocations".into()));
    }

    #[cfg(feature = "jit_dump")]
    ir_dump.finish(compiled.code_buffer());

    Ok(compiled.code_buffer().into())
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
        // Cross-block carry: the interpreter left every slot in the frame, so the
        // carried parameters are frame loads at the proven entry height.
        let args = state
            .edge_args(b, fv, sp_osr, &[(target, pc_val)])
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
    let (carried, entry_heights) = block_carry_facts(verified, osr_pcs, strict);
    let (locals_cacheable, args_cacheable) = cacheable_slots(verified);
    // Carried locals: the function-scoped analysis supplies which locals each
    // block reads before writing, so they can cross the edges as parameters.
    let resume_free = instructions.iter().all(|i| lowers(i.name()));
    let analysis = Analysis::build(verified, resume_free);
    let carried_locals = carried_local_sets(&analysis, verified, locals_cacheable, args_cacheable);
    let dead_stores = dead_local_stores(
        &analysis,
        instructions,
        resume_free && locals_cacheable && args_cacheable,
    );
    let mut state = State {
        blocks: HashMap::new(),
        pending: Vec::new(),
        feedback: &verified.snapshot().feedback,
        ic_count: verified.snapshot().ic_count,
        global_cache_count: verified.snapshot().global_cache_count,
        specialized_ops: specialized_ops,
        allow_speculation: verified.snapshot().allow_speculation,
        stack: StackCache::default(),
        carried,
        carried_locals,
        entry_heights,
        dead_stores,
        cse: RedundantExpressions::build(
            verified,
            locals_cacheable && args_cacheable && !verified.has_exception_regions(),
        ),
        cse_regs: HashMap::new(),
        locals: LocalCache::new(locals_cacheable, args_cacheable),
        const_meta: &verified.snapshot().const_meta,
        barrier_sig: barrier_sig(b, helper_sig),
        direct_call_sites: monomorphic_call_sites(verified),
        inline_sites: inline_call_sites(verified),
        builtin_apply_sites: native_call_sites(verified),
        inline_tag: 0,
        lit_windows: literal_windows(verified, osr_pcs),
        skip_until_pc: 0,
    };
    let entry_block = state.block_for(b, 0);
    if osr_pcs.is_empty() {
        // The entry block has no incoming edge, so it carries nothing.
        debug_assert!(state.carried_for(0).is_empty());
        debug_assert!(state.carried_locals_for(0).is_empty());
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
        // a merged window never crosses a block, and blocks lower in
        // worklist order (their pcs are not monotonic), so the skip bound is
        // local to the block that set it.
        state.skip_until_pc = 0;
        // Cross-block carry: slots carried across the incoming edges arrive as
        // block parameters — re-seed the model from them. The model itself
        // never crosses a block; every edge delivers it through `edge_args`.
        state.stack.entries.clear();
        // Local copies are block-scoped for the same reason: the register only
        // dominates this block, and the interpreter owns the frame for the rest.
        state.locals.clear();
        // Reused-value registers are block-scoped too (the pass plans them per
        // block).
        state.cse_regs.clear();
        let carried: Vec<u32> = state.carried_for(start_pc).to_vec();
        let carried_locals: Vec<LocalRef> = state.carried_locals_for(start_pc).to_vec();
        let params = b.block_params(cranelift_block);
        debug_assert_eq!(
            params.len(),
            carried.len() + carried_locals.len(),
            "block {start_pc} parameters"
        );
        for (&k, &v) in carried.iter().zip(&params[..carried.len()]) {
            state.stack.entries.insert(k, CachedSlot::unknown(v));
        }
        // Carried locals: re-seed the local cache from the carried parameters —
        // the successor's first read of a live-in local is a register now.
        for (slot, &v) in carried_locals.iter().zip(&params[carried.len()..]) {
            state.locals.fill(slot.is_arg, slot.idx, v);
        }
        let mut sp = b.use_var(sp_var);
        let mut stopped = false;

        let base = range.start;
        let total = range.end - base;
        for (offset, insn) in instructions[range].iter().enumerate() {
            // instructions already covered by a merged literal window
            // (lowered at its `object`) are not lowered again.
            if insn.pc() < state.skip_until_pc {
                continue;
            }
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
                verified,
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
                .edge_args(b, fv, sp, &[(next_block, next)])
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
        // Local copies are not carried either: they are block-scoped, so drop
        // them here (the next block starts empty anyway).
        state.locals.clear();
        state.cse_regs.clear();
        debug_assert!(
            state.locals.is_empty(),
            "a block ended with register-resident local slots"
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
    verified: &VerifiedFunction,
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

    // Frame-entry ops that need the current interpreter activation
    // (`special_object`, `fclosure`) and sloppy `this` go through helpers that
    // read the JIT frame's `arena`/`cur_frame_idx`/`new_target`/`home_obj`.
    match name {
        "special_object" => {
            let id = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), id, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                super::abi::HELPER_SPECIAL_OBJECT,
                pc + size,
            );
            return Step::Continue(sp);
        }
        "fclosure" => {
            let id = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), id, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                super::abi::HELPER_FCLOSURE,
                pc + size,
            );
            return Step::Continue(sp);
        }
        "set_name" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                super::abi::HELPER_SET_NAME,
                pc + size,
            );
            return Step::Continue(sp);
        }
        "push_this" if !strict => {
            // Sloppy `this`: the frame holds the raw value, so the helper does
            // the global-object substitution / objectification.
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                super::abi::HELPER_PUSH_THIS,
                pc + size,
            );
            return Step::Continue(sp);
        }
        // `for_of_next` reads its `depth` operand through `imm` (the iterator
        // window is `sp[-3-depth]`), like `add_loc` passes its local index.
        "for_of_next" => {
            let base = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), base, fv.frame, O_IMM);
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_FOR_OF_NEXT, pc + size);
            return Step::Continue(sp);
        }
        // `iterator_call` carries the close/throw flag bits in its operand.
        "iterator_call" => {
            let flags = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), flags, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                HELPER_ITERATOR_CALL,
                pc + size,
            );
            return Step::Continue(sp);
        }
        "iterator_close" => {
            // Drop `next` and the catch offset *before* the helper runs. The
            // interpreter does the same, so if `return()` throws, the unwinder
            // no longer sees this loop's own sentinel and the error propagates
            // instead of closing the same iterator twice.
            state.stack.flush(b, sp);
            let sp = b.ins().iadd_imm(sp, -16);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                HELPER_ITERATOR_CLOSE,
                pc + size,
            );
            return Step::Continue(sp);
        }
        // The four call opcodes. The helper performs the whole JS call on
        // the current frame through `call_internal`, so the caller never exits
        // natively; `imm` carries the argument count (`call`/`call_method`/
        // `call_constructor`) or `apply`'s magic. `call_stack_helper` flushes
        // the operand stack and reloads `sp` from the frame (the helper popped
        // the call window and pushed the result).
        //
        // The direct-call path: a *monomorphic* site (the interpreter only ever saw
        // one callee there) gets the direct-call helper: it enters that
        // callee's compiled code without the interpreter's call protocol. The
        // site id travels in `imm2`; the helper guards on the callee and falls
        // back to the general path when the guard misses.
        "call" | "call_method" | "call_constructor" | "apply" => {
            let operand = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), operand, fv.frame, O_IMM);
            let method = name == "call_method";
            let argc = u32::from(insn.operand_u16(1));

            // The inlined call path: a monomorphic site whose callee is inside
            // the inline subset runs that body here. The guard is the same
            // recorded-callee check the direct helper makes; a miss (or any
            // operand the subset cannot serve) takes the general path.
            // Take the plan out of the map so `state` stays mutable below (the
            // fallback path needs it); put it back before leaving this arm —
            // the OSR tail lowers the same site again.
            let inline_plan = if matches!(name, "call" | "call_method") {
                state.inline_sites.remove(&pc)
            } else {
                None
            };
            if let Some(callee) = inline_plan.as_ref()
                && let Some(&site_id) = state.direct_call_sites.get(&pc)
            {
                let encoded = if method {
                    site_id | INLINE_METHOD_BIT
                } else {
                    site_id
                };
                let site = b.ins().iconst(types::I32, i64::from(encoded));
                b.ins().store(MemFlags::new(), site, fv.frame, O_IMM2);

                flush_and_publish(state, b, fv, sp);
                let guard_fn = b.ins().load(
                    types::I64,
                    MemFlags::new(),
                    fv.helpers,
                    (HELPER_INLINE_GUARD * 8) as i32,
                );
                let call = b.ins().call_indirect(helper_sig, guard_fn, &[fv.frame]);
                let guard = b.inst_results(call)[0];
                let hit = b.ins().icmp_imm(IntCC::NotEqual, guard, 0);

                let inline = b.create_block();
                let fallback = b.create_block();
                let merge = b.create_block();
                b.append_block_param(merge, types::I64);
                b.ins().brif(hit, inline, &[], fallback, &[]);

                // The general call path, also the inline body's bailout: the
                // frame and the call window are untouched up to here.
                b.switch_to_block(fallback);
                let general = if method {
                    HELPER_CALL_METHOD
                } else {
                    HELPER_CALL
                };
                let sp_fallback =
                    call_stack_helper(state, b, fv, helper_sig, sp, general, pc + size);
                b.def_var(sp_var, sp_fallback);
                b.ins().jump(merge, &[sp_fallback]);

                b.switch_to_block(inline);
                let window = if method {
                    i64::from(argc) + 2
                } else {
                    i64::from(argc) + 1
                };
                let base = b.ins().iadd_imm(sp, -(window * 8));
                // The callee's declared arguments, read from the call window
                // (missing ones are `undefined`). Nested copies reuse the
                // same lowering with register arguments.
                let arg_base = b.ins().iadd_imm(base, if method { 16 } else { 8 });
                let undefined = b.ins().iconst(types::I64, UNDEFINED_BITS);
                let declared = u32::from(callee.body.snapshot().arg_count);
                let mut inline_args = Vec::with_capacity(declared as usize);
                for index in 0..declared {
                    inline_args.push(if index < argc {
                        b.ins()
                            .load(types::I64, MemFlags::new(), arg_base, (index as i32) * 8)
                    } else {
                        undefined
                    });
                }
                let sp_after = b.ins().iadd_imm(base, 8);
                super::inline_call::lower(
                    b,
                    callee,
                    &inline_args,
                    Some((base, sp_after)),
                    &mut state.inline_tag,
                    inline,
                    fallback,
                    merge,
                    fv.frame,
                    fv.helpers,
                    helper_sig,
                );

                b.switch_to_block(merge);
                let sp_merge = b.block_params(merge)[0];
                b.def_var(sp_var, sp_merge);
                state
                    .inline_sites
                    .insert(pc, inline_plan.expect("the plan was just borrowed"));
                return Step::Continue(sp_merge);
            }
            if let Some(plan) = inline_plan {
                state.inline_sites.insert(pc, plan);
            }

            let id = match name {
                "call" => HELPER_CALL,
                "call_method" => HELPER_CALL_METHOD,
                "call_constructor" => HELPER_CALL_CONSTRUCTOR,
                _ => HELPER_APPLY,
            };
            let id = match state.direct_call_sites.get(&pc) {
                Some(&site_id) => {
                    let site = b.ins().iconst(types::I32, i64::from(site_id));
                    b.ins().store(MemFlags::new(), site, fv.frame, O_IMM2);
                    match name {
                        "call" => HELPER_CALL_MONO,
                        "call_method" => HELPER_CALL_METHOD_MONO,
                        _ => id,
                    }
                }
                // A native-callee site: `f.apply(thisArg, args)` lands here,
                // and the helper serves the builtin without the interpreter's
                // two call layers.
                None if name == "call_method" && state.builtin_apply_sites.contains(&pc) => {
                    HELPER_CALL_APPLY
                }
                None => id,
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            return Step::Continue(sp);
        }
        // `with_*`: the helper resolves `imm` (atom) on the environment object
        // with `imm2` as the `is_with` flag, reports whether it handled the name
        // and leaves the stack shaped for that answer. On a hit the opcode jumps
        // to its label (skipping the ordinary variable access the compiler
        // emitted right after it); on a miss it falls through to that access.
        "with_get_var" | "with_put_var" | "with_delete_var" | "with_make_ref" | "with_get_ref" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let is_with = b.ins().iconst(types::I32, i64::from(insn.operand_u8(9)));
            b.ins().store(MemFlags::new(), is_with, fv.frame, O_IMM2);
            let id = helper_id(name).expect("with_* helper");
            lower_with(state, b, fv, sp_var, helper_sig, sp, id, insn, pc + size);
            return Step::Stop;
        }
        // Class / object-literal ops whose operand is an immediate. The helper
        // reads it from `imm`/`imm2`; the stack window is the opcode's own.
        "define_field" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp =
                call_stack_helper(state, b, fv, helper_sig, sp, HELPER_DEFINE_FIELD, pc + size);
            return Step::Continue(sp);
        }
        "copy_data_properties" => {
            let mask = b.ins().iconst(types::I32, i64::from(insn.operand_u8(1)));
            b.ins().store(MemFlags::new(), mask, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                HELPER_COPY_DATA_PROPERTIES,
                pc + size,
            );
            return Step::Continue(sp);
        }
        "define_method" | "define_method_computed" => {
            let computed = name == "define_method_computed";
            // `define_method` passes atom (`imm`) and op_flags (`imm2`); the
            // computed form has no atom operand, so it keeps its flags in `imm`
            // and the key is read from the stack.
            let (imm_val, imm2_val) = if computed {
                (i64::from(insn.operand_u8(1)), 0)
            } else {
                (
                    i64::from(insn.operand_u32(1)),
                    i64::from(insn.operand_u8(5)),
                )
            };
            let imm = b.ins().iconst(types::I32, imm_val);
            b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
            let imm2 = b.ins().iconst(types::I32, imm2_val);
            b.ins().store(MemFlags::new(), imm2, fv.frame, O_IMM2);
            let id = if computed {
                HELPER_DEFINE_METHOD_COMPUTED
            } else {
                HELPER_DEFINE_METHOD
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            return Step::Continue(sp);
        }
        "define_class" | "define_class_computed" => {
            let computed = name == "define_class_computed";
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let flags = b.ins().iconst(types::I32, i64::from(insn.operand_u8(5)));
            b.ins().store(MemFlags::new(), flags, fv.frame, O_IMM2);
            let id = if computed {
                HELPER_DEFINE_CLASS_COMPUTED
            } else {
                HELPER_DEFINE_CLASS
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            return Step::Continue(sp);
        }
        "private_symbol" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                HELPER_PRIVATE_SYMBOL,
                pc + size,
            );
            return Step::Continue(sp);
        }
        "close_loc" => {
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_CLOSE_LOC, pc + size);
            return Step::Continue(sp);
        }
        // `array_from` reads its element count (NPOP operand) through `imm`.
        "array_from" => {
            let count = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), count, fv.frame, O_IMM);
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_ARRAY_FROM, pc + size);
            return Step::Continue(sp);
        }
        // Ref cells: `make_loc_ref`/`make_arg_ref`/`make_var_ref_ref` carry the
        // atom in `imm` and the slot index in `imm2`; `make_var_ref` carries only
        // the atom.
        "make_loc_ref" | "make_arg_ref" | "make_var_ref_ref" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u16(5)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM2);
            let id = match name {
                "make_arg_ref" => HELPER_MAKE_ARG_REF,
                "make_var_ref_ref" => HELPER_MAKE_VAR_REF_REF,
                _ => HELPER_MAKE_LOC_REF,
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            return Step::Continue(sp);
        }
        "make_var_ref" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp =
                call_stack_helper(state, b, fv, helper_sig, sp, HELPER_MAKE_VAR_REF, pc + size);
            return Step::Continue(sp);
        }
        // `throw_error <atom> <type>`: the atom rides in `imm`, the error kind in
        // `imm2` (`bytecode::engine_ops::throw_error` names both). It moves
        // nothing and always raises, so the block ends here.
        "throw_error" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let kind = b.ins().iconst(types::I32, i64::from(insn.operand_u8(5)));
            b.ins().store(MemFlags::new(), kind, fv.frame, O_IMM2);
            call_raising_helper(state, b, fv, helper_sig, sp, HELPER_THROW_ERROR, pc + size);
            return Step::Stop;
        }
        // `throw`: pops the exception value and always raises.
        "throw" => {
            call_raising_helper(state, b, fv, helper_sig, sp, HELPER_THROW, pc + size);
            return Step::Stop;
        }
        // `push_bigint_i32 <value>`: the `i32` immediate rides in `imm`.
        "push_bigint_i32" => {
            let value = b.ins().iconst(types::I32, i64::from(insn.operand_i32(1)));
            b.ins().store(MemFlags::new(), value, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                HELPER_PUSH_BIGINT_I32,
                pc + size,
            );
            return Step::Continue(sp);
        }
        // `object` plus the (simple push, `define_field`)* run that
        // follows it in this block collapses into one helper call that builds
        // the whole literal. The window's pushes are emitted here; the driver
        // skips the instructions it covers (`skip_until_pc`). No window means
        // the plain `object` helper, as before.
        "object" => {
            let window = state.lit_windows.get(&pc).cloned();
            let Some(window) = window else {
                let sp = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_OBJECT, pc + size);
                return Step::Continue(sp);
            };
            let instructions = verified.instructions();
            let mut sp = sp;
            for (i, (&index, &atom)) in window
                .value_indices
                .iter()
                .zip(window.atoms.iter())
                .enumerate()
            {
                let atom = b.ins().iconst(types::I32, i64::from(atom));
                b.ins().store(
                    MemFlags::new(),
                    atom,
                    fv.frame,
                    O_LIT_ATOMS + (i as i32) * 4,
                );
                sp = push_value(state, b, fv, &instructions[index], sp);
            }
            let count = b
                .ins()
                .iconst(types::I32, i64::from(window.atoms.len() as u32));
            b.ins().store(MemFlags::new(), count, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                HELPER_OBJECT_LITERAL,
                window.end_pc,
            );
            // The window's net effect is `object`'s: N pushes then the
            // helper's `-N + 1` leaves the literal at `start_height + 1`. The
            // flush emptied the cache, so only the anchor (height + no
            // tracked effect) keeps the driver's cross-check quiet.
            state.stack.anchor(start_height + 1);
            state.skip_until_pc = window.end_pc;
            return Step::Continue(sp);
        }
        _ => {}
    }

    // Float fast path: `div` always yields a `Float64` (Int/Int divides as `f64`), so
    // it gets a dedicated numeric arm instead of a `FastOp`. It is not a CSE
    // candidate (`is_cse_expression` stays exactly the `fast_binary` set).
    if name == "div" {
        return lower_fast_div(state, b, fv, sp_var, helper_sig, sp, insn);
    }
    if let Some(op) = fast_binary(name) {
        // The CSE pass: a repeat of an expression the same block already
        // computed over the same proven-numeric values. The result is still in
        // a register, and the copy *is* the instruction — no tag checks, no
        // arithmetic, no helper fallback.
        if let Some(reg) = state.cse_reuse(insn.pc()) {
            return lower_cse_reuse(state, b, sp, reg);
        }
        let fallback = helper_id(name).expect("a fast binary has a helper");
        return lower_fast_binary(state, b, fv, sp_var, helper_sig, sp, insn, op, fallback);
    }
    if let Some(id) = helper_id(name) {
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
        // The pure pushes share one lowering with the merged-literal window.
        "push_i32" | "push_const" => Step::Continue(push_value(state, b, fv, insn, sp)),
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
        "undefined" | "null" => Step::Continue(push_value(state, b, fv, insn, sp)),
        // Frame-entry TDZ prologue: the slot starts as the `Uninitialized`
        // sentinel so a later `get_loc_check` sees it.
        "set_loc_uninitialized" => {
            let v = b.ins().iconst(types::I64, UNINITIALIZED_BITS);
            b.ins()
                .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
            state.locals.fill(false, u32::from(insn.operand_u16(1)), v);
            Step::Continue(sp)
        }
        // TDZ read: the interpreter owns the error (its message names the
        // binding and, per ES, belongs in the *caller's* realm), so the
        // sentinel case exits `Resume` at this pc and lets it re-execute the
        // op. The flush before the split keeps memory authoritative on both
        // paths (the exit materializes the operand stack). `get_loc_checkthis`
        // shares the lowering: only the error realm differs, and that path is
        // the interpreter's.
        "get_loc_check" | "get_loc_checkthis" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state
                .locals
                .read(b, fv.var_base, false, idx, loc_offset_u16(insn));
            let uninitialized = b.ins().icmp_imm(IntCC::Equal, v, UNINITIALIZED_BITS);
            let slow = b.create_block();
            let ok = b.create_block();
            state.stack.flush(b, sp);
            b.ins().brif(uninitialized, slow, &[], ok, &[]);
            b.switch_to_block(slow);
            exit_const(state, b, fv, EXIT_RESUME, pc, sp);
            b.switch_to_block(ok);
            Step::Continue(state.stack.push(b, sp, v))
        }
        // TDZ-checked local writes: the guard exits `Resume` at this pc and
        // lets the interpreter raise the error (same delegation as the read
        // above). `put_loc_check`/`set_loc_check` require an initialized slot,
        // `put_loc_check_init` requires the opposite.
        "put_loc_check" | "put_loc_check_init" | "set_loc_check" => {
            let idx = u32::from(insn.operand_u16(1));
            let cur = state
                .locals
                .read(b, fv.var_base, false, idx, loc_offset_u16(insn));
            let uninitialized = b.ins().icmp_imm(IntCC::Equal, cur, UNINITIALIZED_BITS);
            let bail = if name == "put_loc_check_init" {
                b.ins().icmp_imm(IntCC::NotEqual, cur, UNINITIALIZED_BITS)
            } else {
                uninitialized
            };
            let (sp, v) = if name == "set_loc_check" {
                let v = state.stack.read_top(b, sp);
                (sp, v)
            } else {
                state.stack.pop(b, sp)
            };
            let slow = b.create_block();
            let ok = b.create_block();
            state.stack.flush(b, sp);
            b.ins().brif(bail, slow, &[], ok, &[]);
            b.switch_to_block(slow);
            exit_const(state, b, fv, EXIT_RESUME, pc, sp);
            b.switch_to_block(ok);
            b.ins()
                .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
            state.locals.fill(false, idx, v);
            Step::Continue(sp)
        }
        // `rest`: the helper rebuilds the interpreter's `args[start..]` slice.
        "rest" => {
            let start = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), start, fv.frame, O_IMM);
            let sp = call_stack_helper(
                state,
                b,
                fv,
                helper_sig,
                sp,
                super::abi::HELPER_REST,
                pc + size,
            );
            Step::Continue(sp)
        }
        // `??`: true when the value is exactly `null` or `undefined`. The tag
        // sits in the top 16 bits (0 for an int, 8 for a float, negative for a
        // pointer), so shifting it out compares without unboxing anything.
        "is_undefined_or_null" => {
            let (sp, v) = state.stack.pop(b, sp);
            let tag = b.ins().ushr_imm(v, 48);
            let is_null = b.ins().icmp_imm(IntCC::Equal, tag, NULL_TAG);
            let is_undefined = b.ins().icmp_imm(IntCC::Equal, tag, UNDEFINED_TAG);
            let cond = b.ins().bor(is_null, is_undefined);
            let widened = b.ins().uextend(types::I64, cond);
            let boxed = b.ins().bor_imm(widened, BOOL_TAG);
            Step::Continue(state.stack.push(b, sp, boxed))
        }
        "get_var" | "get_var_undef" | "put_var" | "put_var_init" => {
            // An own data property of one of the two global objects is
            // read/written inline behind a shape + slot guard. A function with
            // no cache (a persistent restore) keeps the helper.
            if u32::from(insn.operand_u16(1)) < state.global_cache_count {
                return match name {
                    "get_var" => {
                        lower_get_var_fast(state, b, fv, helper_sig, sp_var, sp, insn, true)
                    }
                    "get_var_undef" => {
                        lower_get_var_fast(state, b, fv, helper_sig, sp_var, sp, insn, false)
                    }
                    "put_var" => {
                        lower_put_var_fast(state, b, fv, helper_sig, sp_var, sp, insn, false)
                    }
                    _ => lower_put_var_fast(state, b, fv, helper_sig, sp_var, sp, insn, true),
                };
            }
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
        // `delete name` on the global object (the fall-through of
        // `with_delete_var`); the atom rides in `imm`.
        "delete_var" => {
            let atom = b.ins().iconst(types::I32, i64::from(insn.operand_u32(1)));
            b.ins().store(MemFlags::new(), atom, fv.frame, O_IMM);
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_DELETE_VAR, pc + size);
            Step::Continue(sp)
        }
        // Closure captures live behind the GC, so the helper owns the read and
        // write (and, for the `_check` variants, the TDZ error).
        "get_var_ref"
        | "put_var_ref"
        | "set_var_ref"
        | "get_var_ref_check"
        | "put_var_ref_check"
        | "put_var_ref_check_init" => {
            let idx = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), idx, fv.frame, O_IMM);
            let id = match name {
                "get_var_ref" => HELPER_GET_VAR_REF,
                "put_var_ref" => HELPER_PUT_VAR_REF,
                "set_var_ref" => HELPER_SET_VAR_REF,
                "get_var_ref_check" => HELPER_GET_VAR_REF_CHECK,
                "put_var_ref_check" => HELPER_PUT_VAR_REF_CHECK,
                _ => HELPER_PUT_VAR_REF_CHECK_INIT,
            };
            let sp = call_stack_helper(state, b, fv, helper_sig, sp, id, pc + size);
            Step::Continue(sp)
        }
        "get_array_el" | "get_array_el2" | "get_array_el3" | "put_array_el" => {
            // The property inline guard: the read from a packed array with an integer index is
            // inlined (the guard + the load), and so is the write into one
            // (the guard + the store). The other two read shapes keep the
            // helper.
            if name == "get_array_el" {
                return lower_get_array_el_fast(
                    state,
                    b,
                    fv,
                    helper_sig,
                    sp_var,
                    sp,
                    insn,
                    ArrayRead::Replace,
                );
            }
            if name == "get_array_el2" {
                return lower_get_array_el_fast(
                    state,
                    b,
                    fv,
                    helper_sig,
                    sp_var,
                    sp,
                    insn,
                    ArrayRead::Keep,
                );
            }
            if name == "put_array_el" {
                return lower_put_array_el_fast(state, b, fv, helper_sig, sp_var, sp, insn);
            }
            // The site index rides in `imm2` (array element keys are runtime
            // values, so the cached property atom lives in the cache entry).
            #[cfg(feature = "inline_cache")]
            {
                let ic = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
                b.ins().store(MemFlags::new(), ic, fv.frame, O_IMM2);
            }
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
            // The property inline guard: the read of an own data property and the write into one
            // are inlined (the guard + the load / the store). `get_field2`
            // (the method-call shape) keeps the helper.
            #[cfg(feature = "inline_cache")]
            if ic_index(insn) < state.ic_count {
                if name == "get_field" {
                    return lower_get_field_fast(
                        state,
                        b,
                        fv,
                        helper_sig,
                        sp_var,
                        sp,
                        insn,
                        FieldRead::Replace,
                    );
                }
                if name == "get_field2" {
                    return lower_get_field_fast(
                        state,
                        b,
                        fv,
                        helper_sig,
                        sp_var,
                        sp,
                        insn,
                        FieldRead::Keep,
                    );
                }
                if name == "put_field" {
                    return lower_put_field_fast(state, b, fv, helper_sig, sp_var, sp, insn);
                }
            }
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
            // Both paths (specialized and helper) write the local slot.
            state.locals.drop_slot(false, u32::from(insn.operand_u8(1)));
            // Speculative fast path: type feedback proved this site int-monomorphic, so speculate an
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
        // Local/argument reads and writes keep a register copy (the stack-slot register cache's
        // `LocalCache`): a repeated read in the same block skips its load, and
        // the store still happens, so memory stays authoritative.
        "get_loc" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state
                .locals
                .read(b, fv.var_base, false, idx, loc_offset_u16(insn));
            // Local liveness: a slot the verifier proves `Int32` is a known
            // `Int`. Only trusted when no reference can alias a local (the same
            // condition the local cache uses), because a `VarRef` write out of
            // sight would invalidate the proof.
            let proven_int = state.locals.locals
                && verified
                    .local_kinds_at(pc)
                    .and_then(|kinds| kinds.get(idx as usize))
                    == Some(&SlotKind::Int32);
            Step::Continue(if proven_int {
                state.stack.push_int(b, sp, v)
            } else {
                state.stack.push(b, sp, v)
            })
        }
        "get_arg" => Step::Continue(push_value(state, b, fv, insn, sp)),
        "put_loc" => {
            let idx = u32::from(insn.operand_u16(1));
            let (sp, v) = state.stack.pop(b, sp);
            if state.dead_stores.contains(&pc) {
                // Dead-store elimination: the value is dead before any read, so the frame
                // write is dropped. The register copy must go too — memory keeps
                // the old value and a stale cache entry would shadow it.
                state.locals.drop_slot(false, idx);
            } else {
                b.ins()
                    .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
                state.locals.fill(false, idx, v);
            }
            Step::Continue(sp)
        }
        "put_arg" => {
            let (sp, v) = state.stack.pop(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.arg_base, loc_offset_u16(insn));
            state.locals.fill(true, u32::from(insn.operand_u16(1)), v);
            Step::Continue(sp)
        }
        "set_loc" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state.stack.read_top(b, sp);
            if state.dead_stores.contains(&pc) {
                // Dead store: see the `put_loc` arm.
                state.locals.drop_slot(false, idx);
            } else {
                b.ins()
                    .store(MemFlags::new(), v, fv.var_base, loc_offset_u16(insn));
                state.locals.fill(false, idx, v);
            }
            Step::Continue(sp)
        }
        "set_arg" => {
            let v = state.stack.read_top(b, sp);
            b.ins()
                .store(MemFlags::new(), v, fv.arg_base, loc_offset_u16(insn));
            state.locals.fill(true, u32::from(insn.operand_u16(1)), v);
            Step::Continue(sp)
        }
        // Local `++`/`--`: the helper updates the slot in place, so an exception
        // exit leaves `sp` untouched (matching the interpreter).
        "inc_loc" | "dec_loc" => {
            // Both paths write the slot; the copy is stale either way.
            state.locals.drop_slot(false, u32::from(insn.operand_u8(1)));
            let id = if name == "inc_loc" {
                super::abi::HELPER_INC_LOCAL
            } else {
                super::abi::HELPER_DEC_LOCAL
            };
            // Speculative fast path: type feedback proved this site int-monomorphic, so speculate an
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
                .edge_args(b, fv, sp, &[(dest, target)])
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
                .edge_args(b, fv, sp, &[(dest, target)])
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
            let all_args = state.edge_args(b, fv, sp, &targets);
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
            exit_const(state, b, fv, EXIT_RESUME, pc, sp);
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
            write_exit_frame(b, fv, pc + size, sp);
            b.ins().store(MemFlags::new(), v, fv.frame, O_OUT_VALUE);
            let kind = b.ins().iconst(types::I32, EXIT_DONE);
            b.ins().return_(&[kind]);
            Step::Stop
        }
        "return_undef" => {
            state.stack.flush(b, sp);
            write_exit_frame(b, fv, pc + size, sp);
            let v = b.ins().iconst(types::I64, UNDEFINED_BITS);
            b.ins().store(MemFlags::new(), v, fv.frame, O_OUT_VALUE);
            let kind = b.ins().iconst(types::I32, EXIT_DONE);
            b.ins().return_(&[kind]);
            Step::Stop
        }
        // Unsupported: hand this instruction to the interpreter.
        _ => {
            exit_const(state, b, fv, EXIT_RESUME, pc, sp);
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

#[cfg(test)]
mod tests;
