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

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    mem::offset_of,
};

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

use crate::{jit::snapshot::ConstMeta, vm_stack::ExecMode};

use super::{
    abi::{
        HELPER_ADD, HELPER_ADD_BRAND, HELPER_ADD_LOC, HELPER_AND, HELPER_APPEND, HELPER_APPLY,
        HELPER_ARRAY_FROM, HELPER_CALL, HELPER_CALL_CONSTRUCTOR, HELPER_CALL_METHOD,
        HELPER_CHECK_BRAND, HELPER_CHECK_CTOR, HELPER_CHECK_CTOR_RETURN, HELPER_CLOSE_LOC,
        HELPER_COPY_DATA_PROPERTIES, HELPER_DEC, HELPER_DEFINE_ARRAY_EL, HELPER_DEFINE_CLASS,
        HELPER_DEFINE_CLASS_COMPUTED, HELPER_DEFINE_FIELD, HELPER_DEFINE_METHOD,
        HELPER_DEFINE_METHOD_COMPUTED, HELPER_DEFINE_PRIVATE_FIELD, HELPER_DELETE,
        HELPER_DELETE_VAR, HELPER_DIV, HELPER_EQ, HELPER_FOR_IN_NEXT, HELPER_FOR_IN_START,
        HELPER_FOR_OF_NEXT, HELPER_FOR_OF_START, HELPER_GET_ARRAY_EL, HELPER_GET_ARRAY_EL2,
        HELPER_GET_ARRAY_EL3, HELPER_GET_FIELD, HELPER_GET_FIELD2, HELPER_GET_PRIVATE_FIELD,
        HELPER_GET_REF_VALUE, HELPER_GET_SUPER, HELPER_GET_SUPER_VALUE, HELPER_GET_VAR,
        HELPER_GET_VAR_REF, HELPER_GET_VAR_REF_CHECK, HELPER_GET_VAR_UNDEF, HELPER_GT, HELPER_GTE,
        HELPER_IN, HELPER_INC, HELPER_INIT_CTOR, HELPER_INSTANCEOF, HELPER_ITERATOR_CALL,
        HELPER_ITERATOR_CHECK_OBJECT, HELPER_ITERATOR_CLOSE, HELPER_ITERATOR_GET_VALUE_DONE,
        HELPER_ITERATOR_NEXT, HELPER_LNOT, HELPER_LT, HELPER_LTE, HELPER_MAKE_ARG_REF,
        HELPER_MAKE_LOC_REF, HELPER_MAKE_VAR_REF, HELPER_MAKE_VAR_REF_REF, HELPER_MOD, HELPER_MUL,
        HELPER_NEG, HELPER_NEQ, HELPER_NIP_CATCH, HELPER_NOT, HELPER_OBJECT, HELPER_OK, HELPER_OR,
        HELPER_PLUS, HELPER_POST_DEC, HELPER_POST_INC, HELPER_POW, HELPER_PRIVATE_IN,
        HELPER_PRIVATE_SYMBOL, HELPER_PUSH_BIGINT_I32, HELPER_PUT_ARRAY_EL, HELPER_PUT_FIELD,
        HELPER_PUT_PRIVATE_FIELD, HELPER_PUT_REF_VALUE, HELPER_PUT_SUPER_VALUE, HELPER_PUT_VAR,
        HELPER_PUT_VAR_INIT, HELPER_PUT_VAR_REF, HELPER_PUT_VAR_REF_CHECK,
        HELPER_PUT_VAR_REF_CHECK_INIT, HELPER_REGEXP, HELPER_SAFEPOINT, HELPER_SAR,
        HELPER_SET_HOME_OBJECT, HELPER_SET_NAME_COMPUTED, HELPER_SET_PROTO, HELPER_SET_VAR_REF,
        HELPER_SHL, HELPER_SHR, HELPER_STRICT_EQ, HELPER_STRICT_NEQ, HELPER_SUB, HELPER_THROW,
        HELPER_THROW_ERROR, HELPER_TO_BOOL, HELPER_TO_OBJECT, HELPER_TO_PROPKEY, HELPER_TYPEOF,
        HELPER_WITH_DELETE_VAR, HELPER_WITH_GET_REF, HELPER_WITH_GET_VAR, HELPER_WITH_MAKE_REF,
        HELPER_WITH_MISS, HELPER_WITH_PUT_VAR, HELPER_XOR, JitExecFrame, SPEC_MISS_IMM2,
    },
    bytecode::{
        cfg::ControlFlowGraph, decode::Instruction, stack::SlotKind, verify::VerifiedFunction,
    },
    error::JitError,
    feedback::{CLASS_FLOAT, CLASS_INT, SiteFeedback},
    ir::{Ir, LocalRef, RedundantExpressions},
};

/// Exit codes as written by generated code (keep in sync with `JitExitKind`).
const EXIT_DONE: i64 = 0;
const EXIT_RESUME: i64 = 6;
const EXIT_RETRY: i64 = 7;

const UNDEFINED_BITS: i64 = 3 << 48;
/// `JSValueTag::Uninitialized` (the TDZ sentinel written by
/// `set_loc_uninitialized`).
const UNINITIALIZED_BITS: i64 = 4 << 48;
const BOOL_TAG: i64 = 1 << 48;
/// `JSValueTag::Null`/`Undefined`, i.e. the high 16 bits of `NULL`/`UNDEFINED`.
const NULL_TAG: i64 = 2;
const UNDEFINED_TAG: i64 = 3;
/// `js_new_float64(d) == d.to_bits() - (0xFFF9 << 48)` for a non-NaN value.
const FLOAT64_ADDEND: i64 = 0xFFF9 << 48;
/// `JS_NAN_U64` (`0x7ff8… - (0xFFF9 << 48)`): the canonical quiet NaN
/// `js_new_float64` substitutes for every NaN payload. The float fast path
/// must box through it, because the differential suite compares bit-for-bit.
const JS_NAN_U64_BITS: i64 = 0x7FFF_0000_0000_0000;

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
                | "is_undefined_or_null"
                | "set_loc_uninitialized"
                | "get_loc_check"
                | "rest"
                | "special_object"
                | "fclosure"
                | "set_name"
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
                | "get_var_ref"
                | "put_var_ref"
                | "set_var_ref"
                | "get_var_ref_check"
                | "put_var_ref_check"
                | "put_var_ref_check_init"
                | "get_loc_checkthis"
                | "put_loc_check"
                | "put_loc_check_init"
                | "set_loc_check"
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
                // D-6: the four call opcodes run the callee through a call
                // helper on the same frame (no per-call exit).
                | "call"
                | "call_method"
                | "call_constructor"
                | "apply"
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
        // `delete name` on the global object; the fall-through of `with_delete_var`.
        "delete_var" => HELPER_DELETE_VAR,
        // The engine-generated opcodes (todo 2.6). `throw_error` and
        // `push_bigint_i32` carry an immediate, so they have explicit arms in
        // `lower_insn` and are only listed here for `supported()`.
        "throw" => HELPER_THROW,
        "throw_error" => HELPER_THROW_ERROR,
        "regexp" => HELPER_REGEXP,
        "push_bigint_i32" => HELPER_PUSH_BIGINT_I32,
        "nip_catch" => HELPER_NIP_CATCH,
        // Sync iteration. `for_of_next`/`iterator_close`/`iterator_call` need
        // more than a name→id map (an operand or a stack adjustment), so they
        // have explicit arms in `lower_insn` and are only listed here for
        // `supported()`.
        "for_in_start" => HELPER_FOR_IN_START,
        "for_in_next" => HELPER_FOR_IN_NEXT,
        "for_of_start" => HELPER_FOR_OF_START,
        "for_of_next" => HELPER_FOR_OF_NEXT,
        "iterator_check_object" => HELPER_ITERATOR_CHECK_OBJECT,
        "iterator_get_value_done" => HELPER_ITERATOR_GET_VALUE_DONE,
        "iterator_close" => HELPER_ITERATOR_CLOSE,
        "iterator_next" => HELPER_ITERATOR_NEXT,
        "iterator_call" => HELPER_ITERATOR_CALL,
        // Class / object-literal / private-field machinery. The opcodes that
        // need an immediate (`define_field`, `define_method*`, `define_class*`,
        // `copy_data_properties`, `private_symbol`) have explicit arms in
        // `lower_insn`; the five super/private/brand ones and `append` are
        // plain helper calls.
        "set_name_computed" => HELPER_SET_NAME_COMPUTED,
        "set_proto" => HELPER_SET_PROTO,
        "set_home_object" => HELPER_SET_HOME_OBJECT,
        "define_array_el" => HELPER_DEFINE_ARRAY_EL,
        "define_field" => HELPER_DEFINE_FIELD,
        "copy_data_properties" => HELPER_COPY_DATA_PROPERTIES,
        "define_method" => HELPER_DEFINE_METHOD,
        "define_method_computed" => HELPER_DEFINE_METHOD_COMPUTED,
        "define_class" => HELPER_DEFINE_CLASS,
        "define_class_computed" => HELPER_DEFINE_CLASS_COMPUTED,
        "private_symbol" => HELPER_PRIVATE_SYMBOL,
        "check_ctor_return" => HELPER_CHECK_CTOR_RETURN,
        "check_ctor" => HELPER_CHECK_CTOR,
        "init_ctor" => HELPER_INIT_CTOR,
        "check_brand" => HELPER_CHECK_BRAND,
        "add_brand" => HELPER_ADD_BRAND,
        "get_private_field" => HELPER_GET_PRIVATE_FIELD,
        "put_private_field" => HELPER_PUT_PRIVATE_FIELD,
        "define_private_field" => HELPER_DEFINE_PRIVATE_FIELD,
        "append" => HELPER_APPEND,
        "private_in" => HELPER_PRIVATE_IN,
        "get_super" => HELPER_GET_SUPER,
        "get_super_value" => HELPER_GET_SUPER_VALUE,
        "put_super_value" => HELPER_PUT_SUPER_VALUE,
        "close_loc" => HELPER_CLOSE_LOC,
        // Literal seeds whose opcode carries its count in `imm` (`array_from`)
        // have explicit arms in `lower_insn`.
        "object" => HELPER_OBJECT,
        "array_from" => HELPER_ARRAY_FROM,
        // Ref-cell family (`make_*_ref` build the ref object; both engines share
        // `bytecode::ref_ops`). The `make_*` ops need an atom/slot immediate, so
        // they have explicit arms in `lower_insn`; `get`/`put_ref_value` are
        // plain helper calls.
        "make_loc_ref" => HELPER_MAKE_LOC_REF,
        "make_arg_ref" => HELPER_MAKE_ARG_REF,
        "make_var_ref_ref" => HELPER_MAKE_VAR_REF_REF,
        "make_var_ref" => HELPER_MAKE_VAR_REF,
        "get_ref_value" => HELPER_GET_REF_VALUE,
        "put_ref_value" => HELPER_PUT_REF_VALUE,
        // `with_*` takes the instruction's branch on a hit, so it has an
        // explicit arm in `lower_insn`; listed here for `supported()`.
        "with_get_var" => HELPER_WITH_GET_VAR,
        "with_put_var" => HELPER_WITH_PUT_VAR,
        "with_delete_var" => HELPER_WITH_DELETE_VAR,
        "with_make_ref" => HELPER_WITH_MAKE_REF,
        "with_get_ref" => HELPER_WITH_GET_REF,
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

