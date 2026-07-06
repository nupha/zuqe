// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::path::Path;

use once_cell::sync::Lazy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum OpcodeKind {
    Common,
    Temp,
    Short,
}

/// GC allocation classification for each opcode.
/// 0 = never allocates GC nodes
/// 1 = may allocate GC nodes (type coercion, operator overloading, etc.)
/// 2 = always allocates GC nodes
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum OpcodeGcAlloc {
    Never = 0,
    Conditional = 1,
    Always = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OpcodeDef {
    id: &'static str,
    size: u8,
    pop_n: u8,
    push_n: u8,
    fmt: &'static str,
    kind: OpcodeKind,
    gc_alloc: OpcodeGcAlloc,
}

macro_rules! DEF {
    ($id:expr, $size:expr, $pop_n:expr, $push_n:expr, $fmt:expr, $short:expr, $gc:tt) => {
        OpcodeDef {
            id: $id,
            size: $size,
            pop_n: $pop_n,
            push_n: $push_n,
            fmt: $fmt,
            kind: if $short {
                OpcodeKind::Short
            } else {
                OpcodeKind::Common
            },
            gc_alloc: OpcodeGcAlloc::$gc,
        }
    };
}

macro_rules! def {
    ($id:expr, $size:expr, $pop_n:expr, $push_n:expr, $fmt:expr, $gc:tt) => {
        OpcodeDef {
            id: $id,
            size: $size,
            pop_n: $pop_n,
            push_n: $push_n,
            fmt: $fmt,
            kind: OpcodeKind::Temp,
            gc_alloc: OpcodeGcAlloc::$gc,
        }
    };
}

static OP_CODES: Lazy<Vec<OpcodeDef>> = Lazy::new(|| {
    let ops = vec![
        DEF!("invalid", 1, 0, 0, "NONE", false, Never),
        DEF!("push_i32", 5, 0, 1, "I32", false, Never),
        DEF!("push_const", 5, 0, 1, "CONST", false, Never),
        DEF!("fclosure", 5, 0, 1, "CONST", false, Always),
        DEF!("push_atom_value", 5, 0, 1, "ATOM", false, Conditional),
        DEF!("private_symbol", 5, 0, 1, "ATOM", false, Always),
        DEF!("undefined", 1, 0, 1, "NONE", false, Never),
        DEF!("null", 1, 0, 1, "NONE", false, Never),
        DEF!("push_this", 1, 0, 1, "NONE", false, Conditional),
        DEF!("push_false", 1, 0, 1, "NONE", false, Never),
        DEF!("push_true", 1, 0, 1, "NONE", false, Never),
        DEF!("object", 1, 0, 1, "NONE", false, Always),
        DEF!("special_object", 2, 0, 1, "U8", false, Always),
        DEF!("rest", 3, 0, 1, "U16", false, Always),
        DEF!("drop", 1, 1, 0, "NONE", false, Never),
        DEF!("nip", 1, 2, 1, "NONE", false, Never),
        DEF!("nip1", 1, 3, 2, "NONE", false, Never),
        DEF!("dup", 1, 1, 2, "NONE", false, Never),
        DEF!("dup1", 1, 2, 3, "NONE", false, Never),
        DEF!("dup2", 1, 2, 4, "NONE", false, Never),
        DEF!("dup3", 1, 3, 6, "NONE", false, Never),
        DEF!("insert2", 1, 2, 3, "NONE", false, Never),
        DEF!("insert3", 1, 3, 4, "NONE", false, Never),
        DEF!("insert4", 1, 4, 5, "NONE", false, Never),
        DEF!("perm3", 1, 3, 3, "NONE", false, Never),
        DEF!("perm4", 1, 4, 4, "NONE", false, Never),
        DEF!("perm5", 1, 5, 5, "NONE", false, Never),
        DEF!("swap", 1, 2, 2, "NONE", false, Never),
        DEF!("swap2", 1, 4, 4, "NONE", false, Never),
        DEF!("rot3l", 1, 3, 3, "NONE", false, Never),
        DEF!("rot3r", 1, 3, 3, "NONE", false, Never),
        DEF!("rot4l", 1, 4, 4, "NONE", false, Never),
        DEF!("rot5l", 1, 5, 5, "NONE", false, Never),
        DEF!("call_constructor", 3, 2, 1, "NPOP", false, Never),
        DEF!("call", 3, 1, 1, "NPOP", false, Never),
        DEF!("tail_call", 3, 1, 0, "NPOP", false, Never),
        DEF!("call_method", 3, 2, 1, "NPOP", false, Never),
        DEF!("tail_call_method", 3, 2, 0, "NPOP", false, Never),
        DEF!("array_from", 3, 0, 1, "NPOP", false, Always),
        DEF!("apply", 3, 3, 1, "U16", false, Never),
        DEF!("return", 1, 1, 0, "NONE", false, Never),
        DEF!("return_undef", 1, 0, 0, "NONE", false, Never),
        DEF!("check_ctor_return", 1, 1, 2, "NONE", false, Never),
        DEF!("check_ctor", 1, 0, 0, "NONE", false, Never),
        DEF!("check_brand", 1, 2, 2, "NONE", false, Never),
        DEF!("add_brand", 1, 2, 0, "NONE", false, Never),
        DEF!("return_async", 1, 1, 0, "NONE", false, Never),
        DEF!("throw", 1, 1, 0, "NONE", false, Never),
        DEF!("throw_error", 6, 0, 0, "ATOM_U8", false, Never),
        DEF!("eval", 5, 1, 1, "NPOP_U16", false, Always),
        DEF!("apply_eval", 3, 2, 1, "U16", false, Always),
        DEF!("regexp", 1, 2, 1, "NONE", false, Always),
        DEF!("get_super", 1, 1, 1, "NONE", false, Never),
        DEF!("import", 1, 2, 1, "NONE", false, Always),
        DEF!("check_var", 5, 0, 1, "ATOM", false, Never),
        DEF!("get_var_undef", 5, 0, 1, "ATOM", false, Never),
        DEF!("get_var", 5, 0, 1, "ATOM", false, Never),
        DEF!("put_var", 5, 1, 0, "ATOM", false, Never),
        DEF!("put_var_init", 5, 1, 0, "ATOM", false, Never),
        DEF!("put_var_strict", 5, 2, 0, "ATOM", false, Never),
        DEF!("get_ref_value", 1, 2, 3, "NONE", false, Never),
        DEF!("put_ref_value", 1, 3, 0, "NONE", false, Never),
        DEF!("define_var", 6, 0, 0, "ATOM_U8", false, Never),
        DEF!("check_define_var", 6, 0, 0, "ATOM_U8", false, Never),
        DEF!("define_func", 6, 1, 0, "ATOM_U8", false, Never),
        DEF!("get_field", 5, 1, 1, "ATOM", false, Never),
        DEF!("get_field2", 5, 1, 2, "ATOM", false, Never),
        DEF!("get_field_ic", 7, 1, 1, "ATOM_U16", false, Never),
        DEF!("get_field2_ic", 7, 1, 2, "ATOM_U16", false, Never),
        DEF!("put_field", 5, 2, 0, "ATOM", false, Never),
        DEF!("put_field_ic", 7, 2, 0, "ATOM_U16", false, Never),
        DEF!("get_private_field", 1, 2, 1, "NONE", false, Never),
        DEF!("put_private_field", 1, 3, 0, "NONE", false, Never),
        DEF!("define_private_field", 1, 3, 1, "NONE", false, Never),
        DEF!("get_array_el", 1, 2, 1, "NONE", false, Never),
        DEF!("get_array_el2", 1, 2, 2, "NONE", false, Never),
        DEF!("get_array_el3", 1, 2, 3, "NONE", false, Never),
        DEF!("put_array_el", 1, 3, 0, "NONE", false, Never),
        DEF!("get_super_value", 1, 3, 1, "NONE", false, Never),
        DEF!("put_super_value", 1, 4, 0, "NONE", false, Never),
        DEF!("define_field", 5, 2, 1, "ATOM", false, Never),
        DEF!("set_name", 5, 1, 1, "ATOM", false, Never),
        DEF!("set_name_computed", 1, 2, 2, "NONE", false, Never),
        DEF!("set_proto", 1, 2, 1, "NONE", false, Never),
        DEF!("set_home_object", 1, 2, 2, "NONE", false, Never),
        DEF!("define_array_el", 1, 3, 2, "NONE", false, Never),
        DEF!("append", 1, 3, 2, "NONE", false, Never),
        DEF!("copy_data_properties", 2, 3, 3, "U8", false, Never),
        DEF!("define_method", 6, 2, 1, "ATOM_U8", false, Never),
        DEF!("define_method_computed", 2, 3, 1, "U8", false, Never),
        DEF!("define_class", 6, 2, 2, "ATOM_U8", false, Always),
        DEF!("define_class_computed", 6, 3, 3, "ATOM_U8", false, Always),
        DEF!("get_loc", 3, 0, 1, "LOC", false, Never),
        DEF!("put_loc", 3, 1, 0, "LOC", false, Never),
        DEF!("set_loc", 3, 1, 1, "LOC", false, Never),
        DEF!("get_arg", 3, 0, 1, "ARG", false, Never),
        DEF!("put_arg", 3, 1, 0, "ARG", false, Never),
        DEF!("set_arg", 3, 1, 1, "ARG", false, Never),
        DEF!("get_var_ref", 3, 0, 1, "VAR_REF", false, Never),
        DEF!("put_var_ref", 3, 1, 0, "VAR_REF", false, Never),
        DEF!("set_var_ref", 3, 1, 1, "VAR_REF", false, Never),
        DEF!("set_loc_uninitialized", 3, 0, 0, "LOC", false, Never),
        DEF!("get_loc_check", 3, 0, 1, "LOC", false, Never),
        DEF!("put_loc_check", 3, 1, 0, "LOC", false, Never),
        DEF!("put_loc_check_init", 3, 1, 0, "LOC", false, Never),
        DEF!("get_loc_checkthis", 3, 0, 1, "LOC", false, Never),
        DEF!("get_var_ref_check", 3, 0, 1, "VAR_REF", false, Never),
        DEF!("put_var_ref_check", 3, 1, 0, "VAR_REF", false, Never),
        DEF!("put_var_ref_check_init", 3, 1, 0, "VAR_REF", false, Never),
        DEF!("close_loc", 3, 0, 0, "LOC", false, Never),
        DEF!("if_false", 5, 1, 0, "LABEL", false, Never),
        DEF!("if_true", 5, 1, 0, "LABEL", false, Never),
        DEF!("goto", 5, 0, 0, "LABEL", false, Never),
        DEF!("catch", 5, 0, 1, "LABEL", false, Never),
        DEF!("gosub", 5, 0, 0, "LABEL", false, Never),
        DEF!("ret", 1, 1, 0, "NONE", false, Never),
        DEF!("nip_catch", 1, 2, 1, "NONE", false, Never),
        DEF!("to_object", 1, 1, 1, "NONE", false, Conditional),
        DEF!("to_propkey", 1, 1, 1, "NONE", false, Conditional),
        DEF!("with_get_var", 10, 1, 0, "ATOM_LABEL_U8", false, Never), // must be in the same order as scope_xxx
        DEF!("with_put_var", 10, 2, 1, "ATOM_LABEL_U8", false, Never), // must be in the same order as scope_xxx
        DEF!("with_delete_var", 10, 1, 0, "ATOM_LABEL_U8", false, Never), // must be in the same order as scope_xxx
        DEF!("with_make_ref", 10, 1, 0, "ATOM_LABEL_U8", false, Never), // must be in the same order as scope_xxx
        DEF!("with_get_ref", 10, 1, 0, "ATOM_LABEL_U8", false, Never), // must be in the same order as scope_xxx
        DEF!(
            "with_get_ref_undef",
            10,
            1,
            0,
            "ATOM_LABEL_U8",
            false,
            Never
        ), // must be in the same order as scope_xxx
        DEF!("make_loc_ref", 7, 0, 2, "ATOM_U16", false, Always),
        DEF!("make_arg_ref", 7, 0, 2, "ATOM_U16", false, Always),
        DEF!("make_var_ref_ref", 7, 0, 2, "ATOM_U16", false, Always),
        DEF!("make_var_ref", 5, 0, 2, "ATOM", false, Always),
        DEF!("for_in_start", 1, 1, 1, "NONE", false, Always),
        DEF!("for_of_start", 1, 1, 3, "NONE", false, Always),
        DEF!("for_await_of_start", 1, 1, 3, "NONE", false, Always),
        DEF!("for_in_next", 1, 1, 3, "NONE", false, Never),
        DEF!("for_of_next", 2, 3, 5, "U8", false, Never),
        DEF!("iterator_check_object", 1, 1, 1, "NONE", false, Never),
        DEF!("iterator_get_value_done", 1, 1, 2, "NONE", false, Never),
        DEF!("iterator_close", 1, 3, 0, "NONE", false, Never),
        DEF!("iterator_next", 1, 4, 4, "NONE", false, Never),
        DEF!("iterator_call", 2, 4, 5, "U8", false, Never),
        DEF!("initial_yield", 1, 0, 0, "NONE", false, Never),
        DEF!("yield", 1, 1, 2, "NONE", false, Never),
        DEF!("yield_star", 1, 1, 2, "NONE", false, Never),
        DEF!("async_yield_star", 1, 1, 2, "NONE", false, Never),
        DEF!("await", 1, 1, 1, "NONE", false, Never),
        DEF!("neg", 1, 1, 1, "NONE", false, Conditional),
        DEF!("plus", 1, 1, 1, "NONE", false, Conditional),
        DEF!("dec", 1, 1, 1, "NONE", false, Conditional),
        DEF!("inc", 1, 1, 1, "NONE", false, Conditional),
        DEF!("post_dec", 1, 1, 2, "NONE", false, Conditional),
        DEF!("post_inc", 1, 1, 2, "NONE", false, Conditional),
        DEF!("dec_loc", 2, 0, 0, "LOC8", false, Conditional),
        DEF!("inc_loc", 2, 0, 0, "LOC8", false, Conditional),
        DEF!("add_loc", 2, 1, 0, "LOC8", false, Conditional),
        DEF!("not", 1, 1, 1, "NONE", false, Conditional),
        DEF!("lnot", 1, 1, 1, "NONE", false, Conditional),
        DEF!("typeof", 1, 1, 1, "NONE", false, Conditional),
        DEF!("delete", 1, 2, 1, "NONE", false, Conditional),
        DEF!("delete_var", 5, 0, 1, "ATOM", false, Conditional),
        DEF!("mul", 1, 2, 1, "NONE", false, Conditional),
        DEF!("div", 1, 2, 1, "NONE", false, Conditional),
        DEF!("mod", 1, 2, 1, "NONE", false, Conditional),
        DEF!("add", 1, 2, 1, "NONE", false, Conditional),
        DEF!("sub", 1, 2, 1, "NONE", false, Conditional),
        DEF!("pow", 1, 2, 1, "NONE", false, Conditional),
        DEF!("shl", 1, 2, 1, "NONE", false, Never),
        DEF!("sar", 1, 2, 1, "NONE", false, Never),
        DEF!("shr", 1, 2, 1, "NONE", false, Never),
        DEF!("lt", 1, 2, 1, "NONE", false, Conditional),
        DEF!("lte", 1, 2, 1, "NONE", false, Conditional),
        DEF!("gt", 1, 2, 1, "NONE", false, Conditional),
        DEF!("gte", 1, 2, 1, "NONE", false, Conditional),
        DEF!("instanceof", 1, 2, 1, "NONE", false, Conditional),
        DEF!("in", 1, 2, 1, "NONE", false, Never),
        DEF!("eq", 1, 2, 1, "NONE", false, Conditional),
        DEF!("neq", 1, 2, 1, "NONE", false, Conditional),
        DEF!("strict_eq", 1, 2, 1, "NONE", false, Never),
        DEF!("strict_neq", 1, 2, 1, "NONE", false, Never),
        DEF!("and", 1, 2, 1, "NONE", false, Never),
        DEF!("xor", 1, 2, 1, "NONE", false, Never),
        DEF!("or", 1, 2, 1, "NONE", false, Never),
        DEF!("is_undefined_or_null", 1, 1, 1, "NONE", false, Never),
        DEF!("private_in", 1, 2, 1, "NONE", false, Never),
        DEF!("nop", 1, 0, 0, "NONE", false, Never),
        //
        // intermediate
        //
        def!("enter_scope", 3, 0, 0, "U16", Never),
        def!("leave_scope", 3, 0, 0, "U16", Never),
        def!("label", 5, 0, 0, "LABEL", Never),
        def!("scope_get_var_undef", 7, 0, 1, "ATOM_U16", Never),
        def!("scope_get_var", 7, 0, 1, "ATOM_U16", Never),
        def!("scope_put_var", 7, 1, 0, "ATOM_U16", Never),
        def!("scope_delete_var", 7, 0, 1, "ATOM_U16", Never),
        def!("scope_make_ref", 11, 0, 2, "ATOM_LABEL_U16", Never),
        def!("scope_get_ref", 7, 0, 2, "ATOM_U16", Never),
        def!("scope_put_var_init", 7, 0, 2, "ATOM_U16", Never),
        def!("scope_get_var_checkthis", 7, 0, 1, "ATOM_U16", Never),
        def!("scope_get_private_field", 7, 1, 1, "ATOM_U16", Never),
        def!("scope_get_private_field2", 7, 1, 2, "ATOM_U16", Never),
        def!("scope_put_private_field", 7, 2, 0, "ATOM_U16", Never),
        def!("scope_in_private_field", 7, 1, 1, "ATOM_U16", Never),
        def!("get_field_opt_chain", 5, 1, 1, "ATOM", Never),
        def!("get_array_el_opt_chain", 1, 2, 1, "NONE", Never),
        def!("set_class_name", 5, 1, 1, "U32", Never),
        def!("line_num", 9, 0, 0, "U32_U32", Never),
    ];

    // // short opcodes
    // ops.extend_from_slice(&[
    //     DEF!("push_minus1", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_0", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_1", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_2", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_3", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_4", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_5", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_6", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_7", 1, 0, 1, "NONE_INT", true),
    //     DEF!("push_i8", 2, 0, 1, "I8", true),
    //     DEF!("push_i16", 3, 0, 1, "I16", true),
    //     DEF!("push_const8", 2, 0, 1, "CONST8", true),
    //     DEF!("fclosure8", 2, 0, 1, "CONST8", true),
    //     DEF!("push_empty_string", 1, 0, 1, "NONE", true),
    //     DEF!("get_loc8", 2, 0, 1, "LOC8", true),
    //     DEF!("put_loc8", 2, 1, 0, "LOC8", true),
    //     DEF!("set_loc8", 2, 1, 1, "LOC8", true),
    //     DEF!("get_loc0", 1, 0, 1, "NONE_LOC", true),
    //     DEF!("get_loc1", 1, 0, 1, "NONE_LOC", true),
    //     DEF!("get_loc2", 1, 0, 1, "NONE_LOC", true),
    //     DEF!("get_loc3", 1, 0, 1, "NONE_LOC", true),
    //     DEF!("put_loc0", 1, 1, 0, "NONE_LOC", true),
    //     DEF!("put_loc1", 1, 1, 0, "NONE_LOC", true),
    //     DEF!("put_loc2", 1, 1, 0, "NONE_LOC", true),
    //     DEF!("put_loc3", 1, 1, 0, "NONE_LOC", true),
    //     DEF!("set_loc0", 1, 1, 1, "NONE_LOC", true),
    //     DEF!("set_loc1", 1, 1, 1, "NONE_LOC", true),
    //     DEF!("set_loc2", 1, 1, 1, "NONE_LOC", true),
    //     DEF!("set_loc3", 1, 1, 1, "NONE_LOC", true),
    //     DEF!("set_arg", 3, 1, 1, "ARG", false),
    //     DEF!("get_arg0", 1, 0, 1, "NONE_ARG", true),
    //     DEF!("get_arg1", 1, 0, 1, "NONE_ARG", true),
    //     DEF!("get_arg2", 1, 0, 1, "NONE_ARG", true),
    //     DEF!("get_arg3", 1, 0, 1, "NONE_ARG", true),
    //     DEF!("put_arg0", 1, 1, 0, "NONE_ARG", true),
    //     DEF!("put_arg1", 1, 1, 0, "NONE_ARG", true),
    //     DEF!("put_arg2", 1, 1, 0, "NONE_ARG", true),
    //     DEF!("put_arg3", 1, 1, 0, "NONE_ARG", true),
    //     DEF!("set_arg0", 1, 1, 1, "NONE_ARG", true),
    //     DEF!("set_arg1", 1, 1, 1, "NONE_ARG", true),
    //     DEF!("set_arg2", 1, 1, 1, "NONE_ARG", true),
    //     DEF!("set_arg3", 1, 1, 1, "NONE_ARG", true),
    //     DEF!("get_var_ref0", 1, 0, 1, "NONE_VAR_REF", true),
    //     DEF!("get_var_ref1", 1, 0, 1, "NONE_VAR_REF", true),
    //     DEF!("get_var_ref2", 1, 0, 1, "NONE_VAR_REF", true),
    //     DEF!("get_var_ref3", 1, 0, 1, "NONE_VAR_REF", true),
    //     DEF!("put_var_ref0", 1, 1, 0, "NONE_VAR_REF", true),
    //     DEF!("put_var_ref1", 1, 1, 0, "NONE_VAR_REF", true),
    //     DEF!("put_var_ref2", 1, 1, 0, "NONE_VAR_REF", true),
    //     DEF!("put_var_ref3", 1, 1, 0, "NONE_VAR_REF", true),
    //     DEF!("set_var_ref", 3, 1, 1, "VAR_REF", false),
    //     DEF!("set_var_ref0", 1, 1, 1, "NONE_VAR_REF", true),
    //     DEF!("set_var_ref1", 1, 1, 1, "NONE_VAR_REF", true),
    //     DEF!("set_var_ref2", 1, 1, 1, "NONE_VAR_REF", true),
    //     DEF!("set_var_ref3", 1, 1, 1, "NONE_VAR_REF", true),
    //     DEF!("get_length", 1, 1, 1, "NONE", true),
    //     DEF!("if_false8", 2, 1, 0, "LABEL8", true),
    //     DEF!("if_true8", 2, 1, 0, "LABEL8", true),
    //     DEF!("goto8", 2, 0, 0, "LABEL8", true),
    //     DEF!("goto16", 3, 0, 0, "LABEL16", true),
    //     DEF!("call0", 1, 1, 1, "NPOPX", true),
    //     DEF!("call1", 1, 1, 1, "NPOPX", true),
    //     DEF!("call2", 1, 1, 1, "NPOPX", true),
    //     DEF!("call3", 1, 1, 1, "NPOPX", true),
    //     DEF!("is_undefined", 1, 1, 1, "NONE", true),
    //     DEF!("is_null", 1, 1, 1, "NONE", true),
    //     DEF!("typeof_is_undefined", 1, 1, 1, "NONE", true),
    //     DEF!("typeof_is_function", 1, 1, 1, "NONE", true),
    // ]);

    ops
});

pub fn gen_opcodes(outfile: &Path) {
    let opcode_defs = &OP_CODES;

    let mut enums = Vec::<String>::with_capacity(512);
    let mut enums_temp = Vec::<String>::with_capacity(512);
    let mut consts = Vec::<String>::with_capacity(512);
    let mut info_common = Vec::<String>::with_capacity(512);
    let mut info_temp = Vec::<String>::with_capacity(512);
    let mut info_short = Vec::<String>::with_capacity(512);

    let fmts = &[
        "NONE",
        "NONE_INT",
        "NONE_LOC",
        "NONE_ARG",
        "NONE_VAR_REF",
        "U8",
        "I8",
        "LOC8",
        "CONST8",
        "LABEL8",
        "U16",
        "I16",
        "LABEL16",
        "NPOP",
        "NPOPX",
        "NPOP_U16",
        "LOC",
        "ARG",
        "VAR_REF",
        "U32",
        "U32_U32",
        "I32",
        "CONST",
        "LABEL",
        "ATOM",
        "ATOM_U8",
        "ATOM_U16",
        "ATOM_LABEL_U8",
        "ATOM_LABEL_U16",
        "LABEL_U16",
    ];
    enums.push(
        "#[allow(dead_code)]\n#[derive(Debug,Clone,Copy,PartialEq)]\n#[repr(u8)]\n#[allow(clippy::upper_case_acronyms)]"
            .into(),
    );
    enums.push("pub enum OpcodeFormat {".into());
    enums.push(
        fmts.iter()
            .map(|x| format!("{x},"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    enums.push("}".into());

    enums.push("#[allow(dead_code)]\n#[allow(clippy::upper_case_acronyms)]\n#[derive(Debug,Clone,Copy,PartialEq,Eq)]\n#[repr(u8)]".into());
    enums.push("pub enum OpCode {".into());

    enums_temp.push("#[allow(clippy::upper_case_acronyms)]\n#[derive(Debug,Clone,Copy,PartialEq,Eq)]\n#[repr(u8)]".into());
    enums_temp.push("pub enum OpCodeTemp {".into());

    info_common.push(format!(
        r#"
/// common opcodes
pub(super) const JS_COMMON_OPCODES_INFO: [Opcode; {}] = [
    "#,
        opcode_defs
            .iter()
            .filter(|x| x.kind == OpcodeKind::Common)
            .count()
    ));

    info_temp.push(format!(
        r#"
/// temp opcodes
pub(super) const JS_TEMP_OPCODES_INFO: [Opcode; {}] = [
    "#,
        opcode_defs
            .iter()
            .filter(|x| x.kind == OpcodeKind::Temp)
            .count()
    ));

    info_short.push(format!(
        r#"
/// short opcodes
#[cfg(feature="short_opcode")]
pub(super) const JS_SHORT_OPCODES_INFO: [Opcode; {}] = [
    "#,
        opcode_defs
            .iter()
            .filter(|x| x.kind == OpcodeKind::Short)
            .count()
    ));

    for (i, op) in opcode_defs.iter().enumerate() {
        let descr = format!(
            "    Opcode {{ #[cfg(feature=\"dump_bytecode\")] name: \"{}\", size: {}, n_pop: {}, n_push: {}, fmt: OpcodeFormat::{}, gc_alloc: OpcodeGcAlloc::{gc_alloc:?} }},",
            op.id,
            op.size,
            op.pop_n,
            op.push_n,
            op.fmt,
            gc_alloc = op.gc_alloc
        );

        match op.kind {
            OpcodeKind::Common => {
                info_common.push(descr.clone());
                if i == 0 {
                    enums.push(format!("    {} = 0,", op.id.to_ascii_uppercase()));
                } else {
                    enums.push(format!("    {},", op.id.to_ascii_uppercase()));
                }
                consts.push(format!(
                    "pub const OP_{}: u8 = OpCode::{} as u8;",
                    op.id,
                    op.id.to_ascii_uppercase()
                ));
            }
            OpcodeKind::Short => {
                info_short.push(descr.clone());
                enums.push(format!(
                    "    #[cfg(feature=\"short_opcode\")] {},",
                    op.id.to_ascii_uppercase()
                ));
                consts.push(format!(
                    "#[cfg(feature=\"short_opcode\")]\npub const OP_{}: u8 = OpCode::{} as u8;",
                    op.id,
                    op.id.to_ascii_uppercase()
                ));
            }
            OpcodeKind::Temp => {
                info_temp.push(descr.clone());
                enums_temp.push(format!("    {} = {i},", op.id.to_ascii_uppercase()));
                consts.push(format!(
                    "pub const op_{}: u8 = OpCodeTemp::{} as u8;",
                    op.id,
                    op.id.to_ascii_uppercase()
                ));
            }
        }
    }

    enums.push("}".into());
    enums_temp.push("}".into());
    info_common.push("];".into());
    info_temp.push("];".into());
    info_short.push("];".into());

    std::fs::write(
        outfile,
        [
            r#"
/// GC allocation classification.
/// 0 = never allocates GC nodes
/// 1 = may allocate GC nodes
/// 2 = always allocates GC nodes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum OpcodeGcAlloc {
    Never   = 0,
    Conditional = 1,
    Always  = 2,
}

#[derive(Debug,Clone)]
pub struct Opcode {
    #[cfg(feature="dump_bytecode")]
    pub name: &'static str,

    /// opcode + params
    pub size: u8,
    // the opcodes remove n_pop items from the top of the stack, then pushes n_push items
    pub n_pop: u8,
    pub n_push: u8,

    pub fmt: OpcodeFormat,
    pub gc_alloc: OpcodeGcAlloc,
}"#
            .into(),
            enums.join("\n"),
            enums_temp.join("\n"),
            //
            // opcodes as const u8
            //
            "#[allow(dead_code)]\npub mod opcodes {\nuse super::*;".to_string(),
            consts.join("\n"),
            "}".to_string(),
            info_common.join("\n"),
            info_temp.join("\n"),
            info_short.join("\n"),
            //
            // counts
            //
            format!(
                "pub const OP_COUNT_COMMON:u8={};",
                opcode_defs
                    .iter()
                    .filter(|x| x.kind == OpcodeKind::Common)
                    .count()
            ),
            format!(
                "/// number of final opcodes (common+short)\n#[cfg(feature=\"short_opcode\")]\npub const OP_COUNT_FINAL:u8={}; ",
                opcode_defs
                    .iter()
                    .filter(|x|  x.kind==OpcodeKind::Common || x.kind==OpcodeKind::Short)
                    .count()
            ),
            format!(
                "/// number of final opcodes (common)\n#[cfg(not(feature=\"short_opcode\"))]\npub const OP_COUNT_FINAL:u8={}; ",
                opcode_defs
                    .iter()
                    .filter(|x| x.kind==OpcodeKind::Common)
                    .count()
            ),
            format!(
                "/// number of intermediate opcodes (common+temp)\npub const OP_COUNT_INTERMEDIATE:u8={}; ",
                opcode_defs
                    .iter()
                    .filter(|x| matches!(x.kind,  OpcodeKind::Common| OpcodeKind::Temp))
                    .count()
            ),

            "// end of opcode defs".into(),
        ]
        .join("\n"),
    )
    .unwrap();

    let output = std::process::Command::new("rustfmt")
        .arg("--")
        .arg(outfile)
        .output()
        .expect("Failed to execute rustfmt");
    if !output.status.success() {
        eprintln!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }
}
