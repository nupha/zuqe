use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, opcodes},
    function::BlockEnv,
    object::PROP_TYPE_IDENT,
    parse::token::{
        CHAR_COMMA, CHAR_CURLY_L, CHAR_CURLY_R, CHAR_EQUAL, CHAR_SQUARE_L, CHAR_SQUARE_R,
    },
    stack_frame::ExecMode,
    utils::unlikely,
};

use super::{
    expr_lvalue::PutLValueEnum,
    state::{JSParseState, SKIP_HAS_ELLIPSIS},
    token::{TOK_CONST, TOK_ELLIPSIS, TOK_IDENT, TOK_LET, TOK_VAR},
};

impl<'s> JSParseState<'s> {
    /// parse { ... }
    fn parse_destructuring_curly(
        &mut self,
        tok: i32,
        is_arg: bool,
        has_ellipsis: bool,
        export: bool,
    ) -> Result<(), i32> {
        debug_assert_eq!(self.token.val, '{' as i32, "body starts with '{{'");

        self.next_token()?;

        let mut prop_name: Atom;
        let mut var_name: Atom;

        // throw an exception if the value cannot be converted to an object
        self.emit_op(OpCode::TO_OBJECT);
        if has_ellipsis {
            // add excludeList on stack just below src object
            self.emit_op(OpCode::OBJECT);
            self.emit_op(OpCode::SWAP);
        }

        let mut label_lvalue = -1;
        let mut opcode: i32 = 0;
        let mut scope: i32 = 0;

        macro_rules! set_val {
            () => {
                if tok != 0 {
                    self.define_var(var_name, tok, export)?;
                    scope = self.cur_func().scope_level as _;
                }

                if self.token.val == '=' as i32 {
                    // handle optional default value
                    self.emit_op(OpCode::DUP);
                    self.emit_op(OpCode::UNDEFINED);
                    self.emit_op(OpCode::STRICT_EQ);

                    let label_hasval = self.emit_goto(OpCode::IF_FALSE, -1);

                    self.next_token()?;
                    self.emit_op(OpCode::DROP);
                    self.parse_assign_expr()?;

                    if opcode == opcodes::op_scope_get_var as i32
                        || opcode == opcodes::OP_get_ref_value as i32
                    {
                        self.set_object_name(var_name.clone());
                    }
                    self.emit_label(label_hasval);
                }

                // store value into lvalue object
                self.put_lvalue(
                    opcode,
                    scope,
                    var_name,
                    label_lvalue,
                    PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH,
                    matches!(tok, TOK_CONST | TOK_LET),
                );

                if self.token.val == '}' as i32 {
                    break;
                }

                // accept a trailing comma before the '}'
                self.parse_expect(',' as i32)?;
            };
        }
        macro_rules! lvalue {
            ( ) => {
                let mut depth_lvalue1 = 0_i32;
                self.get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut var_name,
                    &mut label_lvalue,
                    &mut depth_lvalue1,
                    false,
                    '{' as i32,
                )?;

                /* swap ref and lvalue object if any */
                if prop_name.is_null() {
                    match depth_lvalue1 {
                        1 => {
                            /* source prop x -> x source prop */
                            self.emit_op(OpCode::ROT3R);
                        }
                        2 => {
                            /* source prop x y -> x y source prop */
                            self.emit_op(OpCode::SWAP2); /* t p2 s p1 */
                        }
                        3 => {
                            /* source prop x y z -> x y z source prop */
                            self.emit_op(OpCode::ROT5L);
                            self.emit_op(OpCode::ROT5L);
                        }
                        _ => {}
                    }
                } else {
                    match depth_lvalue1 {
                        1 => {
                            /* source x -> x source */
                            self.emit_op(OpCode::SWAP);
                        }
                        2 => {
                            /* source x y -> x y source */
                            self.emit_op(OpCode::ROT3L);
                        }
                        3 => {
                            /* source x y z -> x y z source */
                            self.emit_op(OpCode::ROT4L);
                        }
                        _ => {}
                    }
                }
            };
        }

