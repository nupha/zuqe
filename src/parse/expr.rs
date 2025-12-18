use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp, SpecialObject, opcodes},
    function::{FuncCallType, JSFunctionKindEnum},
    number::IntOrFloat,
    utils::unlikely,
    value::JSValue,
};

use super::{
    array::js_parse_array_literal,
    emit::get_prev_opcode,
    esm::JSParseExportEnum,
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    state::{JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POSTFIX_CALL, SKIP_HAS_ELLIPSIS},
    token::{
        CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_COMMA, CHAR_CURLY_L, CHAR_EQUAL, CHAR_SLASH,
        CHAR_SQUARE_L, TOK_ARROW, TOK_CLASS, TOK_DIV_ASSIGN, TOK_DOUBLE_QUESTION_MARK_ASSIGN,
        TOK_FALSE, TOK_FUNCTION, TOK_IDENT, TOK_IMPORT, TOK_LAND_ASSIGN, TOK_LOR_ASSIGN,
        TOK_MUL_ASSIGN, TOK_NEW, TOK_NULL, TOK_NUMBER, TOK_POW_ASSIGN, TOK_QUESTION_MARK_DOT,
        TOK_STRING, TOK_SUPER, TOK_TEMPLATE, TOK_THIS, TOK_TRUE, TOK_YIELD,
    },
};

impl<'s> JSParseState<'s> {
    // allowed parse_flags: PF_IN_ACCEPTED
    pub(super) fn parse_assign_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
        if self.token.val == TOK_YIELD {
            return self.parse_yield(parse_flags);
        }

        // name0 is used to check for OP_set_name pattern,
        // this atom is not duplicated
        let name0 = if self.token.val == TOK_IDENT {
            self.token.atom().unwrap()
        } else {
            Atom::NULL
        };

        self.parse_cond_expr(parse_flags | PF_ARROW_FUNC)?;

        let mut opcode: i32 = 0;
        let mut scope: i32 = 0;

