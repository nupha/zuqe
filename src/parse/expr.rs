use std::ffi::CStr;

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, consts::*},
    bytecode::{OpCode, OpCodeTemp, opcodes},
    error::JS_BACKTRACE_FLAG_SINGLE_LEVEL,
    function::{FuncCallType, JSFunctionKindEnum},
    parse::token::CHAR_SLASH,
    value::JSValue,
};

use super::{
    array::js_parse_array_literal,
    class::js_parse_class,
    destructing::js_parse_destructuring_element,
    emit::{
        OPSpecialObjectEnum, emit_atom_dup, emit_goto, emit_label, emit_label_raw, emit_op,
        emit_op_temp, emit_push_const, emit_u8, emit_u16, emit_u32, get_prev_opcode,
    },
    error::js_parse_error_reserved_identifier,
    esm::JSParseExportEnum,
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    obj::set_object_name,
    parse::js_parse_skip_parens_token,
    regexp::js_parse_regexp,
    state::{JSParseState, PF_ARROW_FUNC, PF_IN_ACCEPTED, PF_POSTFIX_CALL, SKIP_HAS_ELLIPSIS},
    token::{
        CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_COMMA, CHAR_CURLY_L, CHAR_EQUAL, CHAR_SQUARE_L,
        TOK_ARROW, TOK_CLASS, TOK_DIV_ASSIGN, TOK_DOUBLE_QUESTION_MARK_ASSIGN, TOK_FALSE,
        TOK_FUNCTION, TOK_IDENT, TOK_IMPORT, TOK_LAND_ASSIGN, TOK_LOR_ASSIGN, TOK_MUL_ASSIGN,
        TOK_NEW, TOK_NULL, TOK_NUMBER, TOK_POW_ASSIGN, TOK_QUESTION_MARK_DOT, TOK_STRING,
        TOK_SUPER, TOK_TEMPLATE, TOK_THIS, TOK_TRUE, TOK_YIELD, token_is_pseudo_keyword,
    },
};

impl<'s> JSParseState<'s> {
    // allowed parse_flags: PF_IN_ACCEPTED
    pub(super) unsafe fn parse_assign_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
        if self.token.val == TOK_YIELD {
            return self.parse_yield(parse_flags);
        }

        // name0 is used to check for OP_set_name pattern, not duplicated
        let name0 = if self.token.val == TOK_IDENT {
            self.token.data.ident_atom().unwrap()
        } else {
            JS_ATOM_NULL
        };

        self.parse_cond_expr(parse_flags | PF_ARROW_FUNC)?;

        let mut opcode: i32 = 0;
        let mut scope: i32 = 0;
        let mut name = JS_ATOM_NULL;

