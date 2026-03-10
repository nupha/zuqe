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
    state::{JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POSTFIX_CALL, SKIP_HAS_ELLIPSIS},
    tokenize::TokenKind,
};

impl<'s> JSParseState<'s> {
    // allowed parse_flags: PF_IN_ACCEPTED
    pub(super) fn parse_assign_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
        if self.token.kind == TokenKind::YIELD {
            return self.parse_yield(parse_flags);
        }

        // name0 is used to check for OP_set_name pattern,
        // this atom is not duplicated
        let name0 = if self.token.kind == TokenKind::IDENT {
            self.token.atom().unwrap()
        } else {
            Atom::NULL
        };

        self.parse_cond_expr(parse_flags | PF_ARROW_FUNC)?;

        let mut opcode: i32 = 0;
        let mut scope: i32 = 0;

        match self.token.kind {
            op if op == TokenKind::Char('=') || op.is_op_assign() => {
                self.next_token()?;

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
                    if opcode == OpCode::GET_REF_VALUE as _ && name == name0 {
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

                let mut label: i32 = 0;
                let mut depth_lvalue: i32 = 0;
                let mut name = Atom::NULL;

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

                // XXX: we disable the OP_put_ref_value optimization by not
                // using put_lvalue() otherwise depth_lvalue is not correct
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

                // remove the lvalue stack entries
                while depth_lvalue != 0 {
                    self.emit_op(OpCode::NIP);
                    depth_lvalue -= 1;
                }

                self.emit_label(label2);
            }

            _ => {}
        }

        Ok(())
    }

    /// a short-cut to `self.parse_assign_expr2(PF_IN_ACCEPTED)`
    #[inline(always)]
    pub(super) fn parse_assign_expr(&mut self) -> Result<(), i32> {
        self.parse_assign_expr2(PF_IN_ACCEPTED)
    }

    // allowed parse_flags: PF_IN_ACCEPTED
    pub(super) fn parse_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
        let mut comma = false;

        loop {
            self.parse_assign_expr2(parse_flags)?;

            if comma {
                // prevent get_lvalue from using the last expression
                // as an lvalue. This also prevents the conversion of
                // of get_var to get_ref for method lookup in function
                // call inside `with` statement.
                self.cur_func_mut().last_opcode_pos = -1;
            }

            if self.token.kind != TokenKind::Char(',') {
                break;
            }

            comma = true;
            self.next_token()?;
            self.emit_op(OpCode::DROP);
        }

        Ok(())
    }

    /// a short-cut to `self.parse_expr2(PF_IN_ACCEPTED)`
    #[inline(always)]
    pub(super) fn parse_expr(&mut self) -> Result<(), i32> {
        self.parse_expr2(PF_IN_ACCEPTED)
    }

    pub(super) fn parse_expr_parens(&mut self) -> Result<(), i32> {
        self.parse_expect(TokenKind::Char('('))?;
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
                    self.parse_expr_parens()?;
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
            self.emit_label_raw(optional_chaining_label);

            /* modify the last opcode to indicate optional chain */
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
