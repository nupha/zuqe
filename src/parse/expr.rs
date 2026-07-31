// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, SpecialObject, opcode_info_intermediate, opcodes},
    error::{JS_THROW_ERROR_DELETE_SUPER, JS_THROW_ERROR_ITERATOR_THROW},
    function::{FuncCallType, FunctionKind},
    number::IntOrFloat,
    stack_frame::ExecMode,
    utils::{read_from_ptr, unlikely},
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
    // ===== entry points / postfix / primary / optional chaining =====
    // allowed parse_flags: PF_IN_ACCEPTED
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
                self.parse_template(false)?;
            }
            TokenKind::STRING => {
                let sbuf = self.token.data.take_string().unwrap();
                let str = self.ctx().create_string_with_buf(sbuf)?;
                self.emit_push_const(JSValue::make_string(str), true)?;
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
                    self.emit_push_const(JSValue::make_string(empty_str), false)?;
                    self.emit_push_const(JSValue::from(re_id as i32), false)?;
                    // we use a specific opcode to be sure the correct
                    // function is called (otherwise the bytecode would have
                    // to be verified by the RegExp constructor)
                    self.emit_op(OpCode::REGEXP);

                    self.next_token()?;
                }

                #[cfg(not(feature = "builtin_regexp"))]
                {
                    return Err(self.throw_syntax_error("regex not supported"));
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
                self.emit_scope_var(OpCodeTemp::SCOPE_GET_VAR, Atom::CONST_this, 0);
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
                self.check_not_reserved()?;
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
                        self.emit_scope_var(
                            OpCodeTemp::SCOPE_GET_VAR,
                            Atom::CONST_async,
                            self.cur_func().scope_level as _,
                        );
                    }
                } else {
                    // get atom rc, because later next_token() call will free the atom.
                    let name = self.token.atom().unwrap();

                    if unlikely(
                        name == Atom::CONST_arguments && !self.cur_func().attrs.arguments_allowed,
                    ) {
                        return Err(self.throw_syntax_error(
                            "'arguments' identifier is not allowed in class field initializer",
                        ));
                    } else {
                        self.next_token()?;

                        self.emit_scope_var(
                            OpCodeTemp::SCOPE_GET_VAR,
                            name,
                            self.cur_func().scope_level as _,
                        );
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
                        return Err(self.throw_syntax_error("expecting target"));
                    }
                    if !self.cur_func().attrs.new_target_allowed {
                        return Err(
                            self.throw_syntax_error("new.target only allowed within functions")
                        );
                    }

                    self.next_token()?;

                    self.emit_scope_var(OpCodeTemp::SCOPE_GET_VAR, Atom::CONST_new_target, 0);
                } else {
                    self.parse_postfix_expr(0)?;

                    accept_lparen = true;
                    if self.token.kind != TokenKind::Char('(') {
                        // new operator on an object
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
                        return Err(self.throw_syntax_error(
                            "super() is only valid in a derived class constructor",
                        ));
                    }
                    call_type = FuncCallType::FUNC_CALL_SUPER_CTOR;
                } else if matches!(self.token.kind, TokenKind::Char('.' | '[')) {
                    if !self.cur_func().attrs.super_allowed {
                        return Err(self.throw_syntax_error("'super' is only valid in a method"));
                    }

                    self.emit_scope_var(OpCodeTemp::SCOPE_GET_VAR, Atom::CONST_this, 0);
                    self.emit_scope_var(OpCodeTemp::SCOPE_GET_VAR, Atom::CONST__home_object_, 0);
                    self.emit_op(OpCode::GET_SUPER);
                } else {
                    return Err(self.throw_syntax_error("invalid use of 'super'"));
                }
            }
            TokenKind::IMPORT => {
                self.next_token()?;
                if self.token.kind == TokenKind::Char('.') {
                    self.next_token()?;

                    if !self.token_is_pseudo_keyword(Atom::CONST_meta) {
                        return Err(self.throw_syntax_error("meta expected"));
                    }
                    if !self.is_module {
                        return Err(
                            self.throw_syntax_error("import.meta only valid in module code")
                        );
                    }
                    self.next_token()?;

                    self.emit_op(OpCode::SPECIAL_OBJECT);
                    self.emit_u8(SpecialObject::ImportMeta as u8);
                } else {
                    self.parse_expect(TokenKind::Char('('))?;
                    if !accept_lparen {
                        return Err(self.throw_syntax_error("invalid use of 'import()'"));
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
                return Err(self.throw_syntax_error(&msg));
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
                    return Err(self.throw_syntax_error(
                        "template literal cannot appear in an optional chain",
                    ));
                }
                let opcode = unsafe {
                    self.parse_func_call2(
                        FuncCallType::FUNC_CALL_TEMPLATE,
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )
                };

                let argc = self.parse_template(true)?;
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
            // Strict ES7 exponentiation syntax rules: To solve
            // conficting semantics between different implementations
            // regarding the precedence of prefix operators and the
            // postfix exponential, ES7 specifies that -2**2 is a
            // syntax error.
            if parse_flags & PF_POW_FORBIDDEN != 0 {
                return Err(self.ctx().throw_syntax_error(
                    "unparenthesized unary expression can't appear on the left-hand side of '**'",
                ));
            }

            self.next_token()?;
            self.parse_unary(PF_POW_ALLOWED)?;
            self.emit_op(OpCode::POW);
        }

        Ok(())
    }

    pub(super) fn optional_chain_test(&mut self, label: &mut i32, drop_count: i32) {
        if *label < 0 {
            *label = self.cur_func_mut().add_label().map_or(-1, |x| x as i32);
        }

        // XXX: could be more efficient with a specific opcode
        self.emit_op(OpCode::DUP);
        self.emit_op(OpCode::IS_UNDEFINED_OR_NULL);
        let label_next = self.emit_goto(OpCode::IF_FALSE, -1);
        for _ in 0..drop_count {
            self.emit_op(OpCode::DROP);
        }
        self.emit_op(OpCode::UNDEFINED);
        self.emit_goto(OpCode::GOTO, *label);
        self.emit_label(label_next);
    }

    pub(super) fn parse_yield(&mut self, parse_flags: i32) -> Result<(), i32> {
        debug_assert_eq!(self.token.kind, TokenKind::YIELD);

        if !matches!(
            self.cur_func().attrs.func_kind,
            FunctionKind::Generator | FunctionKind::AsyncGenerator
        ) {
            return Err(self.throw_syntax_error("unexpected 'yield' keyword"));
        } else if !self.cur_func().in_function_body {
            return Err(self.throw_syntax_error("yield in default expression"));
        }

        self.next_token()?;

        let is_async = self.cur_func().attrs.func_kind == FunctionKind::AsyncGenerator;
        let mut is_star = false;

        // XXX: is there a better method to detect 'yield' without parameters?
        if !self.got_lf
            && !matches!(
                self.token.kind,
                TokenKind::Char(';' | ')' | ']' | '}' | ',' | ':')
            )
        {
            if self.token.kind == TokenKind::Char('*') {
                is_star = true;
                self.next_token()?;
            }
            self.parse_assign_expr2(parse_flags)?;
        } else {
            self.emit_op(OpCode::UNDEFINED);
        }

        if is_star {
            let (label_loop, label_yield) = {
                let fd = self.cur_func_mut();
                (fd.add_label()?, fd.add_label()?)
            };

            self.emit_op(if is_async {
                OpCode::FOR_AWAIT_OF_START
            } else {
                OpCode::FOR_OF_START
            });

            // remove the catch offset (XXX: could avoid pushing back undefined)
            self.emit_op(OpCode::DROP);
            self.emit_op(OpCode::UNDEFINED);

            // initial value
            self.emit_op(OpCode::UNDEFINED);

            self.emit_label(label_loop as _);
            self.emit_op(OpCode::ITERATOR_NEXT);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CHECK_OBJECT);
            self.emit_get_field2(Atom::CONST_done);
            let label_next = self.emit_goto(OpCode::IF_TRUE, -1); // end of loop
            self.emit_label(label_yield as _);

            if is_async {
                // OP_async_yield_star takes the value as parameter
                self.emit_get_field(Atom::CONST_value);
                self.emit_op(OpCode::ASYNC_YIELD_STAR);
            } else {
                // OP_yield_star takes (value, done) as parameter
                self.emit_op(OpCode::YIELD_STAR);
            }
            self.emit_op(OpCode::DUP);
            let label_return = self.emit_goto(OpCode::IF_TRUE, -1);
            self.emit_op(OpCode::DROP);
            self.emit_goto(OpCode::GOTO, label_loop as _);

            self.emit_label(label_return);
            self.emit_op(OpCode::PUSH_I32);
            self.emit_u32(2);
            self.emit_op(OpCode::STRICT_EQ);

            let label_throw = self.emit_goto(OpCode::IF_TRUE, -1);

            // return handling
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CALL);
            self.emit_u8(0);
            let label_return1 = self.emit_goto(OpCode::IF_TRUE, -1);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CHECK_OBJECT);
            self.emit_get_field2(Atom::CONST_done);
            self.emit_goto(OpCode::IF_FALSE, label_yield as _);

            self.emit_get_field(Atom::CONST_value);

            self.emit_label(label_return1);
            self.emit_op(OpCode::NIP);
            self.emit_op(OpCode::NIP);
            self.emit_op(OpCode::NIP);
            self.emit_return(true);

            // throw handling
            self.emit_label(label_throw);
            self.emit_op(OpCode::ITERATOR_CALL);
            self.emit_u8(1);
            let label_throw1 = self.emit_goto(OpCode::IF_TRUE, -1);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::ITERATOR_CHECK_OBJECT);
            self.emit_get_field2(Atom::CONST_done);
            self.emit_goto(OpCode::IF_FALSE, label_yield as _);
            self.emit_goto(OpCode::GOTO, label_next);
            // close the iterator and throw a type error exception
            self.emit_label(label_throw1);
            self.emit_op(OpCode::ITERATOR_CALL);
            self.emit_u8(2);
            let label_throw2 = self.emit_goto(OpCode::IF_TRUE, -1);
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_label(label_throw2);

            self.emit_op(OpCode::THROW_ERROR);
            self.emit_atom(Atom::NULL);
            self.emit_u8(JS_THROW_ERROR_ITERATOR_THROW);

            self.emit_label(label_next);
            self.emit_get_field(Atom::CONST_value);
            self.emit_op(OpCode::NIP); // keep the value associated with done = true
            self.emit_op(OpCode::NIP);
            self.emit_op(OpCode::NIP);
        } else {
            if is_async {
                self.emit_op(OpCode::AWAIT);
            }
            self.emit_op(OpCode::YIELD);
            self.with_forward_label(OpCode::IF_FALSE, |s| {
                s.emit_return(true);
                Ok(())
            })?;
        }

        Ok(())
    }

    // ===== binary / higher-expr / unary / delete =====
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
    fn parse_expr_binary(&mut self, level: i32, parse_flags: i32) -> Result<(), i32> {
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
                    return Err(self.throw_syntax_error("expect 'in'"));
                }

                self.next_token()?;
                // right operand: shift and tighter (tier < 4)
                self.parse_expr_binary(4 - 1, parse_flags & !PF_ARROW_FUNC)?;

                self.emit_scope_var(
                    OpCodeTemp::SCOPE_IN_PRIVATE_FIELD,
                    atom,
                    self.cur_func().scope_level as _,
                );

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
                        return Err(self.throw_syntax_error("cannot mix ?? with && or ||"));
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
                    self.with_forward_label(OpCode::GOTO, |s| {
                        s.emit_label(label1);
                        while depth_lvalue != 0 {
                            s.emit_op(OpCode::NIP);
                            depth_lvalue -= 1;
                        }
                        Ok(())
                    })?;
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
                    return Err(self.throw_syntax_error("unexpected 'await' keyword"));
                }
                if !self.cur_func().in_function_body {
                    return Err(self.throw_syntax_error("await in default expression"));
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
                return Err(self.throw_syntax_error(
                    "unparenthesized unary expression can't appear on the left-hand side of '**'",
                ));
            }

            self.next_token()?;
            self.parse_unary(PF_POW_ALLOWED)?;
            self.emit_op(OpCode::POW);
        }

        Ok(())
    }

    fn parse_delete(&mut self) -> Result<(), i32> {
        let fd0 = self.cur_func_def;

        self.next_token()?;
        self.parse_unary(PF_POW_FORBIDDEN)?;

        debug_assert_eq!(
            fd0, self.cur_func_def,
            "cur_func_def must not change after parse"
        );

        let fd = unsafe { fd0.as_mut().unwrap() };

        match fd.get_prev_opcode() {
            opcode @ (opcodes::OP_get_field | opcodes::op_get_field_opt_chain) => {
                // The optional-chain jump target for `get_field_opt_chain` is
                // stored in the `op_label` temp opcode emitted right after the
                // field opcode (see parse_expr_postfix). Its byte offset
                // depends on the field opcode's payload size, which varies
                // (e.g. the inline_cache feature adds an ic-index u16), so
                // derive it from the opcode meta instead of hardcoding
                // `+1 + 4 + 1`.
                let op_size = opcode_info_intermediate(opcode).size as usize;
                let opt_chain_label: i32 = if opcode == opcodes::op_get_field_opt_chain {
                    let label_op_pos = fd.last_opcode_pos as usize + op_size;
                    debug_assert_eq!(fd.byte_code[label_op_pos], OpCodeTemp::LABEL as u8);
                    unsafe {
                        read_from_ptr::<i32>(
                            fd.byte_code.as_ptr().offset(label_op_pos as isize + 1),
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
                self.emit_push_const(JSValue::make_string(name_str), true)?;
                self.emit_op(OpCode::DELETE);

                if opt_chain_label >= 0 {
                    self.with_forward_label(OpCode::GOTO, |s| {
                        s.emit_label(opt_chain_label);
                        // if the optional chain is not taken, return 'true'
                        s.emit_op(OpCode::DROP);
                        s.emit_op(OpCode::PUSH_TRUE);
                        Ok(())
                    })?;
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
                // optional-chain label lives in the following op_label opcode;
                // its position depends on the field opcode's payload size.
                const op_size: usize =
                    opcode_info_intermediate(opcodes::op_get_array_el_opt_chain).size as usize;
                let label_op_pos = fd.last_opcode_pos as usize + op_size;
                debug_assert_eq!(fd.byte_code[label_op_pos], OpCodeTemp::LABEL as u8);
                let opt_chain_label = unsafe {
                    read_from_ptr::<u32>(fd.byte_code.as_ptr().offset(label_op_pos as isize + 1))
                };

                fd.byte_code.truncate(fd.last_opcode_pos as _);

                self.emit_op(OpCode::DELETE);
                self.with_forward_label(OpCode::GOTO, |s| {
                    s.emit_label(opt_chain_label as _);
                    // if the optional chain is not taken, return 'true'
                    s.emit_op(OpCode::DROP);
                    s.emit_op(OpCode::PUSH_TRUE);
                    Ok(())
                })?;
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
                    return Err(
                        self.throw_syntax_error("cannot delete a direct reference in strict mode")
                    );
                } else {
                    fd.byte_code[fd.last_opcode_pos as usize] = OpCodeTemp::SCOPE_DELETE_VAR as u8;
                }
            }
            opcodes::op_scope_get_private_field => {
                return Err(self.throw_syntax_error("cannot delete a private class field"));
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