        match self.token.val {
            op @ (CHAR_EQUAL | TOK_MUL_ASSIGN..=TOK_POW_ASSIGN) => {
                self.next_token()?;

                let mut label: i32 = 0;
                self.get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut name,
                    &mut label,
                    std::ptr::null_mut(),
                    op != CHAR_EQUAL,
                    op,
                )?;

                if self.parse_assign_expr2(parse_flags).is_err() {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }

                if op == CHAR_EQUAL {
                    if opcode == OpCode::GET_REF_VALUE as _ && name == name0 {
                        set_object_name(self, name);
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
                    emit_op(self, assign_opcodes[(op - TOK_MUL_ASSIGN) as usize]);
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

                self.get_lvalue(
                    &mut opcode,
                    &mut scope,
                    &mut name,
                    &mut label,
                    &mut depth_lvalue,
                    true,
                    op,
                )?;

                emit_op(self, OpCode::DUP);
                if op == TOK_DOUBLE_QUESTION_MARK_ASSIGN {
                    emit_op(self, OpCode::IS_UNDEFINED_OR_NULL);
                }
                let label1 = emit_goto(
                    self,
                    if op == TOK_LOR_ASSIGN {
                        OpCode::IF_TRUE
                    } else {
                        OpCode::IF_FALSE
                    },
                    -1,
                );
                emit_op(self, OpCode::DROP);

                if self.parse_assign_expr2(parse_flags).is_err() {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }

                if opcode == OpCode::GET_REF_VALUE as i32 && name == name0 {
                    set_object_name(self, name);
                }

                match depth_lvalue {
                    1 => emit_op(self, OpCode::INSERT2),
                    2 => emit_op(self, OpCode::INSERT3),
                    3 => emit_op(self, OpCode::INSERT4),
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

                let label2 = emit_goto(self, OpCode::GOTO, -1);
                emit_label(self, label1);

                // remove the lvalue stack entries
                while depth_lvalue != 0 {
                    emit_op(self, OpCode::NIP);
                    depth_lvalue -= 1;
                }

                emit_label(self, label2);
            }
            _ => {}
        }

        Ok(())
    }

    /// a short-cut to `self.parse_assign_expr2(PF_IN_ACCEPTED)`
    #[inline(always)]
    pub unsafe fn parse_assign_expr(&mut self) -> Result<(), i32> {
        self.parse_assign_expr2(PF_IN_ACCEPTED)
    }

    // allowed parse_flags: PF_IN_ACCEPTED
    pub(super) unsafe fn parse_expr2(&mut self, parse_flags: i32) -> Result<(), i32> {
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
            emit_op(self, OpCode::DROP);
        }

        Ok(())
    }

    /// a short-cut to `self.parse_expr2(PF_IN_ACCEPTED)`
    #[inline(always)]
    pub unsafe fn parse_expr(&mut self) -> Result<(), i32> {
        self.parse_expr2(PF_IN_ACCEPTED)
    }