        // parse till '}'
        while self.token.val != '}' as i32 {
            if self.token.val == TOK_ELLIPSIS {
                if !has_ellipsis {
                    self.ctx.throw_internal_error("unexpected ellipsis token");
                    return Err(-1);
                }

                self.next_token()?;

                let mut depth_lvalue = 0_i32;
                if tok != 0 {
                    var_name = self.parse_destructuring_var(is_arg)?;
                    opcode = opcodes::op_scope_get_var as _;
                    scope = self.cur_func().scope_level;
                    label_lvalue = -1;
                    depth_lvalue = 0;
                } else {
                    self.parse_left_hand_side_expr()?;

                    let mut a = Atom::NULL;
                    self.get_lvalue(
                        &mut opcode,
                        &mut scope,
                        &mut a,
                        &mut label_lvalue,
                        &mut depth_lvalue,
                        false,
                        '{' as i32,
                    )?;
                    var_name = a;
                }

                if self.token.val != '}' as i32 {
                    self.throw_syntax_error("assignment rest property must be last");
                    return Err(-1);
                }

                self.emit_op(OpCode::OBJECT); // target
                self.emit_op(OpCode::COPY_DATA_PROPERTIES);
                self.emit_u8((((depth_lvalue + 1) << 2) | ((depth_lvalue + 2) << 5)) as u8);

                set_val!();
                continue;
            }

            let r = self.parse_property_name(false, true, false)?;
            prop_name = r.0;
            let prop_type = r.1;

            var_name = Atom::NULL;
            opcode = opcodes::op_scope_get_var as _;
            scope = self.cur_func().scope_level;
            label_lvalue = -1;

            if prop_type == PROP_TYPE_IDENT {
                self.next_token()?;

                // token: [{ && after-parens-token: ,=}
                if matches!(self.token.val, CHAR_SQUARE_L | CHAR_CURLY_L)
                    && matches!(
                        {
                            let mut _p = 0;
                            self.parse_skip_parens_token(&mut _p, false)?
                        },
                        CHAR_COMMA | CHAR_EQUAL | CHAR_CURLY_R // , | = | }
                    )
                {
                    if prop_name.is_null() {
                        /* computed property name on stack */
                        if has_ellipsis {
                            /* define the property in excludeList */
                            self.emit_op(OpCode::TO_PROPKEY); /* avoid calling ToString twice */
                            self.emit_op(OpCode::PERM3); /* TOS: src excludeList prop */
                            self.emit_op(OpCode::NULL); /* TOS: src excludeList prop null */
                            self.emit_op(OpCode::DEFINE_ARRAY_EL); /* TOS: src excludeList prop */
                            self.emit_op(OpCode::PERM3); /* TOS: excludeList src prop */
                        }
                        /* get the computed property from the source object */
                        self.emit_op(OpCode::GET_ARRAY_EL2);
                    } else {
                        /* named property */
                        if has_ellipsis {
                            /* define the property in excludeList */
                            self.emit_op(OpCode::SWAP); /* TOS: src excludeList */
                            self.emit_op(OpCode::NULL); /* TOS: src excludeList null */
                            self.emit_op(OpCode::DEFINE_FIELD); /* TOS: src excludeList */
                            self.emit_atom(prop_name);
                            self.emit_op(OpCode::SWAP); /* TOS: excludeList src */
                        }
                        /* get the named property from the source object */
                        self.emit_op(OpCode::GET_FIELD2);
                        self.emit_atom(prop_name);
                    }

                    self.parse_destructuring_element(tok, is_arg, true, -1, true, export)?;

                    if self.token.val == '}' as i32 {
                        break;
                    } else {
                        // accept a trailing comma before the '}'
                        self.parse_expect(',' as i32)?;
                        continue;
                    }
                }

                if prop_name.is_null() {
                    self.emit_op(OpCode::TO_PROPKEY2);
                    if has_ellipsis {
                        /* define the property in excludeList */
                        self.emit_op(OpCode::PERM3);
                        self.emit_op(OpCode::NULL);
                        self.emit_op(OpCode::DEFINE_ARRAY_EL);
                        self.emit_op(OpCode::PERM3);
                    }
                    /* source prop -- source source prop */
                    self.emit_op(OpCode::DUP1);
                } else {
                    if has_ellipsis {
                        /* define the property in excludeList */
                        self.emit_op(OpCode::SWAP);
                        self.emit_op(OpCode::NULL);
                        self.emit_op(OpCode::DEFINE_FIELD);
                        self.emit_atom(prop_name);
                        self.emit_op(OpCode::SWAP);
                    }
                    /* source -- source source */
                    self.emit_op(OpCode::DUP);
                }

                if tok != 0 {
                    var_name = self.parse_destructuring_var(is_arg)?;
                } else {
                    self.parse_left_hand_side_expr()?;
                    lvalue!();
                }

                if prop_name.is_null() {
                    /* computed property name on stack */
                    /* XXX: should have OP_get_array_el2x with depth */
                    /* source prop -- val */
                    self.emit_op(OpCode::GET_ARRAY_EL);
                } else {
                    /* named property */
                    /* XXX: should have OP_get_field2x with depth */
                    /* source -- val */
                    self.emit_op(OpCode::GET_FIELD);
                    self.emit_atom(prop_name);
                }
            } else {
                // prop_type = PROP_TYPE_VAR, cannot be a computed property
                if is_arg {
                    self.parse_check_duplicate_parameter(prop_name)?;
                }

                if unlikely(
                    self.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
                        && matches!(prop_name, Atom::CONST_eval | Atom::CONST_arguments),
                ) {
                    self.throw_syntax_error("invalid destructuring target");
                    return Err(-1);
                }

                if has_ellipsis {
                    /* define the property in excludeList */
                    self.emit_op(OpCode::SWAP);
                    self.emit_op(OpCode::NULL);
                    self.emit_op(OpCode::DEFINE_FIELD);
                    self.emit_atom(prop_name);
                    self.emit_op(OpCode::SWAP);
                }

                let mut goto_lvalue = false;

                if matches!(tok, 0 | TOK_VAR) {
                    /* generate reference */
                    /* source -- source source */
                    self.emit_op(OpCode::DUP);
                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom(prop_name);
                    self.emit_u16(self.cur_func().scope_level as _);

                    goto_lvalue = true;
                    lvalue!();

                    // code after label lvalue
                    if prop_name.is_null() {
                        /* computed property name on stack */
                        /* XXX: should have OP_get_array_el2x with depth */
                        /* source prop -- val */
                        self.emit_op(OpCode::GET_ARRAY_EL);
                    } else {
                        /* named property */
                        /* XXX: should have OP_get_field2x with depth */
                        /* source -- val */
                        self.emit_op(OpCode::GET_FIELD);
                        self.emit_atom(prop_name);
                    }
                }

                if !goto_lvalue {
                    var_name = prop_name;
                    /* source -- source val */
                    self.emit_op(OpCode::GET_FIELD2);
                    self.emit_atom(prop_name);
                }
            }

            set_val!();
        }

