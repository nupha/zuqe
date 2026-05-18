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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct OpcodeDef {
    id: &'static str,
    size: u8,
    pop_n: u8,
    push_n: u8,
    fmt: &'static str,
    kind: OpcodeKind,
}

macro_rules! DEF {
    ($id:expr, $size:expr, $pop_n:expr, $push_n:expr, $fmt:expr, $short:expr) => {
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
        }
    };
}

macro_rules! def {
    ($id:expr, $size:expr, $pop_n:expr, $push_n:expr, $fmt:expr) => {
        OpcodeDef {
            id: $id,
            size: $size,
            pop_n: $pop_n,
            push_n: $push_n,
            fmt: $fmt,
            kind: OpcodeKind::Temp,
        }
    };
}

static OP_CODES: Lazy<Vec<OpcodeDef>> = Lazy::new(|| {
    let ops = vec![
        DEF!("invalid", 1, 0, 0, "NONE", false),
        DEF!("push_i32", 5, 0, 1, "I32", false),
        DEF!("push_const", 5, 0, 1, "CONST", false),
        DEF!("fclosure", 5, 0, 1, "CONST", false),
        DEF!("push_atom_value", 5, 0, 1, "ATOM", false),
        DEF!("private_symbol", 5, 0, 1, "ATOM", false),
        DEF!("undefined", 1, 0, 1, "NONE", false),
        DEF!("null", 1, 0, 1, "NONE", false),
        DEF!("push_this", 1, 0, 1, "NONE", false),
        DEF!("push_false", 1, 0, 1, "NONE", false),
        DEF!("push_true", 1, 0, 1, "NONE", false),
        DEF!("object", 1, 0, 1, "NONE", false),
        DEF!("special_object", 2, 0, 1, "U8", false),
        DEF!("rest", 3, 0, 1, "U16", false),
        DEF!("drop", 1, 1, 0, "NONE", false),
        DEF!("nip", 1, 2, 1, "NONE", false),
        DEF!("nip1", 1, 3, 2, "NONE", false),
        DEF!("dup", 1, 1, 2, "NONE", false),
        DEF!("dup1", 1, 2, 3, "NONE", false),
        DEF!("dup2", 1, 2, 4, "NONE", false),
        DEF!("dup3", 1, 3, 6, "NONE", false),
        DEF!("insert2", 1, 2, 3, "NONE", false),
        DEF!("insert3", 1, 3, 4, "NONE", false),
        DEF!("insert4", 1, 4, 5, "NONE", false),
        DEF!("perm3", 1, 3, 3, "NONE", false),
        DEF!("perm4", 1, 4, 4, "NONE", false),
        DEF!("perm5", 1, 5, 5, "NONE", false),
        DEF!("swap", 1, 2, 2, "NONE", false),
        DEF!("swap2", 1, 4, 4, "NONE", false),
        DEF!("rot3l", 1, 3, 3, "NONE", false),
        DEF!("rot3r", 1, 3, 3, "NONE", false),
        DEF!("rot4l", 1, 4, 4, "NONE", false),
        DEF!("rot5l", 1, 5, 5, "NONE", false),
        DEF!("call_constructor", 3, 2, 1, "NPOP", false),
        DEF!("call", 3, 1, 1, "NPOP", false),
        DEF!("tail_call", 3, 1, 0, "NPOP", false),
        DEF!("call_method", 3, 2, 1, "NPOP", false),
        DEF!("tail_call_method", 3, 2, 0, "NPOP", false),
        DEF!("array_from", 3, 0, 1, "NPOP", false),
        DEF!("apply", 3, 3, 1, "U16", false),
        DEF!("return", 1, 1, 0, "NONE", false),
        DEF!("return_undef", 1, 0, 0, "NONE", false),
        DEF!("check_ctor_return", 1, 1, 2, "NONE", false),
        DEF!("check_ctor", 1, 0, 0, "NONE", false),
        DEF!("check_brand", 1, 2, 2, "NONE", false),
        DEF!("add_brand", 1, 2, 0, "NONE", false),
        DEF!("return_async", 1, 1, 0, "NONE", false),
        DEF!("throw", 1, 1, 0, "NONE", false),
        DEF!("throw_error", 6, 0, 0, "ATOM_U8", false),
        DEF!("eval", 5, 1, 1, "NPOP_U16", false),
        DEF!("apply_eval", 3, 2, 1, "U16", false),
        DEF!("regexp", 1, 2, 1, "NONE", false),
        DEF!("get_super", 1, 1, 1, "NONE", false),
        DEF!("import", 1, 1, 1, "NONE", false),
        DEF!("check_var", 5, 0, 1, "ATOM", false),
        DEF!("get_var_undef", 5, 0, 1, "ATOM", false),
        DEF!("get_var", 5, 0, 1, "ATOM", false),
        DEF!("put_var", 5, 1, 0, "ATOM", false),
        DEF!("put_var_init", 5, 1, 0, "ATOM", false),
        DEF!("put_var_strict", 5, 2, 0, "ATOM", false),
        DEF!("get_ref_value", 1, 2, 3, "NONE", false),
        DEF!("put_ref_value", 1, 3, 0, "NONE", false),
        DEF!("define_var", 6, 0, 0, "ATOM_U8", false),
        DEF!("check_define_var", 6, 0, 0, "ATOM_U8", false),
        DEF!("define_func", 6, 1, 0, "ATOM_U8", false),
        DEF!("get_field", 5, 1, 1, "ATOM", false),
        DEF!("get_field2", 5, 1, 2, "ATOM", false),
        DEF!("put_field", 5, 2, 0, "ATOM", false),
        DEF!("get_private_field", 1, 2, 1, "NONE", false),
        DEF!("put_private_field", 1, 3, 0, "NONE", false),
        DEF!("define_private_field", 1, 3, 1, "NONE", false),
        DEF!("get_array_el", 1, 2, 1, "NONE", false),
        DEF!("get_array_el2", 1, 2, 2, "NONE", false),
        DEF!("get_array_el3", 1, 2, 3, "NONE", false),
        DEF!("put_array_el", 1, 3, 0, "NONE", false),
        DEF!("get_super_value", 1, 3, 1, "NONE", false),
        DEF!("put_super_value", 1, 4, 0, "NONE", false),
        DEF!("define_field", 5, 2, 1, "ATOM", false),
        DEF!("set_name", 5, 1, 1, "ATOM", false),
        DEF!("set_name_computed", 1, 2, 2, "NONE", false),
        DEF!("set_proto", 1, 2, 1, "NONE", false),
        DEF!("set_home_object", 1, 2, 2, "NONE", false),
        DEF!("define_array_el", 1, 3, 2, "NONE", false),
        DEF!("append", 1, 3, 2, "NONE", false),
        DEF!("copy_data_properties", 2, 3, 3, "U8", false),
        DEF!("define_method", 6, 2, 1, "ATOM_U8", false),
        DEF!("define_method_computed", 2, 3, 1, "U8", false),
        DEF!("define_class", 6, 2, 2, "ATOM_U8", false),
        DEF!("define_class_computed", 6, 3, 3, "ATOM_U8", false),
        DEF!("get_loc", 3, 0, 1, "LOC", false),
        DEF!("put_loc", 3, 1, 0, "LOC", false),
        DEF!("set_loc", 3, 1, 1, "LOC", false),
        DEF!("get_arg", 3, 0, 1, "ARG", false),
        DEF!("put_arg", 3, 1, 0, "ARG", false),
        DEF!("set_arg", 3, 1, 1, "ARG", false),
        DEF!("get_var_ref", 3, 0, 1, "VAR_REF", false),
        DEF!("put_var_ref", 3, 1, 0, "VAR_REF", false),
        DEF!("set_var_ref", 3, 1, 1, "VAR_REF", false),
        DEF!("set_loc_uninitialized", 3, 0, 0, "LOC", false),
        DEF!("get_loc_check", 3, 0, 1, "LOC", false),
        DEF!("put_loc_check", 3, 1, 0, "LOC", false),
        DEF!("put_loc_check_init", 3, 1, 0, "LOC", false),
        DEF!("get_loc_checkthis", 3, 0, 1, "LOC", false),
        DEF!("get_var_ref_check", 3, 0, 1, "VAR_REF", false),
        DEF!("put_var_ref_check", 3, 1, 0, "VAR_REF", false),
        DEF!("put_var_ref_check_init", 3, 1, 0, "VAR_REF", false),
        DEF!("close_loc", 3, 0, 0, "LOC", false),
        DEF!("if_false", 5, 1, 0, "LABEL", false),
        DEF!("if_true", 5, 1, 0, "LABEL", false),
        DEF!("goto", 5, 0, 0, "LABEL", false),
        DEF!("catch", 5, 0, 1, "LABEL", false),
        DEF!("gosub", 5, 0, 0, "LABEL", false),
        DEF!("ret", 1, 1, 0, "NONE", false),
        DEF!("nip_catch", 1, 2, 1, "NONE", false),
        DEF!("to_object", 1, 1, 1, "NONE", false),
        DEF!("to_propkey", 1, 1, 1, "NONE", false),
        DEF!("with_get_var", 10, 1, 0, "ATOM_LABEL_U8", false), // must be in the same order as scope_xxx
        DEF!("with_put_var", 10, 2, 1, "ATOM_LABEL_U8", false), // must be in the same order as scope_xxx
        DEF!("with_delete_var", 10, 1, 0, "ATOM_LABEL_U8", false), // must be in the same order as scope_xxx
        DEF!("with_make_ref", 10, 1, 0, "ATOM_LABEL_U8", false), // must be in the same order as scope_xxx
        DEF!("with_get_ref", 10, 1, 0, "ATOM_LABEL_U8", false), // must be in the same order as scope_xxx
        DEF!("with_get_ref_undef", 10, 1, 0, "ATOM_LABEL_U8", false), // must be in the same order as scope_xxx
        DEF!("make_loc_ref", 7, 0, 2, "ATOM_U16", false),
        DEF!("make_arg_ref", 7, 0, 2, "ATOM_U16", false),
        DEF!("make_var_ref_ref", 7, 0, 2, "ATOM_U16", false),
        DEF!("make_var_ref", 5, 0, 2, "ATOM", false),
        DEF!("for_in_start", 1, 1, 1, "NONE", false),
        DEF!("for_of_start", 1, 1, 3, "NONE", false),
        DEF!("for_await_of_start", 1, 1, 3, "NONE", false),
        DEF!("for_in_next", 1, 1, 3, "NONE", false),
        DEF!("for_of_next", 2, 3, 5, "U8", false),
        DEF!("iterator_check_object", 1, 1, 1, "NONE", false),
        DEF!("iterator_get_value_done", 1, 1, 2, "NONE", false),
        DEF!("iterator_close", 1, 3, 0, "NONE", false),
        DEF!("iterator_next", 1, 4, 4, "NONE", false),
        DEF!("iterator_call", 2, 4, 5, "U8", false),
        DEF!("initial_yield", 1, 0, 0, "NONE", false),
        DEF!("yield", 1, 1, 2, "NONE", false),
        DEF!("yield_star", 1, 1, 2, "NONE", false),
        DEF!("async_yield_star", 1, 1, 2, "NONE", false),
        DEF!("await", 1, 1, 1, "NONE", false),
        DEF!("neg", 1, 1, 1, "NONE", false),
        DEF!("plus", 1, 1, 1, "NONE", false),
        DEF!("dec", 1, 1, 1, "NONE", false),
        DEF!("inc", 1, 1, 1, "NONE", false),
        DEF!("post_dec", 1, 1, 2, "NONE", false),
        DEF!("post_inc", 1, 1, 2, "NONE", false),
        DEF!("dec_loc", 2, 0, 0, "LOC8", false),
        DEF!("inc_loc", 2, 0, 0, "LOC8", false),
        DEF!("add_loc", 2, 1, 0, "LOC8", false),
        DEF!("not", 1, 1, 1, "NONE", false),
        DEF!("lnot", 1, 1, 1, "NONE", false),
        DEF!("typeof", 1, 1, 1, "NONE", false),
        DEF!("delete", 1, 2, 1, "NONE", false),
        DEF!("delete_var", 5, 0, 1, "ATOM", false),
        DEF!("mul", 1, 2, 1, "NONE", false),
        DEF!("div", 1, 2, 1, "NONE", false),
        DEF!("mod", 1, 2, 1, "NONE", false),
        DEF!("add", 1, 2, 1, "NONE", false),
        DEF!("sub", 1, 2, 1, "NONE", false),
        DEF!("pow", 1, 2, 1, "NONE", false),
        DEF!("shl", 1, 2, 1, "NONE", false),
        DEF!("sar", 1, 2, 1, "NONE", false),
        DEF!("shr", 1, 2, 1, "NONE", false),
        DEF!("lt", 1, 2, 1, "NONE", false),
        DEF!("lte", 1, 2, 1, "NONE", false),
        DEF!("gt", 1, 2, 1, "NONE", false),
        DEF!("gte", 1, 2, 1, "NONE", false),
        DEF!("instanceof", 1, 2, 1, "NONE", false),
        DEF!("in", 1, 2, 1, "NONE", false),
        DEF!("eq", 1, 2, 1, "NONE", false),
        DEF!("neq", 1, 2, 1, "NONE", false),
        DEF!("strict_eq", 1, 2, 1, "NONE", false),
        DEF!("strict_neq", 1, 2, 1, "NONE", false),
        DEF!("and", 1, 2, 1, "NONE", false),
        DEF!("xor", 1, 2, 1, "NONE", false),
        DEF!("or", 1, 2, 1, "NONE", false),
        DEF!("is_undefined_or_null", 1, 1, 1, "NONE", false),
        DEF!("private_in", 1, 2, 1, "NONE", false),
        DEF!("nop", 1, 0, 0, "NONE", false),
        //
        // intermediate
        //
        def!("enter_scope", 3, 0, 0, "U16"),
        def!("leave_scope", 3, 0, 0, "U16"),
        def!("label", 5, 0, 0, "LABEL"),
        def!("scope_get_var_undef", 7, 0, 1, "ATOM_U16"),
        def!("scope_get_var", 7, 0, 1, "ATOM_U16"),
        def!("scope_put_var", 7, 1, 0, "ATOM_U16"),
        def!("scope_delete_var", 7, 0, 1, "ATOM_U16"),
        def!("scope_make_ref", 11, 0, 2, "ATOM_LABEL_U16"),
        def!("scope_get_ref", 7, 0, 2, "ATOM_U16"),
        def!("scope_put_var_init", 7, 0, 2, "ATOM_U16"),
        def!("scope_get_var_checkthis", 7, 0, 1, "ATOM_U16"),
        def!("scope_get_private_field", 7, 1, 1, "ATOM_U16"),
        def!("scope_get_private_field2", 7, 1, 2, "ATOM_U16"),
        def!("scope_put_private_field", 7, 2, 0, "ATOM_U16"),
        def!("scope_in_private_field", 7, 1, 1, "ATOM_U16"),
        def!("get_field_opt_chain", 5, 1, 1, "ATOM"),
        def!("get_array_el_opt_chain", 1, 2, 1, "NONE"),
        def!("set_class_name", 5, 1, 1, "U32"),
        def!("line_num", 5, 0, 0, "U32"),
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
            "    Opcode {{ #[cfg(feature=\"dump_bytecode\")] name: \"{}\", size: {}, n_pop: {}, n_push: {}, fmt: OpcodeFormat::{} }},",
            op.id, op.size, op.pop_n, op.push_n, op.fmt
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
