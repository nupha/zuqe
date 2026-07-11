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
        // name0 captures the left-most primary token (used by the assignment
        // OP_set_name optimisation in parse_higher_expr).
        let name0 = if self.token.kind == TokenKind::IDENT {
            self.token.atom().unwrap()
        } else {
            Atom::NULL
        };

        // `yield` must be recognised as the very first token of any expression,
        // otherwise a top-level `yield` statement (e.g. inside a generator)
        // reaches parse_postfix_expr's catch-all and throws "unexpected token
        // in expression: 'yield'".  The original code checked this in
        // `parse_assign_expr2`, which every expression entry routed through;
        // now that `parse_expr` / `parse_expr2` call `parse_expr_binary`
        // directly, we must check it here so the check still happens at every
        // operand position.
        //
        // We parse it and *fall through* (instead of returning) so that further
        // operators at the same expression position are still handled — e.g.
        // `yield, yield` (comma), `yield ? a : b` (ternary) or `yield && x`
        // (logic).  Returning early would leave those trailing operators
        // unconsumed and produce a spurious syntax error.
        if self.token.kind == TokenKind::YIELD {
            self.parse_yield(parse_flags)?;
        } else {
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

            // Left operand.  For the common (non-prefix) case we parse the
            // postfix expression directly, skipping the parse_unary frame so a
            // deeply parenthesised `((((1))))` pays only TWO native frames per
            // parenthesis level (parse_expr_binary -> parse_postfix_expr ->
            // parse_expr_binary) instead of three.  Prefix unary operators
            // (which do not occur on the hot `((1))` path) still go through
            // parse_unary to preserve their exact semantics, including the ES7
            // `**` restrictions.
            match self.token.kind {
                TokenKind::Char('+' | '-' | '!' | '~')
                | TokenKind::VOID
                | TokenKind::TYPEOF
                | TokenKind::DELETE
                | TokenKind::AWAIT
                | TokenKind::DEC
                | TokenKind::INC => {
                    self.parse_unary((parse_flags & PF_ARROW_FUNC) | PF_POW_ALLOWED)?;
                }
                _ => {
                    self.parse_postfix_expr(
                        (parse_flags & PF_ARROW_FUNC) | PF_POSTFIX_CALL | PF_POW_ALLOWED,
                    )?;
                }
            }
        }

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

        // Higher-precedence-than-binary operators (logic, ternary, assignment)
        // are resolved here in a single non-recursive step so that a deeply
        // parenthesised expression such as (((1))) costs only a constant number
        // of native frames per parenthesis level, instead of one frame per
        // wrapper in the old parse_assign_expr2 -> parse_cond_expr ->
        // parse_coalesce_expr -> parse_logical_and_or chain.
        if level >= 9 {
            // Only invoke parse_higher_expr when the next token actually
            // starts one of the looser operators (logic/ternary/assignment).
            // A pure parenthesised chain such as (((1))) never hits these, so
            // it pays no frame for the (empty) higher step — keeping native
            // stack depth proportional to parenthesis nesting.
            let tok = self.token.kind;
            let higher_start = matches!(
                tok,
                TokenKind::LOR
                    | TokenKind::LAND
                    | TokenKind::DOUBLE_QUESTION_MARK
                    | TokenKind::Char('?')
                    | TokenKind::Char('=')
                    | TokenKind::LAND_ASSIGN
                    | TokenKind::LOR_ASSIGN
                    | TokenKind::DOUBLE_QUESTION_MARK_ASSIGN
            ) || tok.is_op_assign();
            if higher_start {
                self.parse_higher_expr(parse_flags, level, name0)?;
            }
        }

        // Comma operator (loosest, level 14).
        if level >= 14 {
            while self.token.kind == TokenKind::Char(',') {
                self.next_token()?;
                self.emit_op(OpCode::DROP);
                self.parse_expr_binary(14, parse_flags)?;
                // Forget the trailing opcode of this operand.  This mirrors the
                // old `parse_expr2` comma loop, which reset `last_opcode_pos`
                // *after* each operand.  Without it, a trailing anonymous
                // function such as the right side of `(0, function() {})`
                // leaves its dummy `SET_NAME` opcode as the last opcode, and an
                // outer `set_object_name` (e.g. `a = (0, function() {})`)
                // would wrongly name the function `a`.
                self.cur_func_mut().last_opcode_pos = -1;
            }
        }

        Ok(())
    }

    /// Handles everything looser than the binary operators (levels 9..=13):
    /// `&&`/`||`/`??` (logic/nullish), `?:` (ternary) and `=`/`op=`/logical
    /// assignment. The left operand is assumed to have already been emitted by
    /// the caller (`parse_expr_binary`). Collapsing the old
    /// `parse_cond_expr -> parse_coalesce_expr -> parse_logical_and_or ->
    /// parse_assign_expr2` chain into this single step keeps native-stack depth
    /// proportional to parenthesis nesting instead of the old per-wrapper
    /// function frames.
    #[inline(never)]
    fn parse_higher_expr(&mut self, parse_flags: i32, level: i32, name0: Atom) -> Result<(), i32> {
        // ===== logic: && || ?? =====
        // `&&` is tighter than `||`; both are tighter than `??`. Process them
        // in order (&&, then ||, then ??) so a left operand such as `a && b`
        // is fully consumed before `||`/`??` are considered, mirroring the old
        // parse_logical_and_or(LOR) -> (LAND) nesting.
        if self.token.kind == TokenKind::LAND {
            let label_and = self.cur_func_mut().add_label()? as i32;
            loop {
                self.next_token()?;
                self.emit_op(OpCode::DUP);
                self.emit_goto(OpCode::IF_FALSE, label_and);
                self.emit_op(OpCode::DROP);
                self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;
                if self.token.kind != TokenKind::LAND {
                    break;
                }
            }
            self.emit_label(label_and);
        }

        if self.token.kind == TokenKind::LOR {
            let label1 = self.cur_func_mut().add_label()? as i32;
            loop {
                self.next_token()?;
                self.emit_op(OpCode::DUP);
                self.emit_goto(OpCode::IF_TRUE, label1);
                self.emit_op(OpCode::DROP);
                // right operand: a binary expression, optionally followed by a
                // `&&` chain (an `||` operand may itself be `b && c`).
                self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;
                if self.token.kind == TokenKind::LAND {
                    let label_and = self.cur_func_mut().add_label()? as i32;
                    loop {
                        self.next_token()?;
                        self.emit_op(OpCode::DUP);
                        self.emit_goto(OpCode::IF_FALSE, label_and);
                        self.emit_op(OpCode::DROP);
                        self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;
                        if self.token.kind != TokenKind::LAND {
                            break;
                        }
                    }
                    self.emit_label(label_and);
                }
                if self.token.kind != TokenKind::LOR {
                    if self.token.kind == TokenKind::DOUBLE_QUESTION_MARK {
                        self.throw_syntax_error("cannot mix ?? with && or ||");
                        return Err(-1);
                    }
                    break;
                }
            }
            self.emit_label(label1);
        }

        if self.token.kind == TokenKind::DOUBLE_QUESTION_MARK {
            let label1 = self.cur_func_mut().add_label()? as i32;
            loop {
                self.next_token()?;
                self.emit_op(OpCode::DUP);
                self.emit_op(OpCode::IS_UNDEFINED_OR_NULL);
                self.emit_goto(OpCode::IF_FALSE, label1);
                self.emit_op(OpCode::DROP);
                self.parse_expr_binary(8, parse_flags & !PF_ARROW_FUNC)?;
                if self.token.kind != TokenKind::DOUBLE_QUESTION_MARK {
                    break;
                }
            }
            self.emit_label(label1);
        }

        // ===== ternary ?: (level >= 12) =====
        if level >= 12 && self.token.kind == TokenKind::Char('?') {
            self.next_token()?;
            let label1 = self.emit_goto(OpCode::IF_FALSE, -1);
            self.parse_assign_expr()?;
            self.parse_expect(TokenKind::Char(':'))?;
            let label2 = self.emit_goto(OpCode::GOTO, -1);
            self.emit_label(label1);
            // Use parse_assign_expr() (not parse_assign_expr2 with a masked
            // flag) so the `:` branch keeps PF_ARROW_FUNC and still accepts an
            // arrow function, e.g. `cond ? () => 1 : () => 2`.
            self.parse_assign_expr()?;
            self.emit_label(label2);
        }

        // ===== assignment = / op= / &&= ||= ??= (level >= 13) =====
        if level >= 13 {
            match self.token.kind {
                op if op == TokenKind::Char('=') || op.is_op_assign() => {
                    self.next_token()?;
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
                        op != TokenKind::Char('='),
                        op,
                    )?;
                    self.parse_assign_expr2(parse_flags)?;
                    if op == TokenKind::Char('=') {
                        if opcode == OpCode::GET_REF_VALUE as i32 && name == name0 {
                            self.set_object_name(name);
                        }
                    } else {
                        self.emit_op(op.op_assign_opcode().unwrap());
                    }
                    self.put_lvalue(
                        opcode,
                        scope,
                        name,
                        label,
                        PutLValueEnum::PUT_LVALUE_KEEP_TOP,
                        false,
                    );
                }
                op @ (TokenKind::LAND_ASSIGN
                | TokenKind::LOR_ASSIGN
                | TokenKind::DOUBLE_QUESTION_MARK_ASSIGN) => {
                    self.next_token()?;
                    let mut opcode: i32 = 0;
                    let mut scope: i32 = 0;
                    let mut label: i32 = 0;
                    let mut name = Atom::NULL;
                    let mut depth_lvalue: i32 = 0;
                    self.get_lvalue(
                        &mut opcode,
                        &mut scope,
                        &mut name,
                        &mut label,
                        &mut depth_lvalue,
                        true,
                        op,
                    )?;
                    self.emit_op(OpCode::DUP);
                    if op == TokenKind::DOUBLE_QUESTION_MARK_ASSIGN {
                        self.emit_op(OpCode::IS_UNDEFINED_OR_NULL);
                    }
                    let label1 = self.emit_goto(
                        if op == TokenKind::LOR_ASSIGN {
                            OpCode::IF_TRUE
                        } else {
                            // LAND_ASSIGN short-circuits when the left value is
                            // falsy; `??=` (after IS_UNDEFINED_OR_NULL) short-circuits
                            // when the value is *not* null/undefined.  Both skip on
                            // IF_FALSE.
                            OpCode::IF_FALSE
                        },
                        -1,
                    );
                    self.emit_op(OpCode::DROP);
                    self.parse_assign_expr2(parse_flags)?;
                    if opcode == OpCode::GET_REF_VALUE as i32 && name == name0 {
                        self.set_object_name(name);
                    }
                    match depth_lvalue {
                        1 => self.emit_op(OpCode::INSERT2),
                        2 => self.emit_op(OpCode::INSERT3),
                        3 => self.emit_op(OpCode::INSERT4),
                        _ => unreachable!(),
                    }
                    self.put_lvalue(
                        opcode,
                        scope,
                        name,
                        label,
                        PutLValueEnum::PUT_LVALUE_NOKEEP_DEPTH,
                        false,
                    );
                    let label2 = self.emit_goto(OpCode::GOTO, -1);
                    self.emit_label(label1);
                    while depth_lvalue != 0 {
                        self.emit_op(OpCode::NIP);
                        depth_lvalue -= 1;
                    }
                    self.emit_label(label2);
                }
                _ => {}
            }
        }

        Ok(())
    }

    // allowed parse_flags: PF_ARROW_FUNC, PF_POW_ALLOWED, PF_POW_FORBIDDEN
    pub(super) fn parse_unary(&mut self, parse_flags: i32) -> Result<(), i32> {
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
            }
            TokenKind::DELETE => {
                self.parse_delete()?;
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
            }
            _ => {
                // The postfix ++/-- and `**` handling now lives in
                // parse_postfix_expr, so just delegate to it here (carrying the
                // POW flags so exponentiation is parsed in the right place).
                self.parse_postfix_expr(
                    (parse_flags & (PF_ARROW_FUNC | PF_POW_ALLOWED | PF_POW_FORBIDDEN))
                        | PF_POSTFIX_CALL,
                )?;
            }
        }

        // Exponentiation (`**`) is handled *here* (at the end of parse_unary),
        // not only in parse_postfix_expr, so that it also applies after a prefix
        // update expression such as `--base ** 2` (which must parse as
        // `(--base) ** 2`).  The ES7 rule forbids an unparenthesised unary
        // expression on the left of `**` (e.g. `-x ** 2`); that is enforced by
        // passing PF_POW_FORBIDDEN into the operand of the unary prefix
        // operators above, so the POW check below throws for them.
        if parse_flags & (PF_POW_ALLOWED | PF_POW_FORBIDDEN) != 0
            && self.token.kind == TokenKind::POW
        {
            if parse_flags & PF_POW_FORBIDDEN != 0 {
                self.throw_syntax_error(
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
