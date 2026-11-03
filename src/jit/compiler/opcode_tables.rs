// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Which opcodes the baseline lowers, which helper serves each of them,
//! and the operand-shuffle permutation table.

use super::*;

/// Whether the baseline can translate this opcode. Everything else is a
/// `Resume` exit: the interpreter runs that instruction and continues.
pub(super) fn lowers(name: &str) -> bool {
    helper_id(name).is_some()
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
                // The four call opcodes run the callee through a call helper
                // on the same frame (no per-call exit).
                | "call"
                | "call_method"
                | "call_constructor"
                | "apply"
        )
}

pub(super) fn helper_id(name: &str) -> Option<usize> {
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
        // The engine-generated opcodes. `throw_error` and
        // `push_bigint_i32` carry an immediate, so they have explicit arms in
        // `lower_insn` and are only listed here for `lowers()`.
        "throw" => HELPER_THROW,
        "throw_error" => HELPER_THROW_ERROR,
        "regexp" => HELPER_REGEXP,
        "push_bigint_i32" => HELPER_PUSH_BIGINT_I32,
        "nip_catch" => HELPER_NIP_CATCH,
        // Sync iteration. `for_of_next`/`iterator_close`/`iterator_call` need
        // more than a name→id map (an operand or a stack adjustment), so they
        // have explicit arms in `lower_insn` and are only listed here for
        // `lowers()`.
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
        // explicit arm in `lower_insn`; listed here for `lowers()`.
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
pub(super) enum FastOp {
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

/// Maps an opcode name to its fast-path form. The fallback helper id comes from
/// [`helper_id`], so the name→helper mapping has one source.
pub(super) fn fast_binary(name: &str) -> Option<FastOp> {
    use FastOp::*;
    Some(match name {
        "add" => Add,
        "sub" => Sub,
        "mul" => Mul,
        "lt" => Lt,
        "lte" => Lte,
        "gt" => Gt,
        "gte" => Gte,
        "eq" => Eq,
        "neq" => Neq,
        "strict_eq" => StrictEq,
        "strict_neq" => StrictNeq,
        "and" => And,
        "or" => Or,
        "xor" => Xor,
        "shl" => Shl,
        "shr" => Shr,
        "sar" => Sar,
        _ => return None,
    })
}

/// Whether an opcode is one of the pure binaries the CSE pass numbers. It is
/// exactly the set [`fast_binary`] lowers, so the pass can neither plan
/// a reuse the lowering would not honor nor miss a site it does.
pub(crate) fn is_cse_expression(name: &str) -> bool {
    fast_binary(name).is_some()
}

/// Output arrangement for the stack shuffles: each output copies the popped
/// operand at that index. Order matches the interpreter's writes.
/// Every opcode the baseline lowers as a pure stack shuffle. Exported like
/// `policy::opcode_names` so the coverage gate in `tests/jit_opcodes.rs` can
/// check its oracle set against the real table instead of a copied list.
pub const fn shuffle_opcodes() -> &'static [&'static str] {
    &[
        "nip", "nip1", "dup", "dup1", "dup2", "dup3", "insert2", "insert3", "insert4", "perm3",
        "perm4", "perm5", "swap", "swap2", "rot3l", "rot3r", "rot4l", "rot5l",
    ]
}

/// Pop count and output source indices for a stack shuffle (see
/// [`shuffle_opcodes`]).
pub(super) fn shuffle_outputs(name: &str) -> Option<(usize, &'static [usize])> {
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
