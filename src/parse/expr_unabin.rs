use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, opcodes},
    error::JS_THROW_ERROR_DELETE_SUPER,
    parse::{
        state::{PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POW_ALLOWED},
        token::TOK_IN,
    },
    stack_frame::ExecMode,
    utils::read_from_ptr,
};

use super::{
    emit::get_prev_opcode,
    expr_lvalue::PutLValueEnum,
    state::{JSParseState, PF_POSTFIX_CALL, PF_POW_FORBIDDEN},
    token::{
        CHAR_MINUS, CHAR_PERCENT, CHAR_PLUS, CHAR_SLASH, CHAR_STAR, TOK_AWAIT, TOK_DEC, TOK_DELETE,
        TOK_EQ, TOK_GTE, TOK_INC, TOK_INSTANCEOF, TOK_LTE, TOK_NEQ, TOK_POW, TOK_PRIVATE_NAME,
        TOK_SAR, TOK_SHL, TOK_SHR, TOK_STRICT_EQ, TOK_STRICT_NEQ, TOK_TYPEOF, TOK_VOID,
    },
};

impl<'s> JSParseState<'s> {
    /// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    pub(super) fn parse_expr_binary(&mut self, level: i32, parse_flags: i32) -> Result<(), i32> {
        if level == 0 {
            return self.parse_unary((parse_flags & PF_ARROW_FUNC) | PF_POW_ALLOWED);
        } else if self.token.val == TOK_PRIVATE_NAME
            && parse_flags & PF_IN_ACCEPTED != 0
            && level == 4
            && self.peek_token(false) == TOK_IN
        {
            let atom = self.token.atom().unwrap();

            self.next_token()?;
            if self.token.val != TOK_IN {
                self.throw_syntax_error("expect 'in'");
                return Err(-1);
            }

            self.next_token()?;
            self.parse_expr_binary(level - 1, parse_flags & !PF_ARROW_FUNC)?;

            self.emit_op_temp(OpCodeTemp::SCOPE_IN_PRIVATE_FIELD);
            self.emit_atom(atom);
            self.emit_u16(self.cur_func().scope_level as _);

            return Ok(());
        } else {
            self.parse_expr_binary(level - 1, parse_flags)?;
        }