        match self.token.val {
            op @ (CHAR_EQUAL | TOK_MUL_ASSIGN..=TOK_POW_ASSIGN) => {
                self.next_token()?;

                let mut label: i32 = 0;
                let mut name = Atom::NULL;

                self.get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut name,
                    &mut label,
                    std::ptr::null_mut(),
                    op != CHAR_EQUAL,
                    op,
                )?;

                self.parse_assign_expr2(parse_flags)?;

                if op == CHAR_EQUAL {
                    if opcode == OpCode::GET_REF_VALUE as _ && name == name0 {
                        self.set_object_name(name);
                    }
                } else {
                    const assign_opcodes: [OpCode; 12] = [
                        OpCode::MUL,
                        OpCode::DIV,
                        OpCode::MOD,
                        OpCode::ADD,
                        OpCode::SUB,
                        OpCode::SHL,
                        OpCode::SAR,
                        OpCode::SHR,
                        OpCode::AND,
                        OpCode::XOR,
                        OpCode::OR,
                        OpCode::POW,
                    ];
                    self.emit_op(assign_opcodes[(op - TOK_MUL_ASSIGN) as usize]);
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
            op @ (TOK_LAND_ASSIGN..=TOK_DOUBLE_QUESTION_MARK_ASSIGN) => {
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
                if op == TOK_DOUBLE_QUESTION_MARK_ASSIGN {
                    self.emit_op(OpCode::IS_UNDEFINED_OR_NULL);
                }
                let label1 = self.emit_goto(
                    if op == TOK_LOR_ASSIGN {
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

            if self.token.val != CHAR_COMMA {
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
        self.parse_expect(CHAR_BRACKET_L)?;
        self.parse_expr()?;
        self.parse_expect(CHAR_BRACKET_R)?;
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

        match self.token.val {
            TOK_NUMBER => {
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
            TOK_TEMPLATE => {
                let mut _x = 0;
                self.parse_template(false, &mut _x)?;
            }
            TOK_STRING => {
                let sbuf = self.token.data.take_string().unwrap();
                self.emit_push_const(self.ctx.create_string_with_buf(sbuf)?.into(), true)?;
                self.next_token()?;
            }

            t @ (TOK_DIV_ASSIGN | CHAR_SLASH) => unsafe {
                self.buf_ptr = self
                    .buf_ptr
                    .offset(if t == TOK_DIV_ASSIGN { -2 } else { -1 });

                // the previous token is '/' or '/=', so no need to free
                self.parse_regexp()?;

                // emit build RegExp opcode
                let re_id = self.token.data.regexp().unwrap();
                self.emit_push_const(
                    Atom::CONST_empty_string
                        .force_string(&self.ctx)
                        .unwrap()
                        .into(),
                    false,
                )?;
                self.emit_push_const(JSValue::Int(re_id as i32), false)?;
                // we use a specific opcode to be sure the correct
                // function is called (otherwise the bytecode would have
                // to be verified by the RegExp constructor)
                self.emit_op(OpCode::REGEXP);

                self.next_token()?;
            },

            // '('
            CHAR_BRACKET_L => {
                let mut _p = 0;
                if parse_flags & PF_ARROW_FUNC != 0
                    && self.parse_skip_parens_token(&mut _p, true)? == TOK_ARROW
                {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        self.token.ptr,
                        self.token.line_num,
                    )?;
                } else {
                    self.parse_expr_parens()?;
                }
            }
            TOK_FUNCTION => {
                self.parse_function_decl(
                    JSParseFunctionEnum::JS_PARSE_FUNC_EXPR,
                    JSFunctionKindEnum::JS_FUNC_NORMAL,
                    self.token.ptr,
                    self.token.line_num,
                )?;
            }
            TOK_CLASS => {
                self.parse_class(true, JSParseExportEnum::JS_PARSE_EXPORT_NONE)?;
            }
            TOK_NULL => {
                self.next_token()?;
                self.emit_op(OpCode::NULL);
            }
            // TODO: how about define TOK_UNDEFINDED token, and emit UNDEFINED opcode?
            // TOK_UNDEFINED => {
            //     self.next_token()?;
            //     emit_op(self, OpCode::UNDEFINED);
            // }
            TOK_THIS => {
                self.next_token()?;
                self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                self.emit_atom(Atom::CONST_this);
                self.emit_u16(0);
            }
            TOK_FALSE => {
                self.next_token()?;
                self.emit_op(OpCode::PUSH_FALSE);
            }
            TOK_TRUE => {
                self.next_token()?;
                self.emit_op(OpCode::PUSH_TRUE);
            }
            TOK_IDENT => {
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }
                if (parse_flags & PF_ARROW_FUNC) != 0 && self.peek_token(true) == TOK_ARROW {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        self.token.ptr,
                        self.token.line_num,
                    )?;
                } else if self.token_is_pseudo_keyword(Atom::CONST_async)
                    && self.peek_token(true) != '\n' as i32
                {
                    let source_ptr = self.token.ptr;
                    let source_line_num = self.token.line_num;

                    self.next_token()?;

                    if self.token.val == TOK_FUNCTION {
                        self.parse_function_decl(
                            JSParseFunctionEnum::JS_PARSE_FUNC_EXPR,
                            JSFunctionKindEnum::JS_FUNC_ASYNC,
                            source_ptr,
                            source_line_num,
                        )?;
                    } else if (parse_flags & PF_ARROW_FUNC) != 0
                        && (self.token.val == b'(' as i32 && {
                            let mut _p = 0;
                            self.parse_skip_parens_token(&mut _p, true)? == TOK_ARROW
                        })
                        || (self.token.val == TOK_IDENT
                            && !self.token.data.is_reserved()
                            && self.peek_token(true) == TOK_ARROW)
                    {
                        self.parse_function_decl(
                            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                            JSFunctionKindEnum::JS_FUNC_ASYNC,
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
            // '{' | '['
            CHAR_CURLY_L | CHAR_SQUARE_L => {
                let mut skip_bits: i32 = 0;
                if self.parse_skip_parens_token(&mut skip_bits, false)? == '=' as i32 {
                    self.parse_destructuring_element(
                        0,
                        false,
                        false,
                        (skip_bits & SKIP_HAS_ELLIPSIS != 0) as i32,
                        true,
                        false,
                    )?;
                } else if self.token.val == b'{' as i32 {
                    self.parse_object_literal()?;
                } else if js_parse_array_literal(self) != 0 {
                    return Err(-1);
                }
            }
            TOK_NEW => {
                self.next_token()?;

                if self.token.val == b'.' as i32 {
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
                    if self.token.val != b'(' as i32 {
                        /* new operator on an object */
                        self.emit_op(OpCode::DUP);
                        self.emit_op(OpCode::CALL_CONSTRUCTOR);
                        self.emit_u16(0);
                    } else {
                        call_type = FuncCallType::FUNC_CALL_NEW;
                    }
                }
            }
            TOK_SUPER => {
                self.next_token()?;
                if self.token.val == b'(' as i32 {
                    if !self.cur_func().attrs.super_call_allowed {
                        self.throw_syntax_error(
                            "super() is only valid in a derived class constructor",
                        );
                        return Err(-1);
                    }
                    call_type = FuncCallType::FUNC_CALL_SUPER_CTOR;
                } else if self.token.val == b'.' as i32 || self.token.val == b'[' as i32 {
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
            TOK_IMPORT => {
                self.next_token()?;
                if self.token.val == b'.' as i32 {
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
                    self.parse_expect(CHAR_BRACKET_L)?;
                    if !accept_lparen {
                        self.throw_syntax_error("invalid use of 'import()'");
                        return Err(-1);
                    }
                    self.parse_assign_expr()?;
                    self.parse_expect(CHAR_BRACKET_R)?;
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

            if self.token.val == TOK_QUESTION_MARK_DOT {
                // optional chaining
                self.next_token()?;

                has_optional_chain = true;

                if self.token.val == b'(' as i32 && accept_lparen {
                    self.parse_func_call(
                        call_type,
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )?;
                    call_type = FuncCallType::FUNC_CALL_NORMAL;
                } else if self.token.val == b'[' as i32 {
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
            } else if self.token.val == TOK_TEMPLATE && call_type == FuncCallType::FUNC_CALL_NORMAL
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
            } else if self.token.val == '(' as i32 && accept_lparen {
                // function call
                self.parse_func_call(call_type, has_optional_chain, &mut optional_chaining_label)?;
                call_type = FuncCallType::FUNC_CALL_NORMAL;
            } else if self.token.val == b'.' as i32 {
                self.next_token()?;
                self.parse_property(
                    unsafe { self.cur_func_def.as_ref().unwrap() },
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
            } else if self.token.val == b'[' as i32 {
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
            match get_prev_opcode(self.cur_func()) {
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