/// Whether an opcode is one of the pure binaries the CSE pass numbers (epic S2
/// 1.5). It is exactly the set [`fast_op`] lowers, so the pass can neither plan
/// a reuse the lowering would not honor nor miss a site it does.
pub(crate) fn is_cse_expression(name: &str) -> bool {
    fast_op(name).is_some()
}

/// Output arrangement for the stack shuffles: each output copies the popped
/// operand at that index. Order matches the interpreter's writes.
/// Every opcode the baseline lowers as a pure stack shuffle. Exported like
/// `policy::opcode_names` so the coverage gate in `tests/jit_opcodes.rs` can
/// check its oracle set against the real table instead of a copied list.
pub fn shuffle_opcodes() -> &'static [&'static str] {
    &[
        "nip", "nip1", "dup", "dup1", "dup2", "dup3", "insert2", "insert3", "insert4", "perm3",
        "perm4", "perm5", "swap", "swap2", "rot3l", "rot3r", "rot4l", "rot5l",
    ]
}

/// Pop count and output source indices for a stack shuffle (see
/// [`shuffle_opcodes`]).
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
        // The `perm` family rotates the *deepest* operand up to the top; it is
        // not a top-two swap. Must match QuickJS (`quickjs-opcode.h`:
        // `perm4: obj prop a b -> a obj prop b`, `perm5: this obj prop a b ->
        // a this obj prop b`, whose C arms rotate `sp[-4..-2]`/`sp[-5..-2]`),
        // this repo's interpreter arms and `stack.rs::copied_stack_values`
        // (the guard test `shuffle_table_matches_the_verifier` keeps the last
        // two in step).
        "perm3" => (3, &[1, 0, 2]),
        "perm4" => (4, &[2, 0, 1, 3]),
        "perm5" => (5, &[3, 0, 1, 2, 4]),
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
    /// Whether tier-2 specialization is allowed (epic 7.2 tier-down). False
    /// after a function's speculative sites missed too often: the recompile
    /// keeps the int fast paths but drops the speculation.
    speculate: bool,
    /// Block-local write-back cache for operand-stack slots (epic F4).
    stack: StackCache,
    /// Epic F4 step 2: per block start pc, the slots its code reads in
    /// registers before it can observe the frame. Each is a block parameter
    /// that every incoming edge supplies (see [`State::edge_args`]).
    carried: HashMap<u32, Vec<u32>>,
    /// Epic S2 (1.1): per block start pc, the locals/arguments its code reads
    /// before writing. They cross the incoming edges as block parameters too,
    /// so the successor's first read is a register, not a frame load.
    carried_locals: HashMap<u32, Vec<LocalRef>>,
    /// Epic S2 (1.2): pcs whose local store is dead (no read before redefinition
    /// on any path), so the lowering skips the memory write. Empty unless the
    /// IR's liveness is exact.
    dead_stores: HashSet<u32>,
    /// Epic S2 (1.5): the value numbering's CSE plan (which expressions are
    /// repeats of an earlier one) and, per block, the registers those earlier
    /// computations published. Both are block-scoped like [`LocalCache`]: a
    /// value does not cross an edge as a register.
    cse: RedundantExpressions,
    cse_regs: HashMap<u32, Value>,
    /// Register copies of local/argument slots (epic F4: fewer loads in a
    /// block). Cleared at every block entry — see [`LocalCache`].
    locals: LocalCache,
    /// The pool's constant tags, for the `Int`-constant guard fact.
    const_meta: &'a [ConstMeta],
    /// Proven entry stack height per block start pc. An edge's `sp` equals it,
    /// so it addresses the frame fallback loads even when the model's own
    /// height is stale (a helper-terminated block never re-anchors).
    entry_heights: HashMap<u32, u16>,
}

/// Block-local register copies of local/argument slots (epic F4, the read half
/// of "locals in registers"): a `get_loc`/`get_arg` that repeats one already in
/// a register skips its load.
///
/// Memory stays authoritative — stores are still emitted, nothing is deferred —
/// so no flush point changes: a copy is simply dropped the moment its slot can
/// change. A slot can only change through this function's own writes, *unless*
/// a nested function or a reference object aliases it, which is why
/// [`cacheable_slots`] turns caching off for such functions. Values never cross
/// a block: a register only dominates its own block, so `lower` clears the
/// cache at every block entry.
#[derive(Default)]
struct LocalCache {
    entries: HashMap<(bool, u32), Value>,
    /// Whether local slots may be cached (false when the function creates
    /// closures or refs to them).
    locals: bool,
    /// Same for argument slots (also false when `arguments` is the *mapped*
    /// object, which aliases the formals through `VarRef`s).
    args: bool,
}

impl LocalCache {
    fn new(locals: bool, args: bool) -> Self {
        Self {
            entries: HashMap::new(),
            locals,
            args,
        }
    }

    /// The register copy of slot `idx`, or a fresh load (which also fills the
    /// cache). `is_arg` selects the frame base and its cacheability.
    fn read(
        &mut self,
        b: &mut FunctionBuilder,
        base: Value,
        is_arg: bool,
        idx: u32,
        offset: i32,
    ) -> Value {
        if let Some(&value) = self.entries.get(&(is_arg, idx)) {
            return value;
        }
        let value = b.ins().load(types::I64, MemFlags::new(), base, offset);
        if self.cacheable(is_arg) {
            self.entries.insert((is_arg, idx), value);
        }
        value
    }

    /// Record `value` as the slot's register copy. Memory is written
    /// separately, so the copy costs no flush.
    fn fill(&mut self, is_arg: bool, idx: u32, value: Value) {
        if self.cacheable(is_arg) {
            self.entries.insert((is_arg, idx), value);
        }
    }

    /// The slot changed: its copy is stale.
    fn drop_slot(&mut self, is_arg: bool, idx: u32) {
        self.entries.remove(&(is_arg, idx));
    }

    fn clear(&mut self) {
        self.entries.clear();
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    const fn cacheable(&self, is_arg: bool) -> bool {
        if is_arg { self.args } else { self.locals }
    }
}

/// Whether [`LocalCache`] may cache local and argument slots of this function.
/// A slot is unsafe to cache exactly when something other than a direct write
/// in this function can change it: a nested function may capture it
/// (`fclosure`), a reference object may point at it (`make_loc_ref` /
/// `make_arg_ref` / `make_var_ref_ref`), or the `arguments` object may alias a
/// formal (`special_object 1`, the mapped variant).
fn cacheable_slots(verified: &VerifiedFunction) -> (bool, bool) {
    let mut creates_refs = false;
    let mut mapped_arguments = false;
    for instruction in verified.instructions() {
        match instruction.name() {
            "fclosure" | "make_loc_ref" | "make_arg_ref" | "make_var_ref_ref" => {
                creates_refs = true
            }
            "special_object" if instruction.operand_u8(1) == MAPPED_ARGUMENTS => {
                mapped_arguments = true;
            }
            _ => {}
        }
    }
    if creates_refs {
        return (false, false);
    }
    (true, !mapped_arguments)
}

/// `SpecialObject::MappedArguments`, whose `VarRef` properties alias the
/// formals (see `src/function/argument.rs`).
const MAPPED_ARGUMENTS: u8 = 1;

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
/// One register-resident operand-stack slot.
struct CachedSlot {
    value: Value,
    /// Statically known to be `Int` (tag 0). Only ever set for values the
    /// compiler itself produced — a `push_i32`, an `Int` constant from the pool
    /// — so it needs no runtime check to be sound. It dies with the entry, which
    /// is what keeps it conservative: anything that flushes the cache (a helper
    /// call, an exit, a branch) drops the fact with the value.
    known_int: bool,
}

impl CachedSlot {
    const fn unknown(value: Value) -> Self {
        Self {
            value,
            known_int: false,
        }
    }
}

#[derive(Default)]
struct StackCache {
    height: u32,
    entries: HashMap<u32, CachedSlot>,
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
        self.push_slot(b, sp, CachedSlot::unknown(v))
    }

    /// [`Self::push`] for a value the compiler produced as an `Int` (a literal
    /// or an `Int` constant), which then needs no tag check.
    fn push_int(&mut self, b: &mut FunctionBuilder, sp: Value, v: Value) -> Value {
        self.push_slot(
            b,
            sp,
            CachedSlot {
                value: v,
                known_int: true,
            },
        )
    }

    fn push_slot(&mut self, b: &mut FunctionBuilder, sp: Value, slot: CachedSlot) -> Value {
        self.height = self.height.saturating_add(1);
        self.tracked = true;
        self.entries.insert(self.height, slot);
        b.ins().iadd_imm(sp, 8)
    }

    /// Is slot `k` statically an `Int`? A miss (already in memory) is unknown.
    fn known_int(&self, k: u32) -> bool {
        self.entries.get(&k).is_some_and(|slot| slot.known_int)
    }

