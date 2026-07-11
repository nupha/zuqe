// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, SpecialObject, opcodes},
    function::{FuncCallType, FunctionKind},
    number::IntOrFloat,
    utils::unlikely,
    value::JSValue,
};

use super::{
    esm::JSParseExportEnum,
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    state::{
        JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POSTFIX_CALL, PF_POW_ALLOWED,
        PF_POW_FORBIDDEN, SKIP_HAS_ELLIPSIS,
    },
    tokenize::TokenKind,
};

impl<'s> JSParseState<'s> {
    // allowed parse_flags: PF_IN_ACCEPTED
    #[inline(never)]
    pub(super) fn parse_assign_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
        if self.token.kind == TokenKind::YIELD {
            return self.parse_yield(parse_flags);
        }
        // The actual assignment / ternary / logic handling now lives in
        // parse_higher_expr, which parse_expr_binary invokes. Using level 13
        // keeps the comma operator (level 14) out, matching the old behaviour
        // where parse_expr2 was responsible for the comma sequence.
        //
        // `PF_ARROW_FUNC` must be threaded down so that arrow-function
        // expressions are recognised in positions like object-literal property
        // values (`{ f: () => 1 }`) and ternary/assignment right-hand sides
        // (`a ? () => 1 : () => 2`). The original code added this flag here
        // (then `parse_cond_expr(parse_flags | PF_ARROW_FUNC)`); keep it.
        self.parse_expr_binary(13, parse_flags | PF_ARROW_FUNC)
    }

    /// a short-cut to `self.parse_assign_expr2(PF_IN_ACCEPTED)`
    #[inline(always)]
    pub(super) fn parse_assign_expr(&mut self) -> Result<(), i32> {
        self.parse_assign_expr2(PF_IN_ACCEPTED)
    }

    // allowed parse_flags: PF_IN_ACCEPTED
    #[inline(never)]
    pub(super) fn parse_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
        // `PF_ARROW_FUNC` must reach the leaf operand so that a top-level
        // expression like `() => this` (e.g. an eval string) is recognised as
        // an arrow function.  The original code added this flag in
        // `parse_assign_expr2` and every entry point routed through it; now
        // that `parse_expr2` calls `parse_expr_binary` directly we add it here.
        self.parse_expr_binary(14, parse_flags | PF_ARROW_FUNC)
    }

    /// a short-cut to `self.parse_expr_binary(14, PF_IN_ACCEPTED)`
    #[inline(always)]
    pub(super) fn parse_expr(&mut self) -> Result<(), i32> {
        self.parse_expr_binary(14, PF_IN_ACCEPTED | PF_ARROW_FUNC)
    }

    pub(super) fn parse_expr_parens(&mut self) -> Result<(), i32> {
        self.parse_expect(TokenKind::Char('('))?;
        // Use parse_expr() (not the raw level-14 call) so PF_ARROW_FUNC is
        // threaded through, matching the historical behaviour where this call
        // reached parse_assign_expr2 (which always added the flag).
        self.parse_expr()?;
        self.parse_expect(TokenKind::Char(')'))?;
        Ok(())
    }

    /// postfix expr typically refers to how the parser handles postfix expressions such as:
    /// - Postfix increment/decrement operators (a++, a--)
    /// - Member access expressions (a.b)
    /// - Array access expressions (a[b])
    /// - Function calls (a())
    ///
    /// these would typically be handled after parsing the primary expression (the "a" part),
    /// and then checking for postfix operators or access patterns that follow it.
    ///
    /// allowed parse_flags: PF_POSTFIX_CALL, PF_ARROW_FUNC
    #[inline(never)]
    pub(super) fn parse_postfix_expr(&mut self, parse_flags: i32) -> Result<(), i32> {
        let mut accept_lparen: bool = (parse_flags & PF_POSTFIX_CALL) != 0;
        let mut call_type = FuncCallType::FUNC_CALL_NORMAL;

        match self.token.kind {
            TokenKind::NUMBER => {
                match self.token.data.number().unwrap() {
                    IntOrFloat::Int64(i) if (i32::MIN as i64..=i32::MAX as i64).contains(&i) => {
                        self.emit_op(OpCode::PUSH_I32);
                        self.emit_u32(i as u32);
                    }
                    n => {
                        self.emit_push_const(JSValue::from(n), false)?;
                    }
                }
                self.next_token()?;
            }
            TokenKind::TEMPLATE => {
                let mut _x = 0;
                self.parse_template(false, &mut _x)?;
            }
            TokenKind::STRING => {
                let sbuf = self.token.data.take_string().unwrap();
                let str = self.ctx().create_string_with_buf(sbuf)?;
                self.emit_push_const(str.into(), true)?;
                self.next_token()?;
            }

            TokenKind::DIV_ASSIGN | TokenKind::Char('/') => {
                #[cfg(feature = "builtin_regexp")]
                {
                    self.buf_ptr = unsafe {
                        self.buf_ptr
                            .offset(if self.token.kind == TokenKind::DIV_ASSIGN {
                                -2
                            } else {
                                -1
                            })
                    };

                    // the previous token is '/' or '/=', so no need to free
                    self.parse_regexp()?;

                    // emit RegExp opcode
                    let re_id = self.token.data.regexp().unwrap();
                    let empty_str = Atom::CONST_empty_string.force_string(self.ctx()).unwrap();
                    self.emit_push_const(empty_str.into(), false)?;
                    self.emit_push_const(JSValue::Int(re_id as i32), false)?;
                    // we use a specific opcode to be sure the correct
                    // function is called (otherwise the bytecode would have
                    // to be verified by the RegExp constructor)
                    self.emit_op(OpCode::REGEXP);

                    self.next_token()?;
                }

                #[cfg(not(feature = "builtin_regexp"))]
                {
                    self.throw_syntax_error("regex not supported");
                    return Err(-1);
                }
            }

            TokenKind::Char('(') => {
                let mut _p = 0;
                if parse_flags & PF_ARROW_FUNC != 0
                    && self.parse_skip_parens_token(&mut _p, true)? == TokenKind::ARROW
                {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                        FunctionKind::Normal,
                        self.token.ptr,
                        self.token.line_num,
                    )?;
                } else {
                    // Inline parse_expr_parens here so a parenthesised
                    // expression does not pay an extra native stack frame per
                    // parenthesis level.  The irreducible cycle for a deeply
                    // parenthesised `((((1))))` is then
                    //   parse_postfix_expr -> parse_expr_binary ->
                    //   parse_unary -> parse_postfix_expr   (3 frames/level)
                    // instead of the old 4-frame
                    //   ... -> parse_expr_parens -> parse_expr_binary -> ...
                    // NB: use parse_expr() (not the raw level-14 call) so the
                    // PF_ARROW_FUNC flag is threaded through — a parenthesised
                    // arrow such as `(() => 5)` must still be recognised.
                    self.parse_expect(TokenKind::Char('('))?;
                    self.parse_expr()?;
                    self.parse_expect(TokenKind::Char(')'))?;
                }
            }
            TokenKind::FUNCTION => {
                self.parse_function_decl(
                    JSParseFunctionEnum::JS_PARSE_FUNC_EXPR,
                    FunctionKind::Normal,
                    self.token.ptr,
                    self.token.line_num,
                )?;
            }
            TokenKind::CLASS => {
                self.parse_class(true, JSParseExportEnum::JS_PARSE_EXPORT_NONE)?;
            }
            TokenKind::NULL => {
                self.next_token()?;
                self.emit_op(OpCode::NULL);
            }
            TokenKind::THIS => {
                self.next_token()?;
                self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                self.emit_atom(Atom::CONST_this);
                self.emit_u16(0);
            }
            TokenKind::FALSE => {
                self.next_token()?;
                self.emit_op(OpCode::PUSH_FALSE);
            }
            TokenKind::TRUE => {
                self.next_token()?;
                self.emit_op(OpCode::PUSH_TRUE);
            }
            TokenKind::IDENT => {
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }
                if (parse_flags & PF_ARROW_FUNC) != 0 && self.peek_token(true) == TokenKind::ARROW {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                        FunctionKind::Normal,
                        self.token.ptr,
                        self.token.line_num,
                    )?;
                } else if self.token_is_pseudo_keyword(Atom::CONST_async)
                    && self.peek_token(true) != TokenKind::Char('\n')
                {
                    let source_ptr = self.token.ptr;
                    let source_line_num = self.token.line_num;

                    self.next_token()?;

                    if self.token.kind == TokenKind::FUNCTION {
                        self.parse_function_decl(
                            JSParseFunctionEnum::JS_PARSE_FUNC_EXPR,
                            FunctionKind::Async,
                            source_ptr,
                            source_line_num,
                        )?;
                    } else if (parse_flags & PF_ARROW_FUNC) != 0
                        && (self.token.kind == TokenKind::Char('(') && {
                            let mut _p = 0;
                            self.parse_skip_parens_token(&mut _p, true)? == TokenKind::ARROW
                        })
                        || (self.token.kind == TokenKind::IDENT
                            && !self.token.data.is_reserved()
                            && self.peek_token(true) == TokenKind::ARROW)
                    {
                        self.parse_function_decl(
                            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                            FunctionKind::Async,
                            source_ptr,
                            source_line_num,
                        )?;
                    } else {
                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom(Atom::CONST_async);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }
                } else {
                    // get atom rc, because later next_token() call will free the atom.
                    let name = self.token.atom().unwrap();

                    if unlikely(
                        name == Atom::CONST_arguments && !self.cur_func().attrs.arguments_allowed,
                    ) {
                        self.throw_syntax_error(
                            "'arguments' identifier is not allowed in class field initializer",
                        );
                        return Err(-1);
                    } else {
                        self.next_token()?;

                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom(name);
                        self.emit_u16(self.cur_func().scope_level as _);
                    }
                }
            }
            TokenKind::Char('{' | '[') => {
                let mut skip_bits: i32 = 0;
                if self.parse_skip_parens_token(&mut skip_bits, false)? == TokenKind::Char('=') {
                    self.parse_destructuring_element(
                        TokenKind::Char('\0'),
                        false,
                        false,
                        (skip_bits & SKIP_HAS_ELLIPSIS != 0) as i32,
                        true,
                        false,
                    )?;
                } else if self.token.kind == TokenKind::Char('{') {
                    self.parse_object_literal()?;
                } else {
                    self.js_parse_array_literal()?;
                }
            }
            TokenKind::NEW => {
                self.next_token()?;

                if self.token.kind == TokenKind::Char('.') {
                    self.next_token()?;

                    if !self.token_is_pseudo_keyword(Atom::CONST_target) {
                        self.throw_syntax_error("expecting target");
                        return Err(-1);
                    }
                    if !self.cur_func().attrs.new_target_allowed {
                        self.throw_syntax_error("new.target only allowed within functions");
                        return Err(-1);
                    }

                    self.next_token()?;

                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom(Atom::CONST_new_target);
                    self.emit_u16(0);
                } else {
                    self.parse_postfix_expr(0)?;

                    accept_lparen = true;
                    if self.token.kind != TokenKind::Char('(') {
                        /* new operator on an object */
                        self.emit_op(OpCode::DUP);
                        self.emit_op(OpCode::CALL_CONSTRUCTOR);
                        self.emit_u16(0);
                    } else {
                        call_type = FuncCallType::FUNC_CALL_NEW;
                    }
                }
            }
            TokenKind::SUPER => {
                self.next_token()?;
                if self.token.kind == TokenKind::Char('(') {
                    if !self.cur_func().attrs.super_call_allowed {
                        self.throw_syntax_error(
                            "super() is only valid in a derived class constructor",
                        );
                        return Err(-1);
                    }
                    call_type = FuncCallType::FUNC_CALL_SUPER_CTOR;
                } else if matches!(self.token.kind, TokenKind::Char('.' | '[')) {
                    if !self.cur_func().attrs.super_allowed {
                        self.throw_syntax_error("'super' is only valid in a method");
                        return Err(-1);
                    }

                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom(Atom::CONST_this);
                    self.emit_u16(0);
                    self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                    self.emit_atom(Atom::CONST__home_object_);
                    self.emit_u16(0);
                    self.emit_op(OpCode::GET_SUPER);
                } else {
                    self.throw_syntax_error("invalid use of 'super'");
                    return Err(-1);
                }
            }
            TokenKind::IMPORT => {
                self.next_token()?;
                if self.token.kind == TokenKind::Char('.') {
                    self.next_token()?;

                    if !self.token_is_pseudo_keyword(Atom::CONST_meta) {
                        self.throw_syntax_error("meta expected");
                        return Err(-1);
                    }
                    if !self.is_module {
                        self.throw_syntax_error("import.meta only valid in module code");
                        return Err(-1);
                    }
                    self.next_token()?;

                    self.emit_op(OpCode::SPECIAL_OBJECT);
                    self.emit_u8(SpecialObject::ImportMeta as u8);
                } else {
                    self.parse_expect(TokenKind::Char('('))?;
                    if !accept_lparen {
                        self.throw_syntax_error("invalid use of 'import()'");
                        return Err(-1);
                    }
                    self.parse_assign_expr()?;

                    // optional second argument: import(specifier, options)
                    if self.token.kind == TokenKind::Char(',') {
                        self.next_token()?;
                        if self.token.kind != TokenKind::Char(')') {
                            self.parse_assign_expr()?;
                            // accept trailing comma
                            if self.token.kind == TokenKind::Char(',') {
                                self.next_token()?;
                            }
                        } else {
                            // import(specifier,) — no options, push undefined
                            self.emit_op(OpCode::UNDEFINED);
                        }
                    } else {
                        // import(specifier) — no options, push undefined
                        self.emit_op(OpCode::UNDEFINED);
                    }
                    self.parse_expect(TokenKind::Char(')'))?;

                    self.emit_op(OpCode::IMPORT);
                }
            }
            _ => unsafe {
                let len = self.buf_ptr.offset_from(self.token.ptr) as usize;
                let msg = format!(
                    "unexpected token in expression: '{}'",
                    String::from_utf8_lossy(std::slice::from_raw_parts(self.token.ptr, len))
                        .as_ref(),
                );
                self.throw_syntax_error(&msg);
                return Err(-1);
            },
        }

        let mut optional_chaining_label = -1;
        loop {
            let mut has_optional_chain: bool = false;

            if self.token.kind == TokenKind::QUESTION_MARK_DOT {
                // optional chaining
                self.next_token()?;

                has_optional_chain = true;

                if self.token.kind == TokenKind::Char('(') && accept_lparen {
                    self.parse_func_call(
                        call_type,
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )?;
                    call_type = FuncCallType::FUNC_CALL_NORMAL;
                } else if self.token.kind == TokenKind::Char('[') {
                    self.parse_array_access(
                        unsafe { self.cur_func_def.as_ref().unwrap() },
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )?;
                } else {
                    self.parse_property(
                        unsafe { self.cur_func_def.as_ref().unwrap() },
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )?;
                }
            } else if self.token.kind == TokenKind::TEMPLATE
                && call_type == FuncCallType::FUNC_CALL_NORMAL
            {
                if optional_chaining_label >= 0 {
                    self.throw_syntax_error("template literal cannot appear in an optional chain");
                    return Err(-1);
                }
                let opcode = unsafe {
                    self.parse_func_call2(
                        FuncCallType::FUNC_CALL_TEMPLATE,
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )
                };

                let mut argc = 0;
                self.parse_template(true, &mut argc)?;
                self.emit_func_call(
                    unsafe { self.cur_func_def.as_mut().unwrap() },
                    FuncCallType::FUNC_CALL_TEMPLATE,
                    opcode,
                    argc as _,
                );
            } else if self.token.kind == TokenKind::Char('(') && accept_lparen {
                // function call
                self.parse_func_call(call_type, has_optional_chain, &mut optional_chaining_label)?;
                call_type = FuncCallType::FUNC_CALL_NORMAL;
            } else if self.token.kind == TokenKind::Char('.') {
                self.next_token()?;
                self.parse_property(
                    unsafe { self.cur_func_def.as_ref().unwrap() },
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
            } else if self.token.kind == TokenKind::Char('[') {
                self.parse_array_access(
                    unsafe { self.cur_func_def.as_ref().unwrap() },
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
            } else {
                break;
            }
        } // loop

        if optional_chaining_label >= 0 {
            // When inline_cache is on, GET_FIELD_IC was emitted instead of
            // GET_FIELD.  It is 7 bytes (opcode + atom + ic_index) but the
            // optional-chain temp opcodes expect the 5-byte format. Patch
            // the instruction down to plain GET_FIELD first, then let the
            // normal get_field → opt_chain conversion handle it.
            //
            // The label emitted by emit_label_raw() comes AFTER the IC
            // instruction in the bytecode buffer, so we remove the 2
            // ic_index bytes from the middle by shifting everything after
            // them forward.
            #[cfg(feature = "inline_cache")]
            if self.cur_func().get_prev_opcode() == opcodes::OP_get_field_ic {
                let fd = self.cur_func_mut();
                // 1) Rewrite opcode from GET_FIELD_IC → GET_FIELD
                fd.byte_code[fd.last_opcode_pos as usize] = opcodes::OP_get_field;
                // 2) Remove the 2 ic_index bytes after atom
                let keep_atom = fd.last_opcode_pos as usize + 5;
                let remove_len = 2;
                fd.byte_code
                    .copy_within(keep_atom + remove_len.., keep_atom);
                fd.byte_code.truncate(fd.byte_code.len() - remove_len);
            }

            self.emit_label_raw(optional_chaining_label);

            match self.cur_func().get_prev_opcode() {
                op @ (opcodes::OP_get_field | opcodes::OP_get_array_el) => {
                    let last_opcode_pos = self.cur_func().last_opcode_pos;
                    self.cur_func_mut().byte_code[last_opcode_pos as usize] =
                        if op == opcodes::OP_get_field {
                            OpCodeTemp::GET_FIELD_OPT_CHAIN
                        } else {
                            OpCodeTemp::GET_ARRAY_EL_OPT_CHAIN
                        } as u8;
                }
                _ => {
                    self.cur_func_mut().last_opcode_pos = -1;
                }
            }
        }

        // Postfix increment/decrement and the `**` (exponentiation) operator
        // used to live in parse_unary; they are handled here so that
        // parse_expr_binary can parse its left operand by calling
        // parse_postfix_expr directly, skipping the parse_unary frame for the
        // common non-prefix case.  That keeps the parenthesised-expression
        // cycle at two native frames per level
        // (parse_expr_binary -> parse_postfix_expr) instead of three.
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

    pub(super) fn optional_chain_test(
        &mut self,
        poptional_chaining_label: &mut i32,
        drop_count: i32,
    ) {
        if *poptional_chaining_label < 0 {
            *poptional_chaining_label = self.cur_func_mut().add_label().map_or(-1, |x| x as i32);
        }

        // XXX: could be more efficient with a specific opcode
        self.emit_op(OpCode::DUP);
        self.emit_op(OpCode::IS_UNDEFINED_OR_NULL);
        let label_next = self.emit_goto(OpCode::IF_FALSE, -1);
        for _ in 0..drop_count {
            self.emit_op(OpCode::DROP);
        }
        self.emit_op(OpCode::UNDEFINED);
        self.emit_goto(OpCode::GOTO, *poptional_chaining_label);
        self.emit_label(label_next);
    }
}
