// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, opcodes},
    error::JS_THROW_ERROR_DELETE_SUPER,
    stack_frame::ExecMode,
    utils::read_from_ptr,
};

use super::{
    expr_lvalue::PutLValueEnum,
    state::{
        JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POSTFIX_CALL, PF_POW_ALLOWED,
        PF_POW_FORBIDDEN,
    },
    tokenize::TokenKind,
};

impl<'s> JSParseState<'s> {
    /// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    /// Binary expression parser using precedence climbing (a single loop
    /// instead of an 8-deep precedence-ladder recursion).
    ///
    /// `level` is the minimum operator *tier* (1 = tightest `* / %`,
    /// 8 = loosest `|`) that this call is allowed to consume. The left operand
    /// is parsed directly via `parse_unary`; binary operators at tier `<= level`
    /// are consumed in the loop. Left-associative operators parse their right
    /// operand at one tighter tier (`op_level - 1`), which excludes the
    /// same-tier operator and keeps native-stack depth proportional to
    /// parenthesis nesting rather than operator count — equivalent to what
    /// QuickJS gets from TCO on its precedence ladder, so a 256k C stack now
    /// covers a much deeper parse tree.
    ///
    /// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
    pub(super) fn parse_expr_binary(&mut self, level: i32, parse_flags: i32) -> Result<(), i32> {
        // #x in obj  — private-name membership test. Only valid at the
        // relational tier (level >= 4) and when `in` is allowed.
        if level >= 4
            && parse_flags & PF_IN_ACCEPTED != 0
            && self.token.kind == TokenKind::PRIVATE_NAME
            && self.peek_token(false) == TokenKind::IN
        {
            let atom = self.token.atom().unwrap();

            self.next_token()?;
            if self.token.kind != TokenKind::IN {
                self.throw_syntax_error("expect 'in'");
                return Err(-1);
            }

            self.next_token()?;
            // right operand: shift and tighter (tier < 4)
            self.parse_expr_binary(4 - 1, parse_flags & !PF_ARROW_FUNC)?;

            self.emit_op_temp(OpCodeTemp::SCOPE_IN_PRIVATE_FIELD);
            self.emit_atom(atom);
            self.emit_u16(self.cur_func().scope_level as _);

            return Ok(());
        }

        // Left operand: a single operand. Binary operators are handled below.
        self.parse_unary((parse_flags & PF_ARROW_FUNC) | PF_POW_ALLOWED)?;

        loop {
            let info = match self.token.kind {
                TokenKind::Char('*') => Some((1, OpCode::MUL)),
                TokenKind::Char('/') => Some((1, OpCode::DIV)),
                TokenKind::Char('%') => Some((1, OpCode::MOD)),
                TokenKind::Char('+') => Some((2, OpCode::ADD)),
                TokenKind::Char('-') => Some((2, OpCode::SUB)),
                TokenKind::SHL => Some((3, OpCode::SHL)),
                TokenKind::SAR => Some((3, OpCode::SAR)),
                TokenKind::SHR => Some((3, OpCode::SHR)),
                TokenKind::Char('<') => Some((4, OpCode::LT)),
                TokenKind::Char('>') => Some((4, OpCode::GT)),
                TokenKind::LTE => Some((4, OpCode::LTE)),
                TokenKind::GTE => Some((4, OpCode::GTE)),
                TokenKind::INSTANCEOF => Some((4, OpCode::INSTANCEOF)),
                TokenKind::IN if parse_flags & PF_IN_ACCEPTED != 0 => Some((4, OpCode::IN)),
                TokenKind::EQ => Some((5, OpCode::EQ)),
                TokenKind::NEQ => Some((5, OpCode::NEQ)),
                TokenKind::STRICT_EQ => Some((5, OpCode::STRICT_EQ)),
                TokenKind::STRICT_NEQ => Some((5, OpCode::STRICT_NEQ)),
                TokenKind::Char('&') => Some((6, OpCode::AND)),
                TokenKind::Char('^') => Some((7, OpCode::XOR)),
                TokenKind::Char('|') => Some((8, OpCode::OR)),
                _ => None,
            };

            let (op_level, opcode) = match info {
                Some(v) => v,
                None => break,
            };
            // Operators looser than the current tier are handled by the caller.
            if op_level > level {
                break;
            }

            self.next_token()?;

            // Right operand at one tighter tier => left-associative.
            self.parse_expr_binary(op_level - 1, parse_flags & !PF_ARROW_FUNC)?;

            self.emit_op(opcode);
        }

        Ok(())
    }