        // drop the source object
        self.emit_op(OpCode::DROP);
        if has_ellipsis {
            self.emit_op(OpCode::DROP); // pop excludeList
        }

        self.next_token()?;
        Ok(())
    }

    /// parse [ ... ]
    fn parse_destructuring_square(
        &mut self,
        tok: i32,
        is_arg: bool,
        mut skip_bits: i32,
        export: bool,
    ) -> Result<(), i32> {
        debug_assert_eq!(self.token.val, '[' as i32, "body starts with '['");

        self.next_token()?;

        // the block environment is only needed in generators in case 'yield' triggers a 'return'
        let mut block_env = BlockEnv {
            drop_count: 2,
            has_iterator: true,
            ..Default::default()
        };
        self.cur_func_mut().push_break_entry(&mut block_env);
        self.emit_op(OpCode::FOR_OF_START);

        let mut has_spread = false;

        // parse till ']'
        while self.token.val != ']' as i32 {
            // get the next value
            if self.token.val == TOK_ELLIPSIS {
                self.next_token()?;
                if self.token.val == ',' as i32 || self.token.val == ']' as i32 {
                    self.throw_syntax_error("missing binding pattern...");
                    return Err(-1);
                }
                has_spread = true;
            }

            let mut opcode: i32 = 0;
            let mut scope: i32 = 0;
            let mut label_lvalue = -1;

            let parse_expr = match self.token.val {
                CHAR_COMMA => {
                    // do nothing, skip the value, has_spread is false */
                    self.emit_op(OpCode::FOR_OF_NEXT);
                    self.emit_u8(0);
                    self.emit_op(OpCode::DROP);
                    self.emit_op(OpCode::DROP);
                    false
                }
                CHAR_SQUARE_L | CHAR_CURLY_L => {
                    match self.parse_skip_parens_token(&mut skip_bits, false)? {
                        t @ (CHAR_COMMA | CHAR_EQUAL | CHAR_SQUARE_R) => {
                            if has_spread {
                                if t == '=' as i32 {
                                    self.throw_syntax_error(
                                        "rest element cannot have a default value",
                                    );
                                    return Err(-1);
                                }
                                self.emit_spread_code(0);
                            } else {
                                self.emit_op(OpCode::FOR_OF_NEXT);
                                self.emit_u8(0);
                                self.emit_op(OpCode::DROP);
                            }

                            self.parse_destructuring_element(
                                tok,
                                is_arg,
                                true,
                                skip_bits & SKIP_HAS_ELLIPSIS,
                                true,
                                export,
                            )?;

                            false
                        }
                        _ => true,
                    }
                }
                _ => true,
            };

            if parse_expr {
                let mut var_name = Atom::NULL;
                let mut enum_depth = 0;

                if tok != 0 {
                    var_name = self.parse_destructuring_var(is_arg)?;
                    self.define_var(var_name, tok, export)?;
                    opcode = opcodes::op_scope_get_var as i32;
                    scope = self.cur_func().scope_level;
                } else {
                    self.parse_left_hand_side_expr()?;

                    self.get_lvalue(
                        &mut opcode,
                        &mut scope,
                        &mut var_name,
                        &mut label_lvalue,
                        &mut enum_depth,
                        false,
                        '[' as i32,
                    )?;
                }

                if has_spread {
                    self.emit_spread_code(enum_depth);
                } else {
                    self.emit_op(OpCode::FOR_OF_NEXT);
                    self.emit_u8(enum_depth as _);
                    self.emit_op(OpCode::DROP);
                }

                if self.token.val == '=' as i32 && !has_spread {
                    /* handle optional default value */
                    self.emit_op(OpCode::DUP);
                    self.emit_op(OpCode::UNDEFINED);
                    self.emit_op(OpCode::STRICT_EQ);

                    let label_hasval = self.emit_goto(OpCode::IF_FALSE, -1);
                    self.next_token()?;

                    self.emit_op(OpCode::DROP);
                    self.parse_assign_expr()?;

                    if opcode == opcodes::op_scope_get_var as i32
                        || opcode == opcodes::OP_get_ref_value as i32
                    {
                        self.set_object_name(var_name);
                    }
                    self.emit_label(label_hasval);
                }

                /* store value into lvalue object */
                self.put_lvalue(
                    opcode,
                    scope,
                    var_name,
                    label_lvalue,
                    PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH,
                    tok == TOK_CONST || tok == TOK_LET,
                );
            }

            if self.token.val == ']' as i32 {
                break;
            }
            if has_spread {
                self.throw_syntax_error("rest element must be the last one");
                return Err(-1);
            }

            // accept a trailing comma before the ']'
            self.parse_expect(',' as i32)?;
        }

        // close iterator object:
        // if completed, enum_obj has been replaced by undefined
        self.emit_op(OpCode::ITERATOR_CLOSE);

        self.cur_func_mut().pop_break_entry();

        self.next_token()?;
        Ok(())
    }

    /// TODO: add argument export:bool to allow add variables to module export entry,
    /// like that in parse_var() does.
    pub fn parse_destructuring_element(
        &mut self,
        tok: i32,
        is_arg: bool,
        hasval: bool,
        mut has_ellipsis: i32,
        allow_initializer: bool,
        export: bool,
    ) -> Result<bool, i32> {
        let mut skip_bits: i32 = 0;

        if has_ellipsis < 0 {
            // Pre-parse destructuration target for spread detection
            self.parse_skip_parens_token(&mut skip_bits, false)?;
            has_ellipsis = skip_bits & SKIP_HAS_ELLIPSIS;
        }

        // generate labels
        let label_parse = self.cur_func_mut().add_label()?;
        let label_assign = self.cur_func_mut().add_label()?;

        let start_addr = self.cur_func().byte_code.len();
        if hasval {
            // Consume value from stack
            self.emit_op(OpCode::DUP);
            self.emit_op(OpCode::UNDEFINED);
            self.emit_op(OpCode::STRICT_EQ);
            self.emit_goto(OpCode::IF_TRUE, label_parse as i32);
            self.emit_label(label_assign as _);
        } else {
            self.emit_goto(OpCode::GOTO, label_parse as _);
            self.emit_label(label_assign as _);
            self.emit_op(OpCode::DUP);
        }
        let assign_addr = self.cur_func().byte_code.len();

        // parse body
        match self.token.val {
            CHAR_CURLY_L => {
                // {: parse { body }
                self.parse_destructuring_curly(tok, is_arg, has_ellipsis != 0, export)?;
            }
            CHAR_SQUARE_L => {
                // [: parse [ body ]
                self.parse_destructuring_square(tok, is_arg, skip_bits, export)?;
            }
            _ => {
                self.throw_syntax_error("invalid assignment syntax");
                return Err(-1);
            }
        }

        // body was parsed.
        let has_initializer = if self.token.val == '=' as i32 && allow_initializer {
            let label_done = self.emit_goto(OpCode::GOTO, -1);

            self.next_token()?;
            self.emit_label(label_parse as _);
            if hasval {
                self.emit_op(OpCode::DROP);
            }
            self.parse_assign_expr()?;
            self.emit_goto(OpCode::GOTO, label_assign as _);
            self.emit_label(label_done);
            true
        } else {
            // normally hasval is true except if js_parse_skip_parens_token() was wrong in the parsing
            if !hasval {
                self.throw_syntax_error("too complicated destructuring expression");
                return Err(-1);
            }

            {
                let cur_func = self.cur_func_mut();
                cur_func.byte_code[start_addr..assign_addr].fill(OpCode::NOP as u8);
                cur_func.label_slots[label_parse as usize].ref_count -= 1;
            }

            false
        };
        Ok(has_initializer)
    }

    fn parse_destructuring_var(&mut self, is_arg: bool) -> Result<Atom, i32> {
        if unlikely(
            (self.token.val != TOK_IDENT || self.token.data.is_reserved())
                || (self.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
                    && matches!(
                        self.token.atom().unwrap(),
                        Atom::CONST_eval | Atom::CONST_arguments
                    )),
        ) {
            self.throw_syntax_error("invalid destructuring target");
            Err(-1)
        } else {
            let name = self.token.atom().unwrap();
            if is_arg {
                self.parse_check_duplicate_parameter(name)?;
            }
            self.next_token()?;
            Ok(name)
        }
    }

    fn parse_check_duplicate_parameter(&self, name: Atom) -> Result<(), i32> {
        // Check for duplicate parameter names
        if self.cur_func().args.iter().any(|vd| vd.var_name == name) {
            self.throw_syntax_error("duplicate parameter names not allowed in this context");
            return Err(-1);
        }

        if self.cur_func().vars.iter().any(|vd| vd.var_name == name) {
            self.throw_syntax_error("duplicate parameter names not allowed in this context");
            return Err(-1);
        }

        Ok(())
    }
}