    pub unsafe fn parse_expr_parens(&mut self) -> Result<(), i32> {
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
    pub unsafe fn parse_postfix_expr(&mut self, parse_flags: i32) -> Result<(), i32> {
        let mut accept_lparen: bool = (parse_flags & PF_POSTFIX_CALL) != 0;
        let mut call_type = FuncCallType::FUNC_CALL_NORMAL;

        match self.token.val {
            TOK_NUMBER => {
                let val = self.token.data.number().unwrap().clone();
                if let JSValue::Int(i) = val {
                    emit_op(self, OpCode::PUSH_I32);
                    emit_u32(self, i as u32);
                } else {
                    if emit_push_const(self, val, false) < 0 {
                        return Err(-1);
                    }
                }

                self.next_token()?;
            }
            TOK_TEMPLATE => {
                self.parse_template(false, core::ptr::null_mut())?;
            }
            TOK_STRING => {
                if emit_push_const(self, self.token.data.string().unwrap().clone(), true) != 0 {
                    return Err(-1);
                }
                self.next_token()?;
            }

            t @ (TOK_DIV_ASSIGN | CHAR_SLASH) => {
                self.buf_ptr = self
                    .buf_ptr
                    .offset(if t == TOK_DIV_ASSIGN { -2 } else { -1 });

                // parse_regexp:
                let compile_regexp = self.ctx.0.borrow().compile_regexp;
                if let Some(cb) = compile_regexp {
                    /* the previous token is '/' or '/=', so no need to free */
                    if js_parse_regexp(self) != 0 {
                        return Err(-1);
                    }

                    emit_push_const(self, self.token.data.regexp_body().unwrap().clone(), false);

                    let str = cb(
                        &self.ctx,
                        self.token.data.regexp_body().unwrap().clone(),
                        self.token.data.regexp_flags().unwrap().clone(),
                    );
                    if str.is_exception() {
                        // add the line number info
                        let backtrace_flags =
                            if !self.cur_func_ptr.is_null() && self.cur_func().backtrace_barrier {
                                JS_BACKTRACE_FLAG_SINGLE_LEVEL
                            } else {
                                0
                            };

                        let exc = self.ctx.exception();
                        self.ctx.build_backtrace_to(
                            &exc,
                            Some((self.filename, self.token.line_num)),
                            backtrace_flags,
                        );

                        return Err(-1);
                    }

                    let ret = emit_push_const(self, str, false);
                    if ret != 0 {
                        return Err(-1);
                    }

                    /* we use a specific opcode to be sure the correct
                    function is called (otherwise the bytecode would have
                    to be verified by the RegExp constructor) */
                    emit_op(self, OpCode::REGEXP);
                    self.next_token()?;
                } else {
                    self.throw_syntax_error("RegExp are not supported");
                    return Err(-1);
                }
            }

            // '('
            CHAR_BRACKET_L => {
                if (parse_flags & PF_ARROW_FUNC) != 0
                    && js_parse_skip_parens_token(self, core::ptr::null_mut(), true) == TOK_ARROW
                {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        JS_ATOM_NULL,
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
                    JS_ATOM_NULL,
                    self.token.ptr,
                    self.token.line_num,
                )?;
            }
            TOK_CLASS => {
                if js_parse_class(self, true, JSParseExportEnum::JS_PARSE_EXPORT_NONE) != 0 {
                    return Err(-1);
                }
            }
            TOK_NULL => {
                self.next_token()?;
                emit_op(self, OpCode::NULL);
            }
            // TODO: how about define TOK_UNDEFINDED token, and emit UNDEFINED opcode?
            // TOK_UNDEFINED => {
            //     self.next_token()?;
            //     emit_op(self, OpCode::UNDEFINED);
            // }
            TOK_THIS => {
                self.next_token()?;
                emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                emit_atom_dup(self, JS_ATOM_this);
                emit_u16(self, 0);
            }
            TOK_FALSE => {
                self.next_token()?;
                emit_op(self, OpCode::PUSH_FALSE);
            }
            TOK_TRUE => {
                self.next_token()?;
                emit_op(self, OpCode::PUSH_TRUE);
            }
            TOK_IDENT => {
                let name: JSAtom;
                if self.token.data.is_reserved() {
                    return if js_parse_error_reserved_identifier(self) < 0 {
                        Err(-1)
                    } else {
                        Ok(())
                    };
                }
                if (parse_flags & PF_ARROW_FUNC) != 0 && self.peek_token(true) == TOK_ARROW {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        JS_ATOM_NULL,
                        self.token.ptr,
                        self.token.line_num,
                    )?;
                } else if token_is_pseudo_keyword(self, JS_ATOM_async)
                    && self.peek_token(true) != '\n' as i32
                {
                    let source_ptr = self.token.ptr;
                    let source_line_num = self.token.line_num;

                    self.next_token()?;

                    if self.token.val == TOK_FUNCTION {
                        self.parse_function_decl(
                            JSParseFunctionEnum::JS_PARSE_FUNC_EXPR,
                            JSFunctionKindEnum::JS_FUNC_ASYNC,
                            JS_ATOM_NULL,
                            source_ptr,
                            source_line_num,
                        )?;
                    } else if (parse_flags & PF_ARROW_FUNC) != 0
                        && (self.token.val == b'(' as i32
                            && js_parse_skip_parens_token(self, core::ptr::null_mut(), true)
                                == TOK_ARROW)
                        || (self.token.val == TOK_IDENT
                            && !self.token.data.is_reserved()
                            && self.peek_token(true) == TOK_ARROW)
                    {
                        self.parse_function_decl(
                            JSParseFunctionEnum::JS_PARSE_FUNC_ARROW,
                            JSFunctionKindEnum::JS_FUNC_ASYNC,
                            JS_ATOM_NULL,
                            source_ptr,
                            source_line_num,
                        )?;
                    } else {
                        name = JS_ATOM_async.dup(self.ctx.rt());
                        emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                        emit_u32(self, name.0);
                        emit_u16(self, self.cur_func().scope_level as _);
                    }
                } else {
                    if self.token.data.ident_atom().unwrap() == JS_ATOM_arguments
                        && !self.cur_func().arguments_allowed
                    {
                        self.throw_syntax_error(
                            "'arguments' identifier is not allowed in class field initializer",
                        );
                        return Err(-1);
                    }

                    name = self.token.data.ident_atom().unwrap().dup(&self.ctx.rt());

                    if self.next_token().is_err() {
                        JS_FreeAtom(&self.ctx, name);
                        return Err(-1);
                    }

                    // do_get_var:
                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                    emit_u32(self, name.0);
                    emit_u16(self, self.cur_func().scope_level as _);
                }
            }
            // '{' | '['
            CHAR_CURLY_L | CHAR_SQUARE_L => {
                let mut skip_bits: i32 = 0;
                if js_parse_skip_parens_token(self, &mut skip_bits, false) == '=' as i32 {
                    if js_parse_destructuring_element(
                        self,
                        0,
                        false,
                        false,
                        (skip_bits & SKIP_HAS_ELLIPSIS != 0) as i32,
                        true,
                    ) < 0
                    {
                        return Err(-1);
                    }
                } else {
                    if self.token.val == b'{' as i32 {
                        self.parse_object_literal()?;
                    } else {
                        if js_parse_array_literal(self) != 0 {
                            return Err(-1);
                        }
                    }
                }
            }
            TOK_NEW => {
                self.next_token()?;

                if self.token.val == b'.' as i32 {
                    self.next_token()?;

                    if !token_is_pseudo_keyword(self, JS_ATOM_target) {
                        self.throw_syntax_error("expecting target");
                        return Err(-1);
                    }
                    if !self.cur_func().new_target_allowed {
                        self.throw_syntax_error("new.target only allowed within functions");
                        return Err(-1);
                    }

                    self.next_token()?;

                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                    emit_atom_dup(self, JS_ATOM_new_target);
                    emit_u16(self, 0);
                } else {
                    self.parse_postfix_expr(0)?;

                    accept_lparen = true;
                    if self.token.val != b'(' as i32 {
                        /* new operator on an object */
                        emit_op(self, OpCode::DUP);
                        emit_op(self, OpCode::CALL_CONSTRUCTOR);
                        emit_u16(self, 0);
                    } else {
                        call_type = FuncCallType::FUNC_CALL_NEW;
                    }
                }
            }
            TOK_SUPER => {
                self.next_token()?;
                if self.token.val == b'(' as i32 {
                    if !self.cur_func().super_call_allowed {
                        self.throw_syntax_error(
                            "super() is only valid in a derived class constructor",
                        );
                        return Err(-1);
                    }
                    call_type = FuncCallType::FUNC_CALL_SUPER_CTOR;
                } else if self.token.val == b'.' as i32 || self.token.val == b'[' as i32 {
                    if !self.cur_func().super_allowed {
                        self.throw_syntax_error("'super' is only valid in a method");
                        return Err(-1);
                    }

                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                    emit_atom_dup(self, JS_ATOM_this);
                    emit_u16(self, 0);
                    emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                    emit_atom_dup(self, JS_ATOM__home_object_);
                    emit_u16(self, 0);
                    emit_op(self, OpCode::GET_SUPER);
                } else {
                    self.throw_syntax_error("invalid use of 'super'");
                    return Err(-1);
                }
            }
            TOK_IMPORT => {
                self.next_token()?;
                if self.token.val == b'.' as i32 {
                    self.next_token()?;
                    if !token_is_pseudo_keyword(self, JS_ATOM_meta) {
                        self.throw_syntax_error("meta expected");
                        return Err(-1);
                    }
                    if !self.is_module {
                        self.throw_syntax_error("import.meta only valid in module code");
                        return Err(-1);
                    }
                    self.next_token()?;

                    emit_op(self, OpCode::SPECIAL_OBJECT);
                    emit_u8(
                        self,
                        OPSpecialObjectEnum::OP_SPECIAL_OBJECT_IMPORT_META as u8,
                    );
                } else {
                    self.parse_expect(CHAR_BRACKET_L)?;
                    if !accept_lparen {
                        self.throw_syntax_error("invalid use of 'import()'");
                        return Err(-1);
                    }
                    self.parse_assign_expr()?;
                    self.parse_expect(CHAR_BRACKET_R)?;
                    emit_op(self, OpCode::IMPORT);
                }
            }
            _ => {
                let len = self.buf_ptr as usize - self.token.ptr as usize;
                let msg = format!(
                    "unexpected token in expression: '{:.1$?}'",
                    CStr::from_ptr(self.token.ptr.cast()).to_string_lossy(),
                    len
                );
                self.throw_syntax_error(&msg);
                return Err(-1);
            }
        }

        let mut optional_chaining_label = -1;

        loop {
            let mut has_optional_chain: bool = false;

            let mut arg_count: i32;
            let mut drop_count: i32;

            if self.token.val == TOK_QUESTION_MARK_DOT {
                /* optional chaining */
                self.next_token()?;

                has_optional_chain = true;

                if self.token.val == b'(' as i32 && accept_lparen {
                    self.parse_func_call(
                        self.cur_func_ptr,
                        call_type,
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )?;
                    call_type = FuncCallType::FUNC_CALL_NORMAL;
                } else if self.token.val == b'[' as i32 {
                    self.parse_array_access(
                        self.cur_func_ptr,
                        has_optional_chain,
                        &mut optional_chaining_label,
                    )?;
                } else {
                    self.parse_property(
                        self.cur_func_ptr,
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

                call_type = FuncCallType::FUNC_CALL_TEMPLATE;
                let op = self.parse_func_call2(
                    self.cur_func_ptr,
                    call_type,
                    has_optional_chain,
                    &mut optional_chaining_label,
                );
                self.parse_func_call3(
                    self.cur_func_ptr,
                    op,
                    call_type,
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
                call_type = FuncCallType::FUNC_CALL_NORMAL;
            } else if self.token.val == CHAR_BRACKET_L && accept_lparen {
                /* function call */
                self.parse_func_call(
                    self.cur_func_ptr,
                    call_type,
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
                call_type = FuncCallType::FUNC_CALL_NORMAL;
            } else if self.token.val == b'.' as i32 {
                self.next_token()?;
                self.parse_property(
                    self.cur_func_ptr,
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
            } else if self.token.val == b'[' as i32 {
                self.parse_array_access(
                    self.cur_func_ptr,
                    has_optional_chain,
                    &mut optional_chaining_label,
                )?;
            } else {
                break;
            }
        } // loop

        if optional_chaining_label >= 0 {
            emit_label_raw(self, optional_chaining_label);

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

    pub unsafe fn optional_chain_test(
        &mut self,
        poptional_chaining_label: &mut i32,
        drop_count: i32,
    ) {
        if *poptional_chaining_label < 0 {
            *poptional_chaining_label = self.cur_func_mut().add_label().map_or(-1, |x| x as i32);
        }

        // XXX: could be more efficient with a specific opcode
        emit_op(self, OpCode::DUP);
        emit_op(self, OpCode::IS_UNDEFINED_OR_NULL);
        let label_next = emit_goto(self, OpCode::IF_FALSE, -1);
        for _ in 0..drop_count {
            emit_op(self, OpCode::DROP);
        }
        emit_op(self, OpCode::UNDEFINED);
        emit_goto(self, OpCode::GOTO, *poptional_chaining_label);
        emit_label(self, label_next);
    }
}

#[deprecated(note = "use JSParseState::parse_assign_expr() instead")]
pub unsafe fn js_parse_assign_expr(s: *mut JSParseState) -> i32 {
    (*s).parse_assign_expr2(PF_IN_ACCEPTED).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::get_lvalue() instead")]
pub unsafe fn get_lvalue(
    s: *mut JSParseState,
    popcode: *mut i32,
    pscope: *mut i32,
    pname: *mut JSAtom,
    plabel: *mut i32,
    pdepth: *mut i32,
    keep: bool,
    tok: i32,
) -> i32 {
    (*s).get_lvalue(popcode, pscope, pname, plabel, pdepth, keep, tok)
        .map_or(-1, |_| 0)
}

// name has a live reference. 'is_let' is only used with opcode =
// op_scope_get_var which is never generated by get_lvalue().
#[deprecated(note = "use JSParseState::get_lvalue() instead")]
pub unsafe fn put_lvalue(
    s: *mut JSParseState,
    opcode: i32,
    scope: i32,
    name: JSAtom,
    label: i32,
    special: PutLValueEnum,
    is_let: bool,
) {
    (*s).put_lvalue(opcode, scope, name, label, special, is_let);
}

/// cond? a : b
/// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_cond_expr() instead")]
pub unsafe fn js_parse_cond_expr(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_cond_expr(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_coalesce_expr() instead")]
pub unsafe fn js_parse_coalesce_expr(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_coalesce_expr(parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_logical_and_or() instead")]
pub unsafe fn js_parse_logical_and_or(s: *mut JSParseState, op: i32, parse_flags: i32) -> i32 {
    (*s).parse_logical_and_or(op, parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_ARROW_FUNC, PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_expr_binary() instead")]
pub unsafe fn js_parse_expr_binary(s: *mut JSParseState, level: i32, parse_flags: i32) -> i32 {
    (*s).parse_expr_binary(level, parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_ARROW_FUNC, PF_POW_ALLOWED, PF_POW_FORBIDDEN
#[deprecated(note = "use JSParseState::parse_unary() instead")]
pub unsafe fn js_parse_unary(s: *mut JSParseState, mut parse_flags: i32) -> i32 {
    (*s).parse_unary(parse_flags).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_POSTFIX_CALL, PF_ARROW_FUNC
#[deprecated(note = "use JSParseState::parse_postfix_expr() instead")]
pub unsafe fn js_parse_postfix_expr(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_postfix_expr(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::optional_chain_test() instead")]
pub unsafe fn optional_chain_test(
    s: *mut JSParseState,
    poptional_chaining_label: &mut i32,
    drop_count: i32,
) {
    (*s).optional_chain_test(poptional_chaining_label, drop_count);
}

// allowed parse_flags: PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_assign_expr2() instead")]
pub unsafe fn js_parse_assign_expr2(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_assign_expr2(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expr() instead")]
pub unsafe fn js_parse_expr(s: *mut JSParseState) -> i32 {
    (*s).parse_expr2(PF_IN_ACCEPTED).map_or(-1, |_| 0)
}

// allowed parse_flags: PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_expr2() instead")]
pub unsafe fn js_parse_expr2(s: *mut JSParseState, parse_flags: i32) -> i32 {
    (*s).parse_expr2(parse_flags).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_template() instead")]
pub unsafe fn js_parse_template(s: *mut JSParseState, call: bool, argc: *mut i32) -> i32 {
    (*s).parse_template(call, argc).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_left_hand_side_expr() instead")]
pub unsafe fn js_parse_left_hand_side_expr(s: *mut JSParseState) -> i32 {
    (*s).parse_left_hand_side_expr().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expr_parens() instead")]
pub unsafe fn js_parse_expr_paren(s: *mut JSParseState) -> i32 {
    (*s).parse_expr_parens().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_object_literal() instead")]
pub unsafe fn js_parse_object_literal(s: *mut JSParseState) -> i32 {
    (*s).parse_object_literal().map_or(-1, |_| 0)
}