    // allowed parse_flags: PF_ARROW_FUNC, PF_POW_ALLOWED, PF_POW_FORBIDDEN
    pub(super) fn parse_unary(&mut self, mut parse_flags: i32) -> Result<(), i32> {
        match self.token.kind {
            op @ (TokenKind::Char('+' | '-' | '!' | '~') | TokenKind::VOID) => {
                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;
                match op {
                    TokenKind::Char('-') => self.emit_op(OpCode::NEG),
                    TokenKind::Char('+') => self.emit_op(OpCode::PLUS),
                    TokenKind::Char('!') => self.emit_op(OpCode::LNOT),
                    TokenKind::Char('~') => self.emit_op(OpCode::NOT),
                    TokenKind::VOID => {
                        self.emit_op(OpCode::DROP);
                        self.emit_op(OpCode::UNDEFINED);
                    }
                    _ => unreachable!(),
                }
                parse_flags = 0;
            }
            op @ (TokenKind::DEC | TokenKind::INC) => {
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

                self.emit_op(if op == TokenKind::DEC {
                    OpCode::DEC
                } else {
                    OpCode::INC
                });

                self.put_lvalue(
                    opcode,
                    scope,
                    name,
                    label,
                    PutLValueEnum::PUT_LVALUE_KEEP_TOP,
                    false,
                );
            }
            TokenKind::TYPEOF => {
                self.next_token()?;
                self.parse_unary(PF_POW_FORBIDDEN)?;

                // reference access should not return an exception, so we patch the get_var
                if self.cur_func().get_prev_opcode() == OpCodeTemp::SCOPE_GET_VAR as u8 {
                    let last_opcode_pos = self.cur_func().last_opcode_pos;
                    self.cur_func_mut().byte_code[last_opcode_pos as usize] =
                        OpCodeTemp::SCOPE_GET_VAR_UNDEF as u8;
                }
                self.emit_op(OpCode::TYPEOF);
                parse_flags = 0;
            }
            TokenKind::DELETE => {
                self.parse_delete()?;
                parse_flags = 0;
            }
            TokenKind::AWAIT => {
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

                if !self.got_lf && matches!(self.token.kind, TokenKind::DEC | TokenKind::INC) {
                    let mut opcode: i32 = 0;
                    let mut scope: i32 = 0;
                    let mut label: i32 = 0;
                    let mut name = Atom::NULL;
                    let tok = self.token.kind;
                    self.get_lvalue(
                        &mut opcode,
                        &mut scope,
                        &mut name,
                        &mut label,
                        std::ptr::null_mut(),
                        true,
                        tok,
                    )?;

                    self.emit_op(if self.token.kind == TokenKind::DEC {
                        OpCode::POST_DEC
                    } else {
                        OpCode::POST_INC
                    });

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

        if parse_flags & (PF_POW_ALLOWED | PF_POW_FORBIDDEN) != 0
            && self.token.kind == TokenKind::POW
        {
            /* Strict ES7 exponentiation syntax rules: To solve
            conficting semantics between different implementations
            regarding the precedence of prefix operators and the
            postfix exponential, ES7 specifies that -2**2 is a
            syntax error. */
            if parse_flags & PF_POW_FORBIDDEN != 0 {
                self.ctx().throw_syntax_error(
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

        match fd.get_prev_opcode() {
            opcode @ (opcodes::OP_get_field
            | opcodes::OP_get_field_ic
            | opcodes::op_get_field_opt_chain) => {
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

                let name_str = name.force_string(self.ctx())?;
                self.emit_push_const(name_str.into(), true)?;
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
