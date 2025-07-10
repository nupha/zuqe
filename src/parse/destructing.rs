use crate::{
    atom::{
        JS_ATOM_NULL, JS_FreeAtom, JSAtom,
        consts::{JS_ATOM_arguments, JS_ATOM_eval},
    },
    bytecode::{OpCode, OpCodeTemp, opcodes},
    context::JS_MODE_STRICT,
    function::{BlockEnv, pop_break_entry, push_break_entry},
    object::PROP_TYPE_IDENT,
    parse::token::{CHAR_COMMA, CHAR_CURLY_R},
};

use super::{
    class::js_parse_property_name,
    expr_lvalue::PutLValueEnum,
    function::js_parse_check_duplicate_parameter,
    parse::js_parse_skip_parens_token,
    state::{JSParseState, SKIP_HAS_ELLIPSIS},
    token::{TOK_CONST, TOK_ELLIPSIS, TOK_IDENT, TOK_LET, TOK_VAR},
};

// FIXME: bug in this func. 25/07/09 debug: should return 0, but got -1
pub unsafe fn js_parse_destructuring_element(
    s: *mut JSParseState,
    tok: i32,
    is_arg: bool,
    hasval: bool,
    mut has_ellipsis: i32,
    allow_initializer: bool,
) -> i32 {
    let ctx = &(*s).ctx;

    let mut label_parse: i32 = 0;
    let mut label_assign: i32 = 0;
    let mut label_done: i32 = 0;
    let mut label_lvalue: i32 = 0;
    let mut depth_lvalue: i32 = 0;
    let mut start_addr: i32 = 0;
    let mut assign_addr: i32 = 0;
    let mut prop_name = JS_ATOM_NULL;
    let mut var_name = JS_ATOM_NULL;
    let mut opcode: i32 = 0;
    let mut scope: i32 = 0;
    let mut tok1: i32 = 0;
    let mut skip_bits: i32 = 0;
    let mut has_initializer: bool = false;

    macro_rules! prop_error {
        () => {
            JS_FreeAtom(ctx, prop_name);
            JS_FreeAtom(ctx, var_name);
            return -1;
        };
    }
    macro_rules! var_error {
        () => {
            JS_FreeAtom(ctx, var_name);
            return -1;
        };
    }
    macro_rules! set_val {
        () => {
            if tok != 0 {
                if (*s).define_var(var_name, tok).is_err() {
                    var_error!();
                }
                scope = (*s).cur_func().scope_level as _;
            }

            if (*s).token.val == '=' as i32 {
                // handle optional default value
                (*s).emit_op(OpCode::DUP);
                (*s).emit_op(OpCode::UNDEFINED);
                (*s).emit_op(OpCode::STRICT_EQ);

                let label_hasval = (*s).emit_goto(OpCode::IF_FALSE, -1);

                if (*s).next_token().is_err() {
                    JS_FreeAtom(ctx, var_name);
                    JS_FreeAtom(ctx, prop_name);
                    return -1;
                }

                (*s).emit_op(OpCode::DROP);

                if (*s).parse_assign_expr().is_err() {
                    JS_FreeAtom(ctx, var_name);
                    JS_FreeAtom(ctx, prop_name);
                    return -1;
                }
                if opcode == opcodes::op_scope_get_var as i32
                    || opcode == opcodes::OP_get_ref_value as i32
                {
                    (*s).set_object_name(var_name);
                }
                (*s).emit_label(label_hasval);
            }

            // store value into lvalue object
            (*s).put_lvalue(
                opcode,
                scope,
                var_name,
                label_lvalue,
                PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH,
                matches!(tok, TOK_CONST | TOK_LET),
            );

            if (*s).token.val == CHAR_CURLY_R {
                break;
            }

            // accept a trailing comma before the '}'
            if (*s).parse_expect(CHAR_COMMA).is_err() {
                return -1;
            }
        };
    }
    macro_rules! lvalue {
        () => {
            if (*s)
                .get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut var_name,
                    &mut label_lvalue,
                    &mut depth_lvalue,
                    false,
                    '{' as i32,
                )
                .is_err()
            {
                prop_error!();
            }

            /* swap ref and lvalue object if any */
            if prop_name == JS_ATOM_NULL {
                match depth_lvalue {
                    1 => {
                        /* source prop x -> x source prop */
                        (*s).emit_op(OpCode::ROT3R);
                    }
                    2 => {
                        /* source prop x y -> x y source prop */
                        (*s).emit_op(OpCode::SWAP2); /* t p2 s p1 */
                    }
                    3 => {
                        /* source prop x y z -> x y z source prop */
                        (*s).emit_op(OpCode::ROT5L);
                        (*s).emit_op(OpCode::ROT5L);
                    }
                    _ => {}
                }
            } else {
                match depth_lvalue {
                    1 => {
                        /* source x -> x source */
                        (*s).emit_op(OpCode::SWAP);
                    }
                    2 => {
                        /* source x y -> x y source */
                        (*s).emit_op(OpCode::ROT3L);
                    }
                    3 => {
                        /* source x y z -> x y z source */
                        (*s).emit_op(OpCode::ROT4L);
                    }
                    _ => {}
                }
            }
        };
    }

    if has_ellipsis < 0 {
        // Pre-parse destructuration target for spread detection
        js_parse_skip_parens_token(s, &mut skip_bits as *mut i32, false);
        has_ellipsis = skip_bits & SKIP_HAS_ELLIPSIS;
    }

    label_parse = (*s).cur_func_mut().add_label().map_or(-1, |l| l as i32);
    label_assign = (*s).cur_func_mut().add_label().map_or(-1, |l| l as i32);

    start_addr = (*s).cur_func().byte_code.len() as i32;
    if hasval {
        // Consume value from stack
        (*s).emit_op(OpCode::DUP);
        (*s).emit_op(OpCode::UNDEFINED);
        (*s).emit_op(OpCode::STRICT_EQ);
        (*s).emit_goto(OpCode::IF_TRUE, label_parse);
        (*s).emit_label(label_assign);
    } else {
        (*s).emit_goto(OpCode::GOTO, label_parse);
        (*s).emit_label(label_assign);
        (*s).emit_op(OpCode::DUP);
    }
    assign_addr = (*s).cur_func().byte_code.len() as i32;

    if (*s).token.val == '{' as i32 {
        if (*s).next_token().is_err() {
            return -1;
        }

        // throw an exception if the value cannot be converted to an object
        (*s).emit_op(OpCode::TO_OBJECT);
        if has_ellipsis != 0 {
            // add excludeList on stack just below src object
            (*s).emit_op(OpCode::OBJECT);
            (*s).emit_op(OpCode::SWAP);
        }

        while (*s).token.val != '}' as i32 {
            if (*s).token.val == TOK_ELLIPSIS {
                if has_ellipsis == 0 {
                    ctx.JS_ThrowInternalError("unexpected ellipsis token");
                    return -1;
                }
                if (*s).next_token().is_err() {
                    return -1;
                }
                if tok != 0 {
                    var_name = js_parse_destructuring_var(s, tok, is_arg);
                    if var_name == JS_ATOM_NULL {
                        return -1;
                    }
                    opcode = opcodes::op_scope_get_var as _;
                    scope = (*s).cur_func().scope_level;
                    label_lvalue = -1;
                    depth_lvalue = 0;
                } else {
                    if (*s).parse_left_hand_side_expr().is_err() {
                        return -1;
                    }

                    if (*s)
                        .get_lvalue(
                            &mut opcode,
                            &mut scope,
                            &mut var_name,
                            &mut label_lvalue,
                            &mut depth_lvalue,
                            false,
                            '{' as i32,
                        )
                        .is_err()
                    {
                        return -1;
                    }
                }

                if (*s).token.val != '}' as i32 {
                    (*s).throw_syntax_error("assignment rest property must be last");
                    var_error!();
                }

                (*s).emit_op(OpCode::OBJECT); // target
                (*s).emit_op(OpCode::COPY_DATA_PROPERTIES);
                (*s).emit_u8((((depth_lvalue + 1) << 2) | ((depth_lvalue + 2) << 5)) as u8);

                set_val!();
                continue;
            }

            let prop_type = js_parse_property_name(s, &mut prop_name, false, true, false);
            if prop_type < 0 {
                return -1;
            }
            var_name = JS_ATOM_NULL;
            opcode = opcodes::op_scope_get_var as _;
            scope = (*s).cur_func().scope_level;
            label_lvalue = -1;
            depth_lvalue = 0;

            if prop_type == PROP_TYPE_IDENT {
                // #89
                if (*s).next_token().is_err() {
                    prop_error!();
                }

                if ((*s).token.val == '[' as i32 || (*s).token.val == '{' as i32)
                    && ({
                        tok1 = js_parse_skip_parens_token(s, &mut skip_bits, false);
                        tok1
                    } == ',' as i32
                        || tok1 == '=' as i32
                        || tok1 == '}' as i32)
                {
                    if prop_name == JS_ATOM_NULL {
                        /* computed property name on stack */
                        if has_ellipsis != 0 {
                            /* define the property in excludeList */
                            (*s).emit_op(OpCode::TO_PROPKEY); /* avoid calling ToString twice */
                            (*s).emit_op(OpCode::PERM3); /* TOS: src excludeList prop */
                            (*s).emit_op(OpCode::NULL); /* TOS: src excludeList prop null */
                            (*s).emit_op(OpCode::DEFINE_ARRAY_EL); /* TOS: src excludeList prop */
                            (*s).emit_op(OpCode::PERM3); /* TOS: excludeList src prop */
                        }
                        /* get the computed property from the source object */
                        (*s).emit_op(OpCode::GET_ARRAY_EL2);
                    } else {
                        /* named property */
                        if has_ellipsis != 0 {
                            /* define the property in excludeList */
                            (*s).emit_op(OpCode::SWAP); /* TOS: src excludeList */
                            (*s).emit_op(OpCode::NULL); /* TOS: src excludeList null */
                            (*s).emit_op(OpCode::DEFINE_FIELD); /* TOS: src excludeList */
                            (*s).emit_atom_dup(prop_name);
                            (*s).emit_op(OpCode::SWAP); /* TOS: excludeList src */
                        }
                        /* get the named property from the source object */
                        (*s).emit_op(OpCode::GET_FIELD2);
                        (*s).emit_u32(prop_name.0);
                    }
                    if js_parse_destructuring_element(s, tok, is_arg, true, -1, true) < 0 {
                        return -1;
                    }

                    if (*s).token.val == '}' as i32 {
                        break;
                    }

                    /* accept a trailing comma before the '}' */
                    if (*s).parse_expect(CHAR_COMMA).is_err() {
                        return -1;
                    }

                    continue;
                }

                if prop_name == JS_ATOM_NULL {
                    (*s).emit_op(OpCode::TO_PROPKEY2);
                    if has_ellipsis != 0 {
                        /* define the property in excludeList */
                        (*s).emit_op(OpCode::PERM3);
                        (*s).emit_op(OpCode::NULL);
                        (*s).emit_op(OpCode::DEFINE_ARRAY_EL);
                        (*s).emit_op(OpCode::PERM3);
                    }
                    /* source prop -- source source prop */
                    (*s).emit_op(OpCode::DUP1);
                } else {
                    if has_ellipsis != 0 {
                        /* define the property in excludeList */
                        (*s).emit_op(OpCode::SWAP);
                        (*s).emit_op(OpCode::NULL);
                        (*s).emit_op(OpCode::DEFINE_FIELD);
                        (*s).emit_atom_dup(prop_name);
                        (*s).emit_op(OpCode::SWAP);
                    }
                    /* source -- source source */
                    (*s).emit_op(OpCode::DUP);
                }
                if tok != 0 {
                    var_name = js_parse_destructuring_var(s, tok, is_arg);
                    if var_name == JS_ATOM_NULL {
                        prop_error!();
                    }
                } else {
                    if (*s).parse_left_hand_side_expr().is_err() {
                        prop_error!();
                    }

                    // #160 label lvalue
                    lvalue!();
                } // #197

                if prop_name == JS_ATOM_NULL {
                    /* computed property name on stack */
                    /* XXX: should have OP_get_array_el2x with depth */
                    /* source prop -- val */
                    (*s).emit_op(OpCode::GET_ARRAY_EL);
                } else {
                    /* named property */
                    /* XXX: should have OP_get_field2x with depth */
                    /* source -- val */
                    (*s).emit_op(OpCode::GET_FIELD);
                    (*s).emit_u32(prop_name.0);
                }
            } else {
                // #210
                /* prop_type = PROP_TYPE_VAR, cannot be a computed property */
                if is_arg && js_parse_check_duplicate_parameter(s, prop_name) != 0 {
                    prop_error!();
                }

                if (*s).cur_func().js_mode & JS_MODE_STRICT as u8 != 0
                    && (prop_name == JS_ATOM_eval || prop_name == JS_ATOM_arguments)
                {
                    (*s).throw_syntax_error("invalid destructuring target");
                    prop_error!();
                }
                if has_ellipsis != 0 {
                    /* define the property in excludeList */
                    (*s).emit_op(OpCode::SWAP);
                    (*s).emit_op(OpCode::NULL);
                    (*s).emit_op(OpCode::DEFINE_FIELD);
                    (*s).emit_atom_dup(prop_name);
                    (*s).emit_op(OpCode::SWAP);
                }

                let mut goto_lvalue = false;

                if tok == 0 || tok == TOK_VAR {
                    /* generate reference */
                    /* source -- source source */
                    (*s).emit_op(OpCode::DUP);
                    (*s).emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    (*s).emit_atom_dup(prop_name);
                    (*s).emit_u16((*s).cur_func().scope_level as _);

                    goto_lvalue = true;
                    lvalue!(); // #234 goto lvalue

                    // code after label lvalue
                    if prop_name == JS_ATOM_NULL {
                        /* computed property name on stack */
                        /* XXX: should have OP_get_array_el2x with depth */
                        /* source prop -- val */
                        (*s).emit_op(OpCode::GET_ARRAY_EL);
                    } else {
                        /* named property */
                        /* XXX: should have OP_get_field2x with depth */
                        /* source -- val */
                        (*s).emit_op(OpCode::GET_FIELD);
                        (*s).emit_u32(prop_name.0);
                    }
                }

                if !goto_lvalue {
                    var_name = prop_name.dup(ctx.rt());
                    /* source -- source val */
                    (*s).emit_op(OpCode::GET_FIELD2);
                    (*s).emit_u32(prop_name.0);
                }
            }

            set_val!();
        } // while

        // drop the source object
        (*s).emit_op(OpCode::DROP);
        if has_ellipsis != 0 {
            (*s).emit_op(OpCode::DROP); // pop excludeList
        }
        if (*s).next_token().is_err() {
            return -1;
        }
    } else if (*s).token.val == '[' as i32 {
        // #279
        let mut has_spread: bool;
        let mut enum_depth: i32;
        let mut block_env: BlockEnv = std::mem::zeroed();

        if (*s).next_token().is_err() {
            return -1;
        }

        /* the block environment is only needed in generators in case
        'yield' triggers a 'return' */
        push_break_entry((*s).cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 2);
        block_env.has_iterator = true;
        (*s).emit_op(OpCode::FOR_OF_START);
        has_spread = false;
        while (*s).token.val != ']' as i32 {
            /* get the next value */
            if (*s).token.val == TOK_ELLIPSIS {
                if (*s).next_token().is_err() {
                    return -1;
                }
                if (*s).token.val == ',' as i32 || (*s).token.val == ']' as i32 {
                    (*s).throw_syntax_error("missing binding pattern...");
                    return -1;
                }
                has_spread = true;
            }
            if (*s).token.val == ',' as i32 {
                /* do nothing, skip the value, has_spread is false */
                (*s).emit_op(OpCode::FOR_OF_NEXT);
                (*s).emit_u8(0);
                (*s).emit_op(OpCode::DROP);
                (*s).emit_op(OpCode::DROP);
            } else if ((*s).token.val == '[' as i32 || (*s).token.val == '{' as i32)
                && ({
                    tok1 = js_parse_skip_parens_token(s, &mut skip_bits, false);
                    tok1
                } == ',' as i32
                    || tok1 == '=' as i32
                    || tok1 == ']' as i32)
            {
                if has_spread {
                    if tok1 == '=' as i32 {
                        (*s).throw_syntax_error("rest element cannot have a default value");
                        return -1;
                    }
                    (*s).emit_spread_code(0);
                } else {
                    (*s).emit_op(OpCode::FOR_OF_NEXT);
                    (*s).emit_u8(0);
                    (*s).emit_op(OpCode::DROP);
                }
                if js_parse_destructuring_element(
                    s,
                    tok,
                    is_arg,
                    true,
                    skip_bits & SKIP_HAS_ELLIPSIS,
                    true,
                ) < 0
                {
                    return -1;
                }
            } else {
                var_name = JS_ATOM_NULL;
                enum_depth = 0;
                if tok != 0 {
                    var_name = js_parse_destructuring_var(s, tok, is_arg);
                    if var_name == JS_ATOM_NULL {
                        var_error!();
                    }
                    if (*s).define_var(var_name, tok).is_err() {
                        var_error!();
                    }
                    opcode = opcodes::op_scope_get_var as i32;
                    scope = (*s).cur_func().scope_level;
                } else {
                    if (*s).parse_left_hand_side_expr().is_err() {
                        return -1;
                    }

                    if (*s)
                        .get_lvalue(
                            &mut opcode,
                            &mut scope,
                            &mut var_name,
                            &mut label_lvalue,
                            &mut enum_depth,
                            false,
                            '[' as i32,
                        )
                        .is_err()
                    {
                        return -1;
                    }
                }
                if has_spread {
                    (*s).emit_spread_code(enum_depth);
                } else {
                    (*s).emit_op(OpCode::FOR_OF_NEXT);
                    (*s).emit_u8(enum_depth as _);
                    (*s).emit_op(OpCode::DROP);
                }
                if (*s).token.val == '=' as i32 && !has_spread {
                    /* handle optional default value */
                    (*s).emit_op(OpCode::DUP);
                    (*s).emit_op(OpCode::UNDEFINED);
                    (*s).emit_op(OpCode::STRICT_EQ);
                    let label_hasval = (*s).emit_goto(OpCode::IF_FALSE, -1);
                    if (*s).next_token().is_err() {
                        var_error!();
                    }
                    (*s).emit_op(OpCode::DROP);
                    if (*s).parse_assign_expr().is_err() {
                        var_error!();
                    }
                    if opcode == opcodes::op_scope_get_var as i32
                        || opcode == opcodes::OP_get_ref_value as i32
                    {
                        (*s).set_object_name(var_name);
                    }
                    (*s).emit_label(label_hasval);
                }
                /* store value into lvalue object */
                (*s).put_lvalue(
                    opcode,
                    scope,
                    var_name,
                    label_lvalue,
                    PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH,
                    tok == TOK_CONST || tok == TOK_LET,
                );
            }

            if (*s).token.val == ']' as i32 {
                break;
            }
            if has_spread {
                (*s).throw_syntax_error("rest element must be the last one");
                return -1;
            }
            /* accept a trailing comma before the ']' */
            if (*s).parse_expect(CHAR_COMMA).is_err() {
                return -1;
            }
        }

        /* close iterator object:
        if completed, enum_obj has been replaced by undefined */
        (*s).emit_op(OpCode::ITERATOR_CLOSE);
        pop_break_entry((*s).cur_func_mut());
        if (*s).next_token().is_err() {
            return -1;
        }
    } else {
        (*s).throw_syntax_error("invalid assignment syntax");
        return -1;
    }

    if (*s).token.val == '=' as i32 && allow_initializer {
        label_done = (*s).emit_goto(OpCode::GOTO, -1);
        if (*s).next_token().is_err() {
            return -1;
        }
        (*s).emit_label(label_parse);
        if hasval {
            (*s).emit_op(OpCode::DROP);
        }
        if (*s).parse_assign_expr().is_err() {
            return -1;
        }
        (*s).emit_goto(OpCode::GOTO, label_assign);
        (*s).emit_label(label_done);
        has_initializer = true;
    } else {
        /* normally hasval is true except if
        js_parse_skip_parens_token() was wrong in the parsing */
        //        assert(hasval);
        if !hasval {
            (*s).throw_syntax_error("too complicated destructuring expression");
            return -1;
        }

        (*s).cur_func_mut().byte_code[start_addr as usize..assign_addr as usize]
            .fill(OpCode::NOP as u8);

        (*s).cur_func_mut().label_slots[label_parse as usize].ref_count -= 1;
        has_initializer = false;
    }

    has_initializer as i32
}

pub unsafe fn js_parse_destructuring_var(s: *mut JSParseState, tok: i32, is_arg: bool) -> JSAtom {
    if ((*s).token.val != TOK_IDENT || (*s).token.data.is_reserved())
        || ((*s).cur_func().js_mode & JS_MODE_STRICT as u8 != 0
            && matches!(
                (*s).token.data.ident_atom().unwrap(),
                JS_ATOM_eval | JS_ATOM_arguments
            ))
    {
        (*s).throw_syntax_error("invalid destructuring target");
        return JS_ATOM_NULL;
    }

    let name = (*s).token.data.ident_atom().unwrap().dup(&(*s).ctx.rt());
    if is_arg && js_parse_check_duplicate_parameter(s, name) != 0 {
        JS_FreeAtom(&(*s).ctx, name);
        return JS_ATOM_NULL;
    }
    if (*s).next_token().is_err() {
        JS_FreeAtom(&(*s).ctx, name);
        return JS_ATOM_NULL;
    }

    name
}