    /// Pop the top slot: reuse the register-resident value when there is one,
    /// otherwise load it from the frame.
    fn pop(&mut self, b: &mut FunctionBuilder, sp: Value) -> (Value, Value) {
        debug_assert!(self.height > 0, "verified code cannot underflow");
        let k = self.height;
        self.height = self.height.saturating_sub(1);
        self.tracked = true;
        match self.entries.remove(&k) {
            Some(slot) => (b.ins().iadd_imm(sp, -8), slot.value),
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
            Some(slot) => slot.value,
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

    /// Record that the current top slot (key `height`) is a known `Int`,
    /// without changing the height or `sp`. Only sound when *every* path that
    /// reaches the slot boxes an `Int` (epic 1.4 item 1: `shl`/`sar`).
    fn result_int(&mut self, sp: Value, value: Value) -> Value {
        self.entries.insert(
            self.height,
            CachedSlot {
                value,
                known_int: true,
            },
        );
        sp
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
            let Some(slot) = self.entries.get(&k) else {
                continue;
            };
            b.ins().store(
                MemFlags::new(),
                slot.value,
                sp,
                Self::slot_offset(height, k),
            );
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
        // Stack slots come first, then the carried locals (epic S2 / 1.1).
        for _ in 0..self.carried_for(pc).len() {
            b.append_block_param(block, types::I64);
        }
        for _ in 0..self.carried_locals_for(pc).len() {
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

    /// Locals/arguments block `pc` reads before writing — carried as block
    /// parameters right after the carried stack slots (epic S2 / 1.1).
    fn carried_locals_for(&self, pc: u32) -> &[LocalRef] {
        self.carried_locals.get(&pc).map_or(&[], Vec::as_slice)
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
        fv: &FrameVals,
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
                    Some(slot) => slot.value,
                    None => b.ins().load(
                        types::I64,
                        MemFlags::new(),
                        sp,
                        StackCache::slot_offset(entry_h, k),
                    ),
                };
                args.push(value);
            }
            // Epic S2 / 1.1: the carried locals follow, in the same order
            // `block_for` appended their parameters. Memory is authoritative
            // here (every write stores), so a miss is a plain frame load.
            for slot in self.carried_locals_for(pc) {
                let value = match self.locals.entries.get(&(slot.is_arg, slot.idx)) {
                    Some(&value) => value,
                    None => {
                        let base = if slot.is_arg {
                            fv.arg_base
                        } else {
                            fv.var_base
                        };
                        b.ins()
                            .load(types::I64, MemFlags::new(), base, (slot.idx * 8) as i32)
                    }
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
    /// Always false once tier-down disabled specialization for this function.
    fn int_only(&self, pc: u32, both: bool) -> bool {
        self.speculate && int_only(self.feedback, pc, both)
    }

    fn note_specialized(&self) {
        self.specialized.set(self.specialized.get() + 1);
    }

    /// Epic S2 (1.5): the register holding the value this instruction is a
    /// repeat of, if the pass planned a reuse and the definition actually ran on
    /// the path taken to here (an OSR entry in the middle of the block skips
    /// it, and then the instruction computes normally).
    fn cse_reuse(&self, pc: u32) -> Option<Value> {
        self.cse
            .reuse(pc)
            .and_then(|id| self.cse_regs.get(&id).copied())
    }

    /// Whether a later instruction reuses this one's result, which then has to
    /// stay in a register.
    fn cse_keep(&self, pc: u32) -> Option<u32> {
        self.cse.keep(pc)
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

/// Like [`call_stack_helper`], but for a helper that **always** leaves the
/// frame: it reports `HELPER_EXCEPTION` (or `HELPER_INTERRUPT`) whatever the
/// operand was. There is then no continuation to emit, so the instruction is a
/// `Step::Stop` — which also means a `throw` can be the last instruction of a
/// function, as it is in real bytecode (`cfg::is_terminal`).
///
/// `pc_after` follows the same convention as the helper failure path:
/// `cur_pc_offset` is post-instruction in both engines.
fn call_raising_helper(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    id: usize,
    pc_after: u32,
) {
    state.stack.flush(b, sp);
    b.ins().store(MemFlags::new(), sp, fv.frame, O_SP);
    write_cur_sp(b, fv, sp);
    let callee = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.helpers, (id * 8) as i32);
    let call = b.ins().call_indirect(helper_sig, callee, &[fv.frame]);
    let status = b.inst_results(call)[0];
    exit_status(b, fv, status, pc_after, sp);
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
///
/// Since D-6 lowered the last four `Helper` opcodes (`call`/`call_method`/
/// `call_constructor`/`apply`), every opcode a *verified* function can contain
/// is supported — the only unsupported names are policy `Reject`, which
/// `verify` refuses before compilation (see
/// `every_opcode_the_verifier_accepts_is_lowered`). The check stays as the
/// fail-closed net for a future `Helper` with no lowering.
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
    let instructions = verified.instructions();
    let mut budget = OSR_TAIL_BUDGET;
    let mut seen_call = false;
    for block in verified.control_flow_graph().blocks() {
        let range = block.instruction_range();
        for index in range.start..range.end {
            let instruction = &instructions[index];
            if !matches!(
                instruction.name(),
                "call" | "call_method" | "call_constructor"
            ) {
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
            pcs.push(instruction.pc() + instruction.size() as u32);
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

/// Epic F2: did F1 feedback observe *only* `Int` at this site? Feedback is
/// sorted by pc, so a binary search answers it. `both` is false for
/// single-value sites (`inc_loc`/`dec_loc`), whose `rhs` slot stays 0.
///
/// Epic P7 (task 7.1): the *result* class must be `Int` too. F2's inline int
/// path bails to the helper on `i32` overflow, so a site whose result is always
/// a `Float64` (an accumulator that overflows every trip) would pay that bail
/// every iteration — speculation would lose to the plain helper. The result
/// class lets the gate refuse exactly those sites.
fn int_only(feedback: &[(u32, SiteFeedback)], pc: u32, both: bool) -> bool {
    feedback
        .binary_search_by_key(&pc, |&(site, _)| site)
        .is_ok_and(|i| {
            let site = feedback[i].1;
            site.lhs == CLASS_INT && (!both || site.rhs == CLASS_INT) && site.result == CLASS_INT
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
        | "set_loc_uninitialized" => Touch::Pass,
        "push_this" if strict => Touch::Pass,
        // Reads the operand in a register and rewrites the slot from it.
        "is_undefined_or_null" => Touch::Read(h),
        "put_loc" | "put_arg" | "set_loc" | "set_arg" => Touch::Read(h),
        // Specialized `add_loc` reads the operand before `flush_except([top])`.
        "add_loc" if int_only(feedback, pc, true) => Touch::ReadFlush(vec![h]),
        // Both variants flush before anything reads the stack (`inc_loc`
        // reads the local out of the frame, not out of a cache entry).
        "add_loc" | "inc_loc" | "dec_loc" => Touch::Flush,
        "if_true" | "if_false" | "return" | "ret" => Touch::ReadFlush(vec![h]),
        // `rest`/`special_object`/`fclosure` call helpers; `get_loc_check`
        // flushes before its TDZ split. Everything else is a `Resume` exit
        // (nothing survives it either).
        "rest" | "special_object" | "fclosure" | "get_loc_check" => Touch::Flush,
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

/// Epic S2 (1.1): the locals/arguments each block reads before writing, sorted
/// so the parameter order is stable. The entry block has no incoming edge, and
/// aliased slots are skipped for the same reason the local cache is disabled for
/// them (`cacheable_slots`: a `VarRef` can rewrite the frame out of sight).
fn carried_local_sets(
    ir: &Ir,
    verified: &VerifiedFunction,
    locals_cacheable: bool,
    args_cacheable: bool,
) -> HashMap<u32, Vec<LocalRef>> {
    let mut carried = HashMap::new();
    for block in ir.blocks() {
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

/// Epic S2 (1.2): the set of `put_loc`/`set_loc` pcs whose store the lowering
/// should skip (see [`Ir::dead_local_stores`]). `gate` is the caller's "the IR's
/// liveness is trustworthy for a skipped write": resume-free, exception-free and
/// with no way to alias a frame slot.
fn dead_local_stores(ir: &Ir, instructions: &[Instruction], gate: bool) -> HashSet<u32> {
    ir.dead_local_stores(instructions, gate)
        .into_iter()
        .collect()
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
    let (carried, entry_heights) = carried_sets(verified, osr_pcs, strict);
    let (locals_cacheable, args_cacheable) = cacheable_slots(verified);
    // Epic S2 (1.1): the function-scoped IR (D-13) supplies which locals each
    // block reads before writing, so they can cross the edges as parameters.
    let resume_free = instructions.iter().all(|i| supported(i.name()));
    let ir = Ir::build(verified, resume_free);
    let carried_locals = carried_local_sets(&ir, verified, locals_cacheable, args_cacheable);
    let dead_stores = dead_local_stores(
        &ir,
        instructions,
        resume_free && locals_cacheable && args_cacheable,
    );
    let mut state = State {
        blocks: HashMap::new(),
        pending: Vec::new(),
        feedback: &verified.snapshot().feedback,
        specialized: specialized_ops,
        speculate: verified.snapshot().speculate,
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
        // Epic F4 step 2: slots carried across the incoming edges arrive as
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
        // Epic S2 (1.1): re-seed the local cache from the carried parameters —
        // the successor's first read of a live-in local is a register now.
        for (slot, &v) in carried_locals.iter().zip(&params[carried.len()..]) {
            state.locals.fill(slot.is_arg, slot.idx, v);
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
        // D-6: the four call opcodes. The helper performs the whole JS call on
        // the current frame through `call_internal`, so the caller never exits
        // natively; `imm` carries the argument count (`call`/`call_method`/
        // `call_constructor`) or `apply`'s magic. `call_stack_helper` flushes
        // the operand stack and reloads `sp` from the frame (the helper popped
        // the call window and pushed the result).
        "call" | "call_method" | "call_constructor" | "apply" => {
            let operand = b.ins().iconst(types::I32, i64::from(insn.operand_u16(1)));
            b.ins().store(MemFlags::new(), operand, fv.frame, O_IMM);
            let id = match name {
                "call" => HELPER_CALL,
                "call_method" => HELPER_CALL_METHOD,
                "call_constructor" => HELPER_CALL_CONSTRUCTOR,
                _ => HELPER_APPLY,
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
            let id = helper_for(name).expect("with_* helper");
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
        _ => {}
    }

    // Epic N1: `div` always yields a `Float64` (Int/Int divides as `f64`), so
    // it gets a dedicated numeric arm instead of a `FastOp`. It is not a CSE
    // candidate (`is_cse_expression` stays exactly the `fast_op` set).
    if name == "div" {
        return lower_fast_div(state, b, fv, sp_var, helper_sig, sp, insn);
    }
    if let Some((op, helper_id)) = fast_op(name) {
        // Epic S2 (1.5): a repeat of an expression the same block already
        // computed over the same proven-numeric values. The result is still in
        // a register, and the copy *is* the instruction — no tag checks, no
        // arithmetic, no helper fallback.
        if let Some(reg) = state.cse_reuse(insn.pc()) {
            return lower_cse_reuse(state, b, sp, reg);
        }
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
            // The literal is an `Int` by construction: no tag check needed
            // wherever it is consumed (epic F4's guard elimination).
            let v = b.ins().iconst(types::I64, i64::from(insn.operand_u32(1)));
            Step::Continue(state.stack.push_int(b, sp, v))
        }
        "push_const" => {
            let idx = insn.operand_u32(1) as usize;
            let v = b
                .ins()
                .load(types::I64, MemFlags::new(), fv.cpools, (idx * 8) as i32);
            let is_int = state.const_meta.get(idx).is_some_and(|meta| meta.tag == 0);
            Step::Continue(if is_int {
                state.stack.push_int(b, sp, v)
            } else {
                state.stack.push(b, sp, v)
            })
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
            exit(state, b, fv, EXIT_RESUME, pc, sp);
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
            let bad = if name == "put_loc_check_init" {
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
            b.ins().brif(bad, slow, &[], ok, &[]);
            b.switch_to_block(slow);
            exit(state, b, fv, EXIT_RESUME, pc, sp);
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
        // Local/argument reads and writes keep a register copy (epic F4's
        // `LocalCache`): a repeated read in the same block skips its load, and
        // the store still happens, so memory stays authoritative.
        "get_loc" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state
                .locals
                .read(b, fv.var_base, false, idx, loc_offset_u16(insn));
            // Epic 1.4 item 3: a slot the verifier proves `Int32` is a known
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
        "get_arg" => {
            let idx = u32::from(insn.operand_u16(1));
            let v = state
                .locals
                .read(b, fv.arg_base, true, idx, loc_offset_u16(insn));
            Step::Continue(state.stack.push(b, sp, v))
        }
        "put_loc" => {
            let idx = u32::from(insn.operand_u16(1));
            let (sp, v) = state.stack.pop(b, sp);
            if state.dead_stores.contains(&pc) {
                // Epic S2 (1.2): the value is dead before any read, so the frame
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
                // Dead store (epic S2 / 1.2): see the `put_loc` arm.
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

/// The `tag == 0` (Int) test a binary fast path emits per unchecked operand.
fn is_int(b: &mut FunctionBuilder, value: Value) -> Value {
    let tag = b.ins().ushr_imm(value, 48);
    b.ins().icmp_imm(IntCC::Equal, tag, 0)
}

/// The `tag < Symbol(=-8) || tag > Exception(=6)` test that recognizes an
/// encoded `Float64` (`JSValueTag::is_float64`): the addend shift keeps every
/// real double's raw tag outside the non-float window.
fn is_float64(b: &mut FunctionBuilder, value: Value) -> Value {
    let tag = b.ins().sshr_imm(value, 48);
    let below = b.ins().icmp_imm(
        IntCC::SignedLessThan,
        tag,
        i64::from(crate::JSValueTag::Symbol as i32),
    );
    let above = b.ins().icmp_imm(
        IntCC::SignedGreaterThan,
        tag,
        i64::from(crate::JSValueTag::Exception as i32),
    );
    b.ins().bor(below, above)
}

/// Whether a value is one of the two numeric tags (`Int` or `Float64`), i.e.
/// the set the inline float path can convert to `f64` without running
/// `ToNumber`.
fn is_number(b: &mut FunctionBuilder, value: Value) -> Value {
    let int = is_int(b, value);
    let float = is_float64(b, value);
    b.ins().bor(int, float)
}

/// Unbox a value already known to be a `Float64`: the tagged bits plus the
/// addend are the double's raw bits (`js_value_get_float64`).
fn unbox_float64(b: &mut FunctionBuilder, value: Value) -> Value {
    let addend = b.ins().iconst(types::I64, FLOAT64_ADDEND);
    let bits = b.ins().iadd(value, addend);
    b.ins().bitcast(types::F64, MemFlags::new(), bits)
}

/// Convert an `Int`-tagged value to `f64`: its low 32 bits are the `i32`.
fn int_to_float64(b: &mut FunctionBuilder, value: Value) -> Value {
    let v = b.ins().ireduce(types::I32, value);
    b.ins().fcvt_from_sint(types::F64, v)
}

/// Convert a proven number to `f64`: widen an `Int` (known-int operands need no
/// check) or unbox a `Float64`. `is_float` is the runtime `is_float64` test for
/// an operand whose tag is not statically known.
fn number_to_float64(
    b: &mut FunctionBuilder,
    value: Value,
    known_int: bool,
    is_float: Option<Value>,
) -> Value {
    if known_int {
        return int_to_float64(b, value);
    }
    let from_float = unbox_float64(b, value);
    match is_float {
        Some(is_float) => {
            let from_int = int_to_float64(b, value);
            b.ins().select(is_float, from_float, from_int)
        }
        None => from_float,
    }
}

/// Box a computed `f64` exactly like `js_new_float64`: any NaN payload becomes
/// the engine's canonical quiet NaN, everything else is the raw bits minus the
/// addend (which turns `-0.0`/`±Inf` into their tagged form unchanged).
fn box_float64(b: &mut FunctionBuilder, d: Value) -> Value {
    let bits = b.ins().bitcast(types::I64, MemFlags::new(), d);
    let abs = b.ins().band_imm(bits, 0x7FFF_FFFF_FFFF_FFFF);
    let nan = b
        .ins()
        .icmp_imm(IntCC::UnsignedGreaterThan, abs, 0x7FF0_0000_0000_0000);
    let addend = b.ins().iconst(types::I64, FLOAT64_ADDEND);
    let encoded = b.ins().isub(bits, addend);
    let canonical = b.ins().iconst(types::I64, JS_NAN_U64_BITS);
    b.ins().select(nan, canonical, encoded)
}

/// Epic S2 (1.5): this instruction is a repeat — the same block computed the
/// same expression over the same proven-numeric values, and its result is still
/// in `reg`. Copying it into the result slot is the whole instruction: the tag
/// checks, the arithmetic and the helper fallback are all skipped, which makes
/// the reuse both smaller and cheaper than recomputing.
fn lower_cse_reuse(state: &mut State, b: &mut FunctionBuilder, sp: Value, reg: Value) -> Step {
    state.stack.flush(b, sp);
    b.ins().store(MemFlags::new(), reg, sp, -16);
    state.stack.consumed_binary();
    Step::Continue(b.ins().iadd_imm(sp, -8))
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
    // Epic S2 (1.5): a later instruction reuses this one's result, so it must
    // leave that value in a register (`cont` carries it as a parameter).
    let keep_id = state.cse_keep(insn.pc());
    // Epic F4 (guard elimination): an operand the compiler itself produced — a
    // `push_i32` literal or an `Int` constant — is statically an `Int`, so its
    // tag check is not emitted at all. Everything else (a local, a helper
    // result, `box_bit_result`, which may be a Float64) is still checked.
    let a_known = state.stack.known_int(top - 1);
    let c_known = state.stack.known_int(top);
    let a = state.stack.read(b, sp, top - 1);
    let c = state.stack.read(b, sp, top);
    let both_int = match (a_known, c_known) {
        (true, true) => None,
        (true, false) => Some(is_int(b, c)),
        (false, true) => Some(is_int(b, a)),
        (false, false) => {
            let int_a = is_int(b, a);
            let int_c = is_int(b, c);
            Some(b.ins().band(int_a, int_c))
        }
    };

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // A reused value has to leave this instruction in a register, so the
    // continuation carries it as a parameter (both paths deliver it).
    let kept = keep_id.is_some();
    if kept {
        b.append_block_param(cont, types::I64);
    }
    // Both paths must agree about memory. Everything except the two operands
    // materializes here, where the flush dominates the split; the operands are
    // dead on the fast path (the result overwrites one, the other lies above
    // the new `sp`) and are flushed by the helper's own preamble on the slow
    // path, which is the only path that still reads them.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    // Epic N1: `add`/`sub`/`mul` over two numbers where at least one is a
    // `Float64` get an inline arm too. Bitwise/shift/comparison ops stay
    // Int-only, and two operands the compiler proved Int never need it.
    let float_ok = matches!(op, FastOp::Add | FastOp::Sub | FastOp::Mul);
    let float = match both_int {
        Some(_) if float_ok => Some(b.create_block()),
        _ => None,
    };
    let else_target = float.unwrap_or(slow);
    match both_int {
        Some(both_int) => {
            b.ins().brif(both_int, fast, &[], else_target, &[]);
        }
        // Both operands are already known: the check is gone, but the split
        // stays so the overflow bail below still has its target.
        None => {
            let yes = b.ins().iconst(types::I8, 1);
            b.ins().brif(yes, fast, &[], slow, &[]);
        }
    }

    if let Some(float) = float {
        b.switch_to_block(float);
        // Not both Int here, so "both numbers" means at least one `Float64`. A
        // known-Int operand needs no check; the others must pass `is_number`.
        let a_num = (!a_known).then(|| is_number(b, a));
        let c_num = (!c_known).then(|| is_number(b, c));
        let both_num = match (a_num, c_num) {
            (Some(x), Some(y)) => b.ins().band(x, y),
            (Some(x), None) => x,
            (None, Some(y)) => y,
            (None, None) => unreachable!("both known Int went to the Int arm"),
        };
        let float_fast = b.create_block();
        b.ins().brif(both_num, float_fast, &[], slow, &[]);

        b.switch_to_block(float_fast);
        let a_float = (!a_known).then(|| is_float64(b, a));
        let c_float = (!c_known).then(|| is_float64(b, c));
        let af = number_to_float64(b, a, a_known, a_float);
        let cf = number_to_float64(b, c, c_known, c_float);
        let f = match op {
            FastOp::Add => b.ins().fadd(af, cf),
            FastOp::Sub => b.ins().fsub(af, cf),
            _ => b.ins().fmul(af, cf),
        };
        let boxed = box_float64(b, f);
        b.ins().store(MemFlags::new(), boxed, sp, -16);
        let sp_next = b.ins().iadd_imm(sp, -8);
        b.def_var(sp_var, sp_next);
        let keep = [boxed];
        b.ins().jump(cont, if kept { &keep } else { &[] });
    }

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
            let keep = [boxed];
            b.ins().jump(cont, if kept { &keep } else { &[] });
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
            let keep = [boxed];
            b.ins().jump(cont, if kept { &keep } else { &[] });
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
            let keep = [boxed];
            b.ins().jump(cont, if kept { &keep } else { &[] });
        }
    }

    b.switch_to_block(slow);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper_id, pc_after);
    b.def_var(sp_var, sp_slow);
    // The helper left the result in slot `top-1` (relative to `sp_slow`). The
    // placeholder is never read when nothing reuses this value (the jump passes
    // no argument then).
    let mut slow_result = [sp_slow];
    if kept {
        slow_result[0] = b.ins().load(types::I64, MemFlags::new(), sp_slow, -8);
    }
    b.ins().jump(cont, if kept { &slow_result } else { &[] });

    b.switch_to_block(cont);
    // Both paths store the result into slot `top-1`; the operand entries are
    // now stale (a flush would overwrite that result), so drop them.
    state.stack.consumed_binary();
    if let (Some(keep_id), Some(&value)) = (keep_id, b.block_params(cont).first()) {
        state.cse_regs.insert(keep_id, value);
    }
    // Epic 1.4 item 1: `shl`/`sar` box `From<i32>`, so *both* paths deliver an
    // `Int` — the merged slot is a known-Int fact with no phi needed.
    if matches!(op, FastOp::Shl | FastOp::Sar) {
        let sp_next = b.use_var(sp_var);
        let v = b.ins().load(types::I64, MemFlags::new(), sp_next, -8);
        return Step::Continue(state.stack.result_int(sp_next, v));
    }
    Step::Continue(b.use_var(sp_var))
}

/// Epic N1: inline `Float64` division. `div` always produces a `Float64`
/// (`Int/Int` divides as `f64` in the interpreter: `TwoNumbers::div`), so there
/// is no Int arm — any two proven numbers convert to `f64` and divide.
/// Everything else (BigInt, string, `ToPrimitive`) falls back to the helper.
/// The float-heavy kernels spend most of their arithmetic here.
#[allow(clippy::too_many_arguments)]
fn lower_fast_div(
    state: &mut State,
    b: &mut FunctionBuilder,
    fv: &FrameVals,
    sp_var: Variable,
    helper_sig: cranelift_codegen::ir::SigRef,
    sp: Value,
    insn: &Instruction,
) -> Step {
    let pc_after = insn.pc() + insn.size() as u32;
    let top = state.stack.height;
    let a_known = state.stack.known_int(top - 1);
    let c_known = state.stack.known_int(top);
    let a = state.stack.read(b, sp, top - 1);
    let c = state.stack.read(b, sp, top);

    let fast = b.create_block();
    let slow = b.create_block();
    let cont = b.create_block();
    // Like `lower_fast_binary`: materialize everything but the operands; the
    // helper's own preamble flushes those on the slow path.
    state.stack.flush_except(b, sp, &[top - 1, top]);
    let both_num = match (a_known, c_known) {
        (true, true) => None,
        (true, false) => Some(is_number(b, c)),
        (false, true) => Some(is_number(b, a)),
        (false, false) => {
            let a_num = is_number(b, a);
            let c_num = is_number(b, c);
            Some(b.ins().band(a_num, c_num))
        }
    };
    match both_num {
        Some(both_num) => {
            b.ins().brif(both_num, fast, &[], slow, &[]);
        }
        // Both operands are already known Int: Int/Int still divides through
        // `f64`, so this arm is the same as the float one.
        None => {
            let yes = b.ins().iconst(types::I8, 1);
            b.ins().brif(yes, fast, &[], slow, &[]);
        }
    }

    b.switch_to_block(fast);
    let a_float = (!a_known).then(|| is_float64(b, a));
    let c_float = (!c_known).then(|| is_float64(b, c));
    let af = number_to_float64(b, a, a_known, a_float);
    let cf = number_to_float64(b, c, c_known, c_float);
    let quotient = b.ins().fdiv(af, cf);
    let boxed = box_float64(b, quotient);
    b.ins().store(MemFlags::new(), boxed, sp, -16);
    let sp_next = b.ins().iadd_imm(sp, -8);
    b.def_var(sp_var, sp_next);
    b.ins().jump(cont, &[]);

    b.switch_to_block(slow);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_DIV, pc_after);
    b.def_var(sp_var, sp_slow);
    b.ins().jump(cont, &[]);

    b.switch_to_block(cont);
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
    // The new local value travels to `cont` so a later read in the block reuses
    // it (epic 1.4 item 2). Both paths write the same slot, so a phi is exact.
    b.append_block_param(cont, types::I64);
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
    b.ins().jump(cont, &[boxed.into()]);

    b.switch_to_block(slow);
    // Leave the operand on the stack and set `imm`; the helper pops it and
    // re-derives the full `arith_add` semantics (string concat, ToPrimitive, …).
    let imm = b.ins().iconst(types::I32, i64::from(idx));
    b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
    // P7 7.2 instrumentation: mark this helper call as the bail of the int fast
    // path, so the helper tallies it as a guard miss.
    let miss = b.ins().iconst(types::I32, i64::from(SPEC_MISS_IMM2 as i32));
    b.ins().store(MemFlags::new(), miss, fv.frame, O_IMM2);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, HELPER_ADD_LOC, pc_after);
    b.def_var(sp_var, sp_slow);
    let slow_local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    b.ins().jump(cont, &[slow_local.into()]);

    b.switch_to_block(cont);
    // Pop2/push1 would be wrong here: this op only pops the operand — the
    // result went into the *local* (fast path stores it, helper writes it).
    state.stack.drop_top();
    state
        .locals
        .fill(false, idx as u32, b.block_params(cont)[0]);
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
    // Carry the new local value across the merge for a later read (epic 1.4
    // item 2).
    b.append_block_param(cont, types::I64);
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
    b.ins().jump(cont, &[boxed.into()]);

    b.switch_to_block(slow);
    let imm = b.ins().iconst(types::I32, i64::from(idx));
    b.ins().store(MemFlags::new(), imm, fv.frame, O_IMM);
    // P7 7.2 instrumentation: mark this helper call as the bail of the int fast
    // path, so the helper tallies it as a guard miss.
    let miss = b.ins().iconst(types::I32, i64::from(SPEC_MISS_IMM2 as i32));
    b.ins().store(MemFlags::new(), miss, fv.frame, O_IMM2);
    let sp_slow = call_stack_helper(state, b, fv, helper_sig, sp, helper_id, pc_after);
    b.def_var(sp_var, sp_slow);
    let slow_local = b
        .ins()
        .load(types::I64, MemFlags::new(), fv.var_base, byte_off);
    b.ins().jump(cont, &[slow_local.into()]);

    b.switch_to_block(cont);
    state
        .locals
        .fill(false, idx as u32, b.block_params(cont)[0]);
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

/// `with_*`: call the resolver helper, then branch to the instruction's label
/// when it handled the name and fall through to the ordinary variable access
/// otherwise. The helper owns both arms' stack shapes (it leaves `sp` at the
/// shape of whichever path it reports), so the code loads `sp` back and hands
/// the same value to either edge; only the status picks the edge.
#[allow(clippy::too_many_arguments)]
fn lower_with(
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
    state.stack.flush(b, sp);
    b.ins().store(MemFlags::new(), sp, fv.frame, O_SP);
    write_cur_sp(b, fv, sp);
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
    exit_status(b, fv, status, pc_after, sp);
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

    /// Compiled code size for `bytes`, with one constant pool entry of tag
    /// `const_tag` (0 = Int, 8 = Float64).
    fn compile_len(bytes: Vec<u8>, vars: u16, const_tag: i16) -> usize {
        let mut snap = snapshot(bytes, 0, vars);
        snap.const_meta = Box::new([ConstMeta { tag: const_tag }]);
        let isa = crate::jit::build_host_isa().unwrap();
        let verified = verify(snap, VerifyLimits::default())
            .unwrap_or_else(|e| panic!("verify rejected: {e:?}"));
        compile(&verified, isa.as_ref(), &Cell::new(0))
            .expect("compile")
            .len()
    }

    /// Epic S2 (1.5): a repeated numeric expression is computed once — the
    /// second occurrence copies the register the first one left, so its tag
    /// checks, arithmetic and helper fallback disappear. The same shape over a
    /// *different* local cannot reuse anything and keeps all of that.
    #[test]
    fn cse_shrinks_the_code() {
        // The store gives the local a numeric fact (`push_i32`), which is what
        // the pass needs before it may claim an operand pair is pure.
        let literal = |idx: u16, value: i32| seq(&[&push_i32(value), &loc(op::OP_put_loc, idx)]);
        let shape = |second: u16| {
            seq(&[
                &literal(0, 1),
                &literal(1, 2),
                &loc(op::OP_get_loc, 0),
                &loc(op::OP_get_loc, 0),
                &[op::OP_add],
                &loc(op::OP_get_loc, second),
                &loc(op::OP_get_loc, second),
                &[op::OP_add],
                &[op::OP_add],
                &loc(op::OP_put_loc, 2),
                &[op::OP_return_undef],
            ])
        };
        // `(x+x) + (x+x)` reuses; `(x+x) + (y+y)` cannot.
        let reused = compile_len(shape(0), 3, 0);
        let distinct = compile_len(shape(1), 3, 0);
        assert!(
            reused < distinct,
            "the reused expression should be a copy: {reused} vs {distinct} bytes"
        );
    }

    /// Epic F4 (guard elimination), measured deterministically: the same op
    /// sequence needs a tag check for an operand that is *not* statically known.
    /// An `Int` constant from the pool is known, a `Float64` one is not, so the
    /// two compilations differ exactly by the check the second one emits.
    #[test]
    fn guard_elimination_shrinks_the_code() {
        let bytes = seq(&[
            &loc(op::OP_get_loc, 0),
            &[op::OP_push_const],
            &u32b(0),
            &[op::OP_mul],
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]);
        let known = compile_len(bytes.clone(), 1, 0);
        let checked = compile_len(bytes, 1, 8);
        assert!(
            known < checked,
            "the known-Int operand skips its check: {known} vs {checked} bytes"
        );
    }

    /// Epic 1.4 item 1: `shl`/`sar` box `From<i32>` on *both* paths, so their
    /// merged result is a known `Int` and the consumer skips its tag check.
    /// `shr` boxes `From<u32>` (possibly a Float64), so the identical shape is
    /// bigger by exactly that check.
    #[test]
    fn shift_results_are_known_int() {
        let shape = |op: u8| {
            seq(&[
                &push_i32(3),
                &push_i32(1),
                &[op],
                &[op::OP_push_const],
                &u32b(0),
                &[op::OP_add],
                &loc(op::OP_put_loc, 0),
                &[op::OP_return_undef],
            ])
        };
        let shl = compile_len(shape(op::OP_shl), 1, 0);
        let shr = compile_len(shape(op::OP_shr), 1, 0);
        assert!(
            shl < shr,
            "a shift result skips the consumer's check: {shl} vs {shr} bytes"
        );
    }

    /// Epic S2 (1.6): a local's `Int32` proof survives a **loop header** when
    /// every edge into it agrees, so a loop-invariant bound is known-Int on every
    /// iteration and the comparison in the header needs no tag check.
    ///
    /// The two compilations are the *same bytes* and differ only in the tag the
    /// pool constant carries (0 = Int, 8 = Float64), so the byte gap is exactly
    /// the check the cross-block fact removes. Before 1.6 the header was widened
    /// on the way in, and both shapes compiled identically.
    #[test]
    fn a_loop_invariant_int_bound_keeps_its_fact() {
        // i = 0; limit = <const>;                        (preheader)
        // L: get_loc i; get_loc limit; lt; if_false E;   (header)
        //    get_loc limit; push_i32 1; add; put_loc 3;  (body: sum up)
        //    inc_loc i; goto L;
        // E: return_undef
        let shape = |preheader: Vec<u8>| {
            let mut bytes = preheader;
            let header = bytes.len() as i32;
            bytes.extend_from_slice(&loc(op::OP_get_loc, 0));
            bytes.extend_from_slice(&loc(op::OP_get_loc, 1));
            bytes.extend_from_slice(&[op::OP_lt]);
            let if_pc = bytes.len() as i32;
            bytes.push(op::OP_if_false);
            bytes.extend_from_slice(&0i32.to_le_bytes());
            let body = bytes.len() as i32;
            bytes.extend_from_slice(&loc(op::OP_get_loc, 1));
            bytes.extend_from_slice(&push_i32(1));
            bytes.push(op::OP_add);
            bytes.extend_from_slice(&loc(op::OP_put_loc, 3));
            bytes.extend_from_slice(&loc8(op::OP_inc_loc, 0));
            let goto_pc = bytes.len() as i32;
            bytes.push(op::OP_goto);
            bytes.extend_from_slice(&0i32.to_le_bytes());
            let end = bytes.len() as i32;
            bytes.push(op::OP_return_undef);
            // Branch offsets are relative to the byte after the opcode.
            bytes[(if_pc as usize + 1)..(if_pc as usize + 5)]
                .copy_from_slice(&(end - (if_pc + 1)).to_le_bytes());
            bytes[(goto_pc as usize + 1)..(goto_pc as usize + 5)]
                .copy_from_slice(&(header - (goto_pc + 1)).to_le_bytes());
            assert!(body < end);
            bytes
        };
        // Both shapes are the *same* bytes: `limit` is stored from a pool constant,
        // and only the constant's tag differs (0 = Int, 8 = Float64). The gap is
        // exactly the checks the cross-block fact removes from the header.
        let body = shape(seq(&[
            &push_i32(0),
            &loc(op::OP_put_loc, 0),
            &[op::OP_push_const],
            &u32b(0),
            &loc(op::OP_put_loc, 1),
        ]));
        let proven = compile_len(body.clone(), 4, 0);
        let unproven = compile_len(body, 4, 8);
        assert!(
            proven < unproven,
            "a loop-invariant Int bound keeps its fact: {proven} vs {unproven} bytes"
        );
    }

    /// Epic 1.4 item 3: a local the verifier proves `Int32` carries the fact
    /// through `get_loc`, so its consumer skips a check. The two shapes are the
    /// same bytecode; only the initializing producer differs, so the byte-size
    /// gap is the checks the `Int32` proof removes.
    #[test]
    fn proven_int_local_skips_the_check() {
        let tail = seq(&[
            &loc(op::OP_get_loc, 0),
            &[op::OP_push_const],
            &u32b(0),
            &[op::OP_mul],
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]);
        // `push_i32` proves the local `Int32`; a non-numeric `push_const` (tag
        // 2) leaves it `Tagged` and its constant operand unknown too.
        let int_local = seq(&[&push_i32(5), &loc(op::OP_put_loc, 0), &tail]);
        let tagged_local = seq(&[
            &[op::OP_push_const],
            &u32b(0),
            &loc(op::OP_put_loc, 0),
            &tail,
        ]);
        let proven = compile_len(int_local, 2, 0);
        let unproven = compile_len(tagged_local, 2, 2);
        assert!(
            proven < unproven,
            "a proven-Int local feeds the guard fact: {proven} vs {unproven} bytes"
        );
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

    /// `get_array_el*`/`put_array_el` encoding: `[op]`, plus `[ic_idx:u16]` when
    /// the inline cache is compiled in.
    fn array_el(opcode: u8) -> Vec<u8> {
        let mut out = vec![opcode];
        #[cfg(feature = "inline_cache")]
        out.extend_from_slice(&u16b(0));
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
        // Frame-entry TDZ prologue and its guarded read.
        cases.push((
            "set_loc_uninitialized",
            seq(&[
                &loc(op::OP_set_loc_uninitialized, 0),
                &[op::OP_return_undef],
            ]),
            0,
            1,
        ));
        cases.push((
            "get_loc_check",
            seq(&[&loc(op::OP_get_loc_check, 0), &ret]),
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
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &array_el(op::OP_get_array_el),
                &ret,
            ]),
            0,
            0,
        ));
        cases.push((
            "get_array_el2",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &array_el(op::OP_get_array_el2),
                &ret,
            ]),
            0,
            0,
        ));
        cases.push((
            "get_array_el3",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &array_el(op::OP_get_array_el3),
                &ret,
            ]),
            0,
            0,
        ));
        cases.push((
            "put_array_el",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &push_i32(0),
                &array_el(op::OP_put_array_el),
                &[op::OP_return_undef],
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
        // `rest` spends its u16 operand to index the helper's argument window.
        cases.push(("rest", seq(&[&[op::OP_rest, 1, 0], &ret]), 2, 0));
        // Literal seeds: `object` pushes an empty base object; `array_from`
        // pops its NPOP operand and pushes one array.
        cases.push(("object", seq(&[&[op::OP_object, op::OP_return]]), 0, 0));
        cases.push((
            "array_from",
            seq(&[
                &push_i32(1),
                &push_i32(2),
                &[op::OP_array_from],
                &u16b(2),
                &ret,
            ]),
            0,
            0,
        ));
        // `??`'s guard: lowered inline (the tag of the top slot), not resumed.
        cases.push((
            "is_undefined_or_null",
            seq(&[&push_i32(1), &[op::OP_is_undefined_or_null], &ret]),
            0,
            0,
        ));
        // Frame-entry special objects (kind 3 = `new.target`) and sloppy `this`.
        cases.push((
            "special_object",
            seq(&[&[op::OP_special_object, 3], &ret]),
            0,
            0,
        ));
        // `set_name` peeks the function it names (atom operand).
        cases.push((
            "set_name",
            seq(&[&push_i32(0), &[op::OP_set_name], &u32b(0), &ret]),
            0,
            0,
        ));
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

        // Sync iteration (S3 2.4.1): real `for-of`/`for-in` shapes. The
        // iterator window is [iter_obj, next, catch_offset]; `for_of_next`
        // appends value/done and `iterator_close` drops the window.
        cases.push((
            "for_of_start",
            seq(&[
                &push_i32(0),
                &[op::OP_for_of_start],
                &[op::OP_iterator_close, op::OP_return_undef],
            ]),
            0,
            0,
        ));
        cases.push((
            "for_of_next",
            seq(&[
                &push_i32(0),
                &[op::OP_for_of_start],
                &[op::OP_for_of_next, 0],
                &[op::OP_drop, op::OP_drop],
                &[op::OP_iterator_close, op::OP_return_undef],
            ]),
            0,
            0,
        ));
        cases.push((
            "for_in",
            seq(&[
                &push_i32(0),
                &[op::OP_for_in_start, op::OP_for_in_next],
                &[op::OP_drop, op::OP_drop, op::OP_drop],
                &[op::OP_return_undef],
            ]),
            0,
            0,
        ));
        cases.push((
            "iterator_get_value_done",
            seq(&[
                &push_i32(0),
                &[op::OP_iterator_get_value_done],
                &[op::OP_drop, op::OP_drop, op::OP_return_undef],
            ]),
            0,
            0,
        ));
        cases.push((
            "iterator_check_object",
            seq(&[
                &push_i32(0),
                &[
                    op::OP_iterator_check_object,
                    op::OP_drop,
                    op::OP_return_undef,
                ],
            ]),
            0,
            0,
        ));
        cases.push((
            "iterator_next",
            seq(&[
                &push_i32(0),
                &[op::OP_for_of_start],
                &push_i32(0),
                &[op::OP_iterator_next, op::OP_drop],
                &[op::OP_iterator_close, op::OP_return_undef],
            ]),
            0,
            0,
        ));
        cases.push((
            "iterator_call",
            seq(&[
                &push_i32(0),
                &[op::OP_for_of_start],
                &push_i32(0),
                &[op::OP_iterator_call, 0, op::OP_drop, op::OP_drop],
                &[op::OP_iterator_close, op::OP_return_undef],
            ]),
            0,
            0,
        ));

        // Class / object-literal / private-field machinery (S3 2.4.3). Each
        // entry pushes as many operands as the opcode pops, applies the opcode,
        // then drops whatever it left (so a `pops`/`pushes` mismatch fails to
        // even verify).
        let class_cases: Vec<(&str, Vec<u8>, usize, usize)> = vec![
            ("check_ctor", seq(&[&[op::OP_check_ctor]]), 0, 0),
            ("init_ctor", seq(&[&[op::OP_init_ctor]]), 0, 1),
            (
                "private_symbol",
                seq(&[&[op::OP_private_symbol], &u32b(0)]),
                0,
                1,
            ),
            ("get_super", seq(&[&[op::OP_get_super]]), 1, 1),
            ("private_in", seq(&[&[op::OP_private_in]]), 2, 1),
            ("check_brand", seq(&[&[op::OP_check_brand]]), 2, 2),
            ("add_brand", seq(&[&[op::OP_add_brand]]), 2, 0),
            (
                "get_private_field",
                seq(&[&[op::OP_get_private_field]]),
                2,
                1,
            ),
            (
                "define_field",
                seq(&[&[op::OP_define_field], &u32b(0)]),
                2,
                1,
            ),
            (
                "set_name_computed",
                seq(&[&[op::OP_set_name_computed]]),
                2,
                2,
            ),
            ("set_proto", seq(&[&[op::OP_set_proto]]), 2, 1),
            ("set_home_object", seq(&[&[op::OP_set_home_object]]), 2, 2),
            (
                "define_method",
                seq(&[&[op::OP_define_method], &u32b(0), &[0]]),
                2,
                1,
            ),
            (
                "define_class",
                seq(&[&[op::OP_define_class], &u32b(0), &[0]]),
                2,
                2,
            ),
            (
                "copy_data_properties",
                seq(&[&[op::OP_copy_data_properties, 0]]),
                3,
                3,
            ),
            ("define_array_el", seq(&[&[op::OP_define_array_el]]), 3, 2),
            ("append", seq(&[&[op::OP_append]]), 3, 2),
            (
                "put_private_field",
                seq(&[&[op::OP_put_private_field]]),
                3,
                0,
            ),
            (
                "define_private_field",
                seq(&[&[op::OP_define_private_field]]),
                3,
                1,
            ),
            (
                "define_method_computed",
                seq(&[&[op::OP_define_method_computed, 0]]),
                3,
                1,
            ),
            (
                "define_class_computed",
                seq(&[&[op::OP_define_class_computed], &u32b(0), &[0]]),
                3,
                3,
            ),
            ("get_super_value", seq(&[&[op::OP_get_super_value]]), 3, 1),
            ("put_super_value", seq(&[&[op::OP_put_super_value]]), 4, 0),
            ("close_loc", seq(&[&loc(op::OP_close_loc, 0)]), 0, 0),
        ];
        for (name, op_bytes, pops, pushes) in class_cases {
            let mut bytes = Vec::new();
            for i in 0..pops {
                bytes.extend_from_slice(&push_i32(i as i32));
            }
            bytes.extend_from_slice(&op_bytes);
            for _ in 0..pushes {
                bytes.push(op::OP_drop);
            }
            bytes.push(op::OP_return_undef);
            cases.push((name, bytes, 0, 1));
        }

        // Ref-cell family (S3 2.5): `make_*_ref` push the ref object and its
        // property name; `get`/`put_ref_value` read/write through them. The
        // indices must fit their space (local/argument/closure).
        cases.push((
            "make_loc_ref",
            seq(&[
                &[op::OP_make_loc_ref],
                &u32b(0),
                &u16b(0),
                &[op::OP_drop, op::OP_drop, op::OP_return_undef],
            ]),
            0,
            1,
        ));
        cases.push((
            "make_arg_ref",
            seq(&[
                &[op::OP_make_arg_ref],
                &u32b(0),
                &u16b(0),
                &[op::OP_drop, op::OP_drop, op::OP_return_undef],
            ]),
            1,
            0,
        ));
        cases.push((
            "make_var_ref",
            seq(&[
                &[op::OP_make_var_ref],
                &u32b(0),
                &[op::OP_drop, op::OP_drop, op::OP_return_undef],
            ]),
            0,
            0,
        ));
        cases.push((
            "get_ref_value",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &[
                    op::OP_get_ref_value,
                    op::OP_drop,
                    op::OP_drop,
                    op::OP_return_undef,
                ],
            ]),
            0,
            0,
        ));
        cases.push((
            "put_ref_value",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &push_i32(0),
                &[op::OP_put_ref_value, op::OP_return_undef],
            ]),
            0,
            0,
        ));

        // `with_*` (S3 2.4): the environment object(s) are the entry window, the
        // label is the hit edge, and the fall-through runs the ordinary variable
        // access the opcode guards. That access is sized so both edges reach the
        // label at the same height.
        for &(name, code, pops, pushed) in &[
            ("with_get_var", op::OP_with_get_var, 1, 1),
            // Its fall-through leaves the value `put_loc`/`set_loc` would take;
            // drop it, since the hit edge consumed the value too.
            ("with_put_var", op::OP_with_put_var, 2, 0),
            ("with_delete_var", op::OP_with_delete_var, 1, 1),
            ("with_make_ref", op::OP_with_make_ref, 1, 2),
            ("with_get_ref", op::OP_with_get_ref, 1, 2),
        ] {
            let access: Vec<u8> = if name == "with_put_var" {
                vec![op::OP_drop]
            } else {
                (0..pushed).flat_map(|i| push_i32(i as i32)).collect()
            };
            let mut bytes = Vec::new();
            for i in 0..pops {
                bytes.extend_from_slice(&push_i32(i as i32));
            }
            bytes.push(code);
            bytes.extend_from_slice(&u32b(0));
            // The label sits `5 + access.len()` bytes after the operand base.
            bytes.extend_from_slice(&i32b(5 + access.len() as i32));
            bytes.push(1);
            let label = bytes.len() + access.len();
            bytes.extend_from_slice(&access);
            bytes.push(op::OP_return_undef);
            assert_eq!(bytes.len(), label + 1);
            cases.push((name, bytes, 0, 0));
        }

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

        // The engine-generated opcodes (todo 2.6). `throw` pops its operand and
        // ends the block; `throw_error` moves nothing. `regexp` replaces the
        // slot below its id, `push_bigint_i32` pushes one value.
        cases.push(("throw", seq(&[&push_i32(1), &[op::OP_throw]]), 0, 0));
        let mut throw_error = vec![op::OP_throw_error];
        throw_error.extend_from_slice(&u32b(0));
        throw_error.push(0);
        cases.push(("throw_error", seq(&[&throw_error]), 0, 0));
        cases.push((
            "regexp",
            seq(&[&push_i32(0), &push_i32(0), &[op::OP_regexp], &ret]),
            0,
            0,
        ));
        let mut push_bigint = vec![op::OP_push_bigint_i32];
        push_bigint.extend_from_slice(&i32b(-7));
        cases.push(("push_bigint_i32", seq(&[&push_bigint, &ret]), 0, 0));

        // D-6 call opcodes: the helper takes the whole call window off the
        // operand stack, so just enough operands are pushed for the verifier.
        // `call`/`call_method`/`call_constructor` carry an NPOP argc (0 here);
        // `apply` a `U16` magic.
        cases.push((
            "call",
            seq(&[&push_i32(0), &[op::OP_call], &u16b(0), &ret]),
            0,
            0,
        ));
        cases.push((
            "call_method",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &[op::OP_call_method],
                &u16b(0),
                &ret,
            ]),
            0,
            0,
        ));
        cases.push((
            "call_constructor",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &[op::OP_call_constructor],
                &u16b(0),
                &ret,
            ]),
            0,
            0,
        ));
        cases.push((
            "apply",
            seq(&[
                &push_i32(0),
                &push_i32(0),
                &push_i32(0),
                &[op::OP_apply],
                &u16b(0),
                &ret,
            ]),
            0,
            0,
        ));

        for (name, bytes, args, vars) in cases {
            compile_bytes(bytes, args, vars)
                .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }

        // A long straight-line block full of calls must not get one post-call OSR
        // entry per call.
        //
        // `block_range` hands a mid-block entry the whole tail of its block, and
        // `block_for` gives that tail a Cranelift block of its own, so every copy
        // stays in the compiled code: a block with N calls costs O(N x length).
        // Unbounded, that pushed a real test262 function (658-instruction block,
        // 116 `call_method`s) to 10.5M IR instructions, 31 s of Cranelift, and a
        // `CodeTooLarge` that cost it native code entirely. Entries are a
        // performance affordance — a dropped call site resumes in the
        // interpreter — so they are what gives way.
        const CALLS: usize = 40;
        let mut heavy = Vec::new();
        for _ in 0..CALLS {
            heavy.extend_from_slice(&push_i32(0)); // func
            heavy.extend_from_slice(&push_i32(0)); // this
            heavy.push(op::OP_call_method);
            heavy.extend_from_slice(&u16b(0)); // argc
        }
        heavy.push(op::OP_return_undef);

        let verified = verify(
            CompileSnapshot::from_raw(heavy, 0, 0, 0, CALLS as u16 + 4),
            VerifyLimits::default(),
        )
        .expect("a straight-line block of calls verifies");
        let pcs = collect_osr_pcs(&verified);
        assert!(
            !pcs.is_empty(),
            "the first call still hands its tail back to native code"
        );
        assert!(
            pcs.len() <= 8,
            "{CALLS} calls in one block yielded {} OSR entries",
            pcs.len()
        );
        let isa = crate::jit::build_host_isa().unwrap();
        compile(&verified, isa.as_ref(), &Cell::new(0))
            .expect("a call-heavy straight-line block compiles");

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

        // `delete name` on the global object: the fall-through of `with_delete_var`,
        // so a `with` body's `delete` runs natively too.
        compile_bytes(
            seq(&[
                &[op::OP_delete_var],
                &u32b(0),
                &[op::OP_drop, op::OP_return_undef],
            ]),
            0,
            0,
        )
        .unwrap_or_else(|e| panic!("opcode delete_var failed to lower: {e:?}"));

        // Closure var refs (epic 2.1) need a non-zero closure-var count.
        for (name, code) in [
            ("get_var_ref", op::OP_get_var_ref),
            ("get_var_ref_check", op::OP_get_var_ref_check),
        ] {
            compile_bytes_gv(seq(&[&loc(code, 0), &ret]), 0, 0, 1)
                .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }
        for (name, code) in [
            ("put_var_ref", op::OP_put_var_ref),
            ("put_var_ref_check", op::OP_put_var_ref_check),
            ("put_var_ref_check_init", op::OP_put_var_ref_check_init),
        ] {
            compile_bytes_gv(
                seq(&[&push_i32(0), &loc(code, 0), &[op::OP_return_undef]]),
                0,
                0,
                1,
            )
            .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }
        compile_bytes_gv(
            seq(&[&push_i32(0), &loc(op::OP_set_var_ref, 0), &ret]),
            0,
            0,
            1,
        )
        .unwrap_or_else(|e| panic!("opcode set_var_ref failed to lower: {e:?}"));

        // `make_var_ref_ref` (S3 2.5) needs the closure-var space to be non-empty.
        compile_bytes_gv(
            seq(&[
                &[op::OP_make_var_ref_ref],
                &u32b(0),
                &u16b(0),
                &[op::OP_drop, op::OP_drop, op::OP_return_undef],
            ]),
            0,
            0,
            1,
        )
        .unwrap_or_else(|e| panic!("opcode make_var_ref_ref failed to lower: {e:?}"));

        // TDZ-checked local ops (epic 2.3).
        for (name, code) in [
            ("put_loc_check", op::OP_put_loc_check),
            ("put_loc_check_init", op::OP_put_loc_check_init),
        ] {
            compile_bytes(
                seq(&[
                    &loc(op::OP_set_loc_uninitialized, 0),
                    &push_i32(0),
                    &loc(code, 0),
                    &[op::OP_return_undef],
                ]),
                0,
                1,
            )
            .unwrap_or_else(|e| panic!("opcode {name} failed to lower: {e:?}"));
        }
        compile_bytes(
            seq(&[
                &loc(op::OP_set_loc_uninitialized, 0),
                &push_i32(0),
                &loc(op::OP_set_loc_check, 0),
                &ret,
            ]),
            0,
            1,
        )
        .unwrap_or_else(|e| panic!("opcode set_loc_check failed to lower: {e:?}"));
        compile_bytes(seq(&[&loc(op::OP_get_loc_checkthis, 0), &ret]), 0, 1)
            .unwrap_or_else(|e| panic!("opcode get_loc_checkthis failed to lower: {e:?}"));
    }

    /// Epic 2.3: policy promises `Native` must mean "the baseline lowers it".
    /// Every opcode the generated table calls `Native` must pass [`supported`];
    /// otherwise it silently degrades to an instruction-level `Resume` (the
    /// function compiles, but re-enters the interpreter at that op). This is
    /// the guard that stops a new `Native` opcode from being added without a
    /// lowering.
    #[test]
    fn every_native_policy_opcode_is_lowered() {
        use crate::jit::{JitPolicy, opcode_names, opcode_policies};
        let missing: Vec<&str> = opcode_names()
            .iter()
            .zip(opcode_policies())
            .filter(|(name, policy)| **policy == JitPolicy::Native && !supported(name))
            .map(|(name, _)| *name)
            .collect();
        assert!(
            missing.is_empty(),
            "Native opcodes with no lowering: {missing:?}"
        );
    }

    /// `Helper` opcodes the baseline deliberately does **not** lower. Empty
    /// since D-6 lowered the last four (`call`/`call_method`/`call_constructor`/
    /// `apply`); every `Helper` opcode now has a lowering. `delete_var` was in
    /// this set by accident until 2.6 lowered it —
    /// `every_native_policy_opcode_is_lowered` cannot catch that class of
    /// mistake because it only guards `Native`.
    #[test]
    fn every_helper_policy_opcode_is_lowered_or_pinned() {
        use crate::jit::{JitPolicy, opcode_names, opcode_policies};
        /// `Helper` policy, no lowering, intentional. A new entry must carry the
        /// reason it still exits the native frame.
        const PINNED: &[&str] = &[];
        let mut missing: Vec<&str> = opcode_names()
            .iter()
            .zip(opcode_policies())
            .filter(|(name, policy)| **policy == JitPolicy::Helper && !supported(name))
            .map(|(name, _)| *name)
            .collect();
        missing.sort_unstable();
        assert_eq!(
            missing, PINNED,
            "Helper opcodes without a lowering changed: add the lowering, or pin the \
             name here with the reason it still exits the native frame"
        );
    }

    /// D-6 completes the coverage: every opcode the verifier accepts is lowered,
    /// so the `Resume` fallback in `lower_insn` is unreachable for verified
    /// bytecode. The only unsupported names are policy `Reject`, which `verify`
    /// refuses before lowering (this test pins that equality so a new `Reject`
    /// that verifies — or a new supported-but-unlowered opcode — is caught).
    #[test]
    fn every_opcode_the_verifier_accepts_is_lowered() {
        use crate::jit::{JitPolicy, opcode_names, opcode_policies};
        let unsupported: Vec<&str> = opcode_names()
            .iter()
            .zip(opcode_policies())
            .filter(|(name, _)| !supported(name))
            .map(|(name, _)| *name)
            .collect();
        let rejected: Vec<&str> = opcode_names()
            .iter()
            .zip(opcode_policies())
            .filter(|(_, policy)| **policy == JitPolicy::Reject)
            .map(|(name, _)| *name)
            .collect();
        assert_eq!(
            unsupported, rejected,
            "the unsupported set must be exactly the `Reject` policy set"
        );
    }

    /// Every shuffle's JIT permutation must equal the frontend's model in
    /// `jit/bytecode/stack.rs::copied_stack_values`, which is written against
    /// the QuickJS interpreter arms (`quickjs-opcode.h`: `perm4: obj prop a b
    /// -> a obj prop b`). `perm4`/`perm5` silently drifted here once: no JS
    /// case reached them, so the wrong table survived until the opcodes got
    /// end-to-end coverage (2026-10-17).
    #[test]
    fn shuffle_table_matches_the_verifier() {
        // Five distinct kinds stand in for the slots, so a permutation that
        // reorders them is visible.
        let kinds = [
            SlotKind::Tagged,
            SlotKind::Int32,
            SlotKind::Float64,
            SlotKind::CatchOffset,
            SlotKind::Uninitialized,
        ];
        // `shuffle_opcodes` is the single source of truth: it must name exactly
        // the opcodes the dispatch below handles, so neither can drift.
        for name in crate::jit::opcode_names() {
            assert_eq!(
                shuffle_opcodes().contains(name),
                shuffle_outputs(name).is_some(),
                "{name}: `shuffle_opcodes` and the table disagree"
            );
        }
        for name in shuffle_opcodes() {
            let (pop, outputs) = shuffle_outputs(name).expect("known shuffle");
            let popped = &kinds[..pop];
            let modeled = crate::jit::bytecode::stack::copied_stack_values(name, popped)
                .unwrap_or_else(|| panic!("{name} has no verifier model"));
            assert_eq!(outputs.len(), modeled.len(), "{name}: output count");
            for (slot, &source) in outputs.iter().enumerate() {
                assert_eq!(
                    modeled[slot], popped[source],
                    "{name}: output {slot} takes the wrong operand"
                );
            }
        }
    }

    /// A small pre-tested/loop/body/exit function: `i = 0`; `while (i < 3) i++`.
    /// The header reads `i` before writing it, so 1.1 carries it; the body
    /// writes it, so the back edge re-carries the written value.
    fn loop_bytes() -> Vec<u8> {
        seq(&[
            &push_i32(0),                 // 0..5
            &loc(op::OP_put_loc, 0),      // 5..8
            &branch(op::OP_goto, 4),      // 8..13 -> header at 13
            &loc(op::OP_get_loc, 0),      // 13..16
            &push_i32(3),                 // 16..21
            &[op::OP_lt],                 // 21..22
            &branch(op::OP_if_false, 11), // 22..27 -> end at 34
            &[op::OP_inc_loc, 0],         // 27..29
            &branch(op::OP_goto, -17),    // 29..34 -> header
            &[op::OP_return_undef],       // 34
        ])
    }

    fn verified_local(bytes: Vec<u8>, args: u16, vars: u16) -> VerifiedFunction {
        verify(snapshot(bytes, args, vars), VerifyLimits::default())
            .unwrap_or_else(|e| panic!("verify rejected: {e:?}"))
    }

    /// Epic S2 (1.1): the loop header reads `i` before writing it, so the block
    /// declares it carried; a block that writes before reading does not.
    #[test]
    fn the_loop_header_carries_its_induction_local() {
        let verified = verified_local(loop_bytes(), 0, 1);
        let ir = Ir::build(&verified, true);
        let carried = carried_local_sets(&ir, &verified, true, true);
        // Header at pc 13.
        assert_eq!(
            carried.get(&13).cloned(),
            Some(vec![LocalRef::local(0)]),
            "the header reads `i` before writing it"
        );
        // The entry block has no incoming edge, so it carries nothing.
        assert!(!carried.contains_key(&0));
    }

    /// Epic S2 (1.2): an unread local store is dead; a read one is not; and the
    /// gate (no resume points / exact liveness) turns the pass off entirely.
    #[test]
    fn dead_local_stores_are_elided_only_when_read_free() {
        // push_i32 1; put_loc 0; push_i32 2; put_loc 1; return_undef
        let dead = seq(&[
            &push_i32(1),
            &loc(op::OP_put_loc, 0),
            &push_i32(2),
            &loc(op::OP_put_loc, 1),
            &[op::OP_return_undef],
        ]);
        let verified = verified_local(dead, 0, 2);
        let ir = Ir::build(&verified, true);
        let hints = dead_local_stores(&ir, verified.instructions(), true);
        assert!(hints.contains(&5), "put_loc 0 at pc 5 is never read");
        assert!(hints.contains(&13), "put_loc 1 at pc 13 is never read");
        // The gate being closed yields no hints at all.
        assert!(dead_local_stores(&ir, verified.instructions(), false).is_empty());

        // push_i32 1; put_loc 0; get_loc 0; return -> the store is live.
        let live = seq(&[
            &push_i32(1),
            &loc(op::OP_put_loc, 0),
            &loc(op::OP_get_loc, 0),
            &[op::OP_return],
        ]);
        let verified = verified_local(live, 0, 1);
        let ir = Ir::build(&verified, true);
        let hints = dead_local_stores(&ir, verified.instructions(), true);
        assert!(!hints.contains(&5), "put_loc 0 at pc 5 is read back");
    }

    /// Epic P7 (task 7.1): a site whose *result* was observed as `Float64` (an
    /// always-overflowing accumulator) is not `int_only`, so F2 never pays its
    /// overflow bail every trip.
    #[test]
    fn an_overflowing_site_is_not_int_only() {
        let site = |result: u16| {
            (
                0u32,
                SiteFeedback {
                    lhs: CLASS_INT,
                    rhs: CLASS_INT,
                    result,
                    samples: 8,
                },
            )
        };
        assert!(int_only(&[site(CLASS_INT)], 0, true));
        assert!(!int_only(&[site(CLASS_INT | CLASS_FLOAT)], 0, true));
        assert!(!int_only(&[site(CLASS_FLOAT)], 0, true));
    }

    /// Epic S2 (1.2): a read in a *later block* keeps the store alive through
    /// the IR's liveness (not just the block-local scan).
    #[test]
    fn a_store_read_in_a_successor_block_stays_live() {
        let bytes = seq(&[
            &push_i32(1),
            &loc(op::OP_put_loc, 0),
            &[op::OP_push_false],
            &branch(op::OP_if_false, 5), // -> L
            &[op::OP_return_undef],
            &loc(op::OP_get_loc, 0), // L
            &[op::OP_return],
        ]);
        let verified = verified_local(bytes, 0, 1);
        let ir = Ir::build(&verified, true);
        let hints = dead_local_stores(&ir, verified.instructions(), true);
        assert!(!hints.contains(&5), "the successor reads local 0");
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

    /// `fclosure` (epic F §6.12) reads a nested bytecode out of the constant
    /// pool, so it needs the same non-empty pool `push_const` does.
    #[test]
    fn fclosure_lowers_against_the_constant_pool() {
        let mut snap = snapshot(seq(&[&[op::OP_fclosure], &u32b(0), &[op::OP_return]]), 0, 0);
        snap.const_meta = Box::new([ConstMeta { tag: 0 }]);
        let isa = crate::jit::build_host_isa().unwrap();
        let verified = verify(snap, VerifyLimits::default()).unwrap();
        assert!(compile(&verified, isa.as_ref(), &Cell::new(0)).is_ok());
    }

    /// Why [`entry_is_translatable`] cannot fire today: the only opcodes the
    /// baseline could refuse are policy `Reject`, and every surviving opcode is
    /// lowered (D-6); a `Reject` opcode at pc 0 fails verification before the
    /// entry check is reached. The check stays as the fail-closed net for a
    /// future operand-free unsupported opcode.
    #[test]
    fn the_call_opcodes_cannot_be_a_function_entry() {
        use crate::bytecode::opcode_info_final;
        use crate::jit::opcode_names;
        // `Reject` opcodes that pop no operand are the only ones that could start
        // a verified function; none of them lower, so `entry_is_translatable`
        // stays unreachable. `apply`/`call`/`call_constructor`/`call_method` pop
        // at least one operand each (D-6 note moved to
        // `every_opcode_the_verifier_accepts_is_lowered`).
        const PINNED: &[&str] = &["apply", "call", "call_constructor", "call_method"];
        for name in PINNED {
            let id = opcode_names()
                .iter()
                .position(|n| n == name)
                .unwrap_or_else(|| panic!("{name} is not in the policy table"))
                as u8;
            assert!(
                opcode_info_final(id).n_pop >= 1,
                "{name} pops nothing, so it *can* start a verified function — and then \
                 `entry_is_translatable` would refuse the whole function"
            );
        }
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

    /// The guard fact: only values the compiler produced as `Int` (a `push_i32`
    /// literal, an `Int` constant) are known, and the fact dies with the entry —
    /// a flush (helper call, exit, branch) drops both.
    #[test]
    fn stack_cache_tracks_known_int_operands() {
        let ir = ir_of(|b| {
            let sp = b.ins().iconst(types::I64, 4096);
            let mut cache = StackCache::default();
            cache.anchor(0);
            let literal = b.ins().iconst(types::I64, 7);
            let sp = cache.push_int(b, sp, literal);
            assert!(cache.known_int(1), "a literal is a known Int");
            let loaded = b.ins().load(types::I64, MemFlags::new(), sp, 0);
            let sp = cache.push(b, sp, loaded);
            assert!(!cache.known_int(2), "a loaded value is not");
            // Storing materializes the value without changing the fact.
            cache.flush(b, sp);
            assert!(!cache.known_int(1), "the flush dropped the entry");
            assert!(cache.is_empty());
        });
        assert_eq!(count(&ir, "load"), 1, "only the loaded operand:\n{ir}");
    }

    /// Epic F4: a slot read twice in one block is loaded once — the second read
    /// reuses the register the first one filled (`LocalCache`).
    #[test]
    fn local_cache_reads_a_slot_once_per_block() {
        let ir = ir_of(|b| {
            let base = b.ins().iconst(types::I64, 4096);
            let mut cache = LocalCache::new(true, true);
            let first = cache.read(b, base, false, 2, 16);
            let second = cache.read(b, base, false, 2, 16);
            assert_eq!(first, second, "the same register value comes back");
            assert!(!cache.is_empty());
        });
        assert_eq!(count(&ir, "load"), 1, "one load for two reads:\n{ir}");
    }

    /// The counter-case: as soon as the slot can change, the copy is dropped and
    /// the next read loads again.
    #[test]
    fn local_cache_drops_the_copy_when_the_slot_changes() {
        let ir = ir_of(|b| {
            let base = b.ins().iconst(types::I64, 4096);
            let mut cache = LocalCache::new(true, true);
            let _ = cache.read(b, base, false, 1, 8);
            cache.drop_slot(false, 1);
            let _ = cache.read(b, base, false, 1, 8);
            // The argument space is keyed separately from the local space, so
            // argument 1 keeps its own copy.
            let _ = cache.read(b, base, true, 1, 8);
            assert_eq!(cache.entries.len(), 2, "one copy per (space, index)");
        });
        assert_eq!(count(&ir, "load"), 3, "an invalidated slot reloads:\n{ir}");
    }

    /// Aliased slots (`fclosure`/refs) and mapped `arguments` formals are never
    /// cached: every read is a load.
    #[test]
    fn local_cache_is_off_for_aliased_slots() {
        let ir = ir_of(|b| {
            let base = b.ins().iconst(types::I64, 4096);
            let mut cache = LocalCache::new(false, false);
            let _ = cache.read(b, base, false, 0, 0);
            let _ = cache.read(b, base, false, 0, 0);
            let _ = cache.read(b, base, true, 0, 0);
            cache.fill(false, 0, base);
            assert!(cache.is_empty(), "nothing may be cached");
        });
        assert_eq!(count(&ir, "load"), 3, "every read loads:\n{ir}");
    }

    /// The analysis that decides the two flags above.
    #[test]
    fn cacheable_slots_tracks_aliasing() {
        let verify_bytes = |bytes: Vec<u8>, vars: u16, consts: usize| {
            let mut snap = snapshot(bytes, 0, vars);
            snap.const_meta = vec![ConstMeta { tag: 0 }; consts].into_boxed_slice();
            verify(snap, VerifyLimits::default()).unwrap_or_else(|e| panic!("verify: {e:?}"))
        };

        // Plain locals and arguments: both cacheable.
        let plain = seq(&[
            &loc(op::OP_get_loc, 0),
            &loc(op::OP_put_loc, 0),
            &[op::OP_return_undef],
        ]);
        assert_eq!(cacheable_slots(&verify_bytes(plain, 1, 0)), (true, true));

        // A nested function may capture either space.
        let closure = seq(&[&[op::OP_fclosure], &u32b(0), &[op::OP_return]]);
        assert_eq!(
            cacheable_slots(&verify_bytes(closure, 0, 1)),
            (false, false)
        );

        // A reference object pointing at a local.
        let loc_ref = seq(&[&[op::OP_make_loc_ref, 0, 0, 0, 0, 0, 0], &[op::OP_return]]);
        assert_eq!(
            cacheable_slots(&verify_bytes(loc_ref, 1, 0)),
            (false, false)
        );

        // Mapped `arguments` aliases the formals only.
        let mapped = seq(&[&[op::OP_special_object, 1], &[op::OP_return]]);
        assert_eq!(cacheable_slots(&verify_bytes(mapped, 0, 0)), (true, false));

        // The strict (copying) `arguments` object does not.
        let strict = seq(&[&[op::OP_special_object, 0], &[op::OP_return]]);
        assert_eq!(cacheable_slots(&verify_bytes(strict, 0, 0)), (true, true));
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

    /// A join whose arms disagree only in an operand *proof* — a `Tagged`
    /// operand against an `Int32` literal, the shape every short-circuit and
    /// `??` produces (`x || 5`, `x ?? 7`, `c ? x : 7`) — widens the operand to
    /// `Tagged` and verifies; the whole function then compiles. Before the
    /// widening it was rejected fail-closed, which is why the F4 carry cases
    /// cross every join with same-kind operands.
    #[test]
    fn join_of_tagged_and_int32_operands_widens_to_tagged() {
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
        let verified = verify(snapshot(bytes.clone(), 0, 1), VerifyLimits::default())
            .expect("a lost kind proof must not reject the function");
        assert_eq!(
            verified.stack_heights(),
            [0, 1, 2, 1, 0, 1],
            "a kind-only widening must not change the proven heights"
        );
        compile_bytes(bytes, 0, 1).expect("the widened join lowers");
    }
}