        loop {
            let op = self.token.val;
            let opcode = match level {
                1 => match op {
                    CHAR_STAR => OpCode::MUL,    // '*'
                    CHAR_SLASH => OpCode::DIV,   // '/'
                    CHAR_PERCENT => OpCode::MOD, // '%'
                    _ => {
                        return Ok(());
                    }
                },
                2 => match op {
                    CHAR_PLUS => OpCode::ADD,  // '+'
                    CHAR_MINUS => OpCode::SUB, // '-'
                    _ => {
                        return Ok(());
                    }
                },
                3 => match op {
                    TOK_SHL => OpCode::SHL,
                    TOK_SAR => OpCode::SAR,
                    TOK_SHR => OpCode::SHR,
                    _ => {
                        return Ok(());
                    }
                },
                4 => match op {
                    0x3c => OpCode::LT, // '<'
                    0x3e => OpCode::GT, // '>'
                    TOK_LTE => OpCode::LTE,
                    TOK_GTE => OpCode::GTE,
                    TOK_INSTANCEOF => OpCode::INSTANCEOF,
                    TOK_IN => {
                        if parse_flags & PF_IN_ACCEPTED != 0 {
                            OpCode::IN
                        } else {
                            return Ok(());
                        }
                    }
                    _ => {
                        return Ok(());
                    }
                },
                5 => match op {
                    TOK_EQ => OpCode::EQ,
                    TOK_NEQ => OpCode::NEQ,
                    TOK_STRICT_EQ => OpCode::STRICT_EQ,
                    TOK_STRICT_NEQ => OpCode::STRICT_NEQ,
                    _ => {
                        return Ok(());
                    }
                },
                6 => match op {
                    0x26 => OpCode::AND, // '&'
                    _ => {
                        return Ok(());
                    }
                },
                7 => match op {
                    0x5e => OpCode::XOR, // '^'
                    _ => {
                        return Ok(());
                    }
                },
                8 => match op {
                    0x7c => OpCode::OR, // '|'
                    _ => {
                        return Ok(());
                    }
                },
                _ => unreachable!(),
            };

            self.next_token()?;
            self.parse_expr_binary(level - 1, parse_flags & !PF_ARROW_FUNC)?;

            self.emit_op(opcode);
        }
    }

    // allowed parse_flags: PF_ARROW_FUNC, PF_POW_ALLOWED, PF_POW_FORBIDDEN
    pub(super) fn parse_unary(&mut self, mut parse_flags: i32) -> Result<(), i32> {
        match self.token.val {
            // '+', '-', '!', '~'
            op @ (CHAR_PLUS | CHAR_MINUS | 0x21 | 0x7e | TOK_VOID) => {
                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                match op {
                    0x2d => self.emit_op(OpCode::NEG),  // '-'
                    0x2b => self.emit_op(OpCode::PLUS), // '+'
                    0x21 => self.emit_op(OpCode::LNOT), // '!'
                    0x7e => self.emit_op(OpCode::NOT),  // '~'
                    TOK_VOID => {
                        self.emit_op(OpCode::DROP);
                        self.emit_op(OpCode::UNDEFINED);
                    }
                    _ => unreachable!(),
                }
                parse_flags = 0;
            }
            op @ (TOK_DEC | TOK_INC) => {
                self.next_token()?;
                self.parse_unary(0)?;

                let mut opcode: i32 = 0;
                let mut scope: i32 = 0;
                let mut label: i32 = 0;
                let mut name = Atom::NULL;

                self.get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut name,
                    &mut label,
                    std::ptr::null_mut(),
                    true,
                    op,
                )?;

                self.emit_op(if op == TOK_DEC {
                    OpCode::DEC
                } else {
                    OpCode::INC
                }); //emit_op_u8(s, (OpCode::DEC as i32 + op - TOK_DEC) as u8);

                self.put_lvalue(
                    opcode,
                    scope,
                    name,
                    label,
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP,
                    false,
                );
            }
            TOK_TYPEOF => {
                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                // reference access should not return an exception, so we patch the get_var
                if get_prev_opcode(self.cur_func()) == OpCodeTemp::SCOPE_GET_VAR as u8 {
                    let last_opcode_pos = self.cur_func().last_opcode_pos;
                    self.cur_func_mut().byte_code[last_opcode_pos as usize] =
                        OpCodeTemp::SCOPE_GET_VAR_UNDEF as u8;
                }
                self.emit_op(OpCode::TYPEOF);
                parse_flags = 0;
            }
            TOK_DELETE => {
                self.parse_delete()?;
                parse_flags = 0;
            }
            TOK_AWAIT => {
                if !self.cur_func().attrs.func_kind.is_async() {
                    self.throw_syntax_error("unexpected 'await' keyword");
                    return Err(-1);
                }
                if !self.cur_func().in_function_body {
                    self.throw_syntax_error("await in default expression");
                    return Err(-1);
                }

                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                self.cur_func_mut().has_await = true;
                self.emit_op(OpCode::AWAIT);
                parse_flags = 0;
            }
            _ => {
                self.parse_postfix_expr((parse_flags & PF_ARROW_FUNC) | PF_POSTFIX_CALL)?;

                if !self.got_lf && (self.token.val == TOK_DEC || self.token.val == TOK_INC) {
                    let mut opcode: i32 = 0;
                    let mut scope: i32 = 0;
                    let mut label: i32 = 0;
                    let mut name = Atom::NULL;
                    let tok = self.token.val;
                    self.get_lvalue(
                        &mut opcode,
                        &mut scope,
                        &mut name,
                        &mut label,
                        std::ptr::null_mut(),
                        true,
                        tok,
                    )?;

                    self.emit_op(if self.token.val == TOK_DEC {
                        OpCode::POST_DEC
                    } else {
                        OpCode::POST_INC
                    }); //  emit_op_u8(s, (OpCode::POST_DEC as i32 + op - TOK_DEC) as u8);

                    self.put_lvalue(
                        opcode,
                        scope,
                        name,
                        label,
                        PutLValueEnum::PUT_LVALUE_KEEP_SECOND,
                        false,
                    );

                    self.next_token()?;
                }
            }
        }

        if parse_flags & (PF_POW_ALLOWED | PF_POW_FORBIDDEN) != 0 && self.token.val == TOK_POW {
            /* Strict ES7 exponentiation syntax rules: To solve
            conficting semantics between different implementations
            regarding the precedence of prefix operators and the
            postfix exponential, ES7 specifies that -2**2 is a
            syntax error. */
            if parse_flags & PF_POW_FORBIDDEN != 0 {
                self.ctx.throw_syntax_error(
                    "unparenthesized unary expression can't appear on the left-hand side of '**'",
                );
                return Err(-1);
            }

            self.next_token()?;
            self.parse_unary(PF_POW_ALLOWED)?;
            self.emit_op(OpCode::POW);
        }

        Ok(())
    }

    pub fn parse_delete(&mut self) -> Result<(), i32> {
        let fd0 = self.cur_func_def;

        self.next_token()?;
        self.parse_unary(PF_POW_FORBIDDEN)?;

        debug_assert_eq!(
            fd0, self.cur_func_def,
            "cur_func_def must not change after parse"
        );

        let fd = unsafe { fd0.as_mut().unwrap() };

        match get_prev_opcode(fd) {
            opcode @ (opcodes::OP_get_field | opcodes::op_get_field_opt_chain) => {
                let opt_chain_label: i32 = if opcode == opcodes::op_get_field_opt_chain {
                    unsafe {
                        read_from_ptr::<i32>(
                            fd.byte_code
                                .as_ptr()
                                .offset(fd.last_opcode_pos as isize + 1 + 4 + 1),
                        )
                    }
                } else {
                    -1
                };

                let name = Atom::from_id(unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 1),
                    )
                });

                debug_assert!(fd.last_opcode_pos > 0);
                fd.byte_code.truncate(fd.last_opcode_pos as _);

                self.emit_push_const(name.force_string(&self.ctx)?.into(), true)?;
                self.emit_op(OpCode::DELETE);

                if opt_chain_label >= 0 {
                    let next_label = self.emit_goto(OpCode::GOTO, -1);
                    self.emit_label(opt_chain_label);
                    // if the optional chain is not taken, return 'true'
                    self.emit_op(OpCode::DROP);
                    self.emit_op(OpCode::PUSH_TRUE);
                    self.emit_label(next_label);
                }

                fd.last_opcode_pos = -1;
            }
            opcodes::OP_get_array_el => {
                //(*fd).byte_code.size = (*fd).last_opcode_pos;

                // equivelent of  (*fd).byte_code.size = (*fd).last_opcode_pos;
                debug_assert!(fd.last_opcode_pos >= 0);
                fd.byte_code.truncate(fd.last_opcode_pos as _);

                fd.last_opcode_pos = -1;
                self.emit_op(OpCode::DELETE);
            }
            opcodes::op_get_array_el_opt_chain => {
                let opt_chain_label = unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 1 + 1),
                    )
                };

                fd.byte_code.truncate(fd.last_opcode_pos as _);

                self.emit_op(OpCode::DELETE);
                let next_label = self.emit_goto(OpCode::GOTO, -1);
                self.emit_label(opt_chain_label as _);
                // if the optional chain is not taken, return 'true'
                self.emit_op(OpCode::DROP);
                self.emit_op(OpCode::PUSH_TRUE);
                self.emit_label(next_label);
                fd.last_opcode_pos = -1;
            }
            opcodes::op_scope_get_var => {
                // 'delete this': this is not a reference
                let name = Atom::from_id(unsafe {
                    read_from_ptr::<u32>(
                        fd.byte_code
                            .as_ptr()
                            .offset(fd.last_opcode_pos as isize + 1),
                    )
                });
                if matches!(name, Atom::CONST_this | Atom::CONST_new_target) {
                    self.emit_op(OpCode::DROP);
                    self.emit_op(OpCode::PUSH_TRUE);
                    return Ok(());
                }
                if fd.attrs.exec_mode.contains(ExecMode::STRICT) {
                    self.throw_syntax_error("cannot delete a direct reference in strict mode");
                    return Err(-1);
                } else {
                    fd.byte_code[fd.last_opcode_pos as usize] = OpCodeTemp::SCOPE_DELETE_VAR as u8;
                }
            }
            opcodes::op_scope_get_private_field => {
                self.throw_syntax_error("cannot delete a private class field");
                return Err(-1);
            }
            opcodes::OP_get_super_value => {
                fd.byte_code.truncate(fd.last_opcode_pos as _);
                fd.last_opcode_pos = -1;
                self.emit_op(OpCode::THROW_ERROR);
                self.emit_atom(Atom::NULL);
                self.emit_u8(JS_THROW_ERROR_DELETE_SUPER);
            }
            _ => {
                // ret_true:
                self.emit_op(OpCode::DROP);
                self.emit_op(OpCode::PUSH_TRUE);
            }
        }

        Ok(())
    }
}
