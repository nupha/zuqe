// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp},
    function::{BlockEnv, FunctionKind, JSVarDefEnum, OPTIMIZE},
    stack_frame::ExecMode,
    utils::{unlikely, write_to_ptr},
};

use super::{
    esm::JSParseExportEnum,
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    state::{JSParseState, SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI},
    tokenize::TokenKind,
};

pub const DECL_MASK_FUNC: i32 = 1 << 0; /* allow normal function declaration */
/* ored with DECL_MASK_FUNC if function declarations are allowed with a label */
pub const DECL_MASK_FUNC_WITH_LABEL: i32 = 1 << 1;
pub const DECL_MASK_OTHER: i32 = 1 << 2; /* all other declarations */
pub const DECL_MASK_ALL: i32 = DECL_MASK_FUNC | DECL_MASK_FUNC_WITH_LABEL | DECL_MASK_OTHER;

enum TodoState {
    HasLet(TokenKind),
    HasExpr,
    ParseFuncVar,
    FuncDeclError,
}

impl<'s> JSParseState<'s> {
    fn parse_block(&mut self) -> Result<TokenKind, i32> {
        self.parse_expect(TokenKind::Char('{'))?;

        if self.token.kind != TokenKind::Char('}') {
            self.push_scope()?;
            while self.token.kind != TokenKind::Char('}') {
                self.parse_stmt_or_decl(DECL_MASK_ALL)?;
            }
            self.pop_scope();
        }

        self.next_token()
    }

    pub(super) fn parse_stmt_or_decl(&mut self, decl_mask: i32) -> Result<(), i32> {
        let mut label_name = Atom::NULL;

        // specific label handling
        // XXX: support multiple labels on loop statements
        if is_label(self) {
            label_name = self.token.atom().unwrap();

            let mut be = self.cur_func().top_break;
            while !be.is_null() {
                if unsafe { (*be).label_name.eq(&label_name) } {
                    self.throw_syntax_error("duplicate label name");
                    return Err(-1);
                }
                be = unsafe { (*be).prev };
            }

            self.next_token()?;
            self.parse_expect(TokenKind::Char(':'))?;

            if !matches!(
                self.token.kind,
                TokenKind::FOR | TokenKind::DO | TokenKind::WHILE
            ) {
                // labelled regular statement
                let label_break = self.cur_func_mut().add_label().map_or(-1, |l| l as i32);
                let mut break_entry = BlockEnv {
                    label_name,
                    label_break,
                    ..Default::default()
                };
                self.cur_func_mut().push_break_entry(&mut break_entry);

                self.parse_stmt_or_decl(
                    if !self.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
                        && (decl_mask & DECL_MASK_FUNC_WITH_LABEL) != 0
                    {
                        DECL_MASK_FUNC | DECL_MASK_FUNC_WITH_LABEL
                    } else {
                        0
                    },
                )?;

                self.emit_label(label_break);

                self.cur_func_mut().pop_break_entry();
                return Ok(());
            }
        }

        let mut todo: Option<TodoState> = None;

        match self.token.kind {
            TokenKind::Char('{') => {
                self.parse_block()?;
            }
            TokenKind::RETURN => {
                let fd = self.cur_func();
                if fd.is_eval {
                    self.throw_syntax_error("return not in a function");
                    return Err(-1);
                } else if fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
                    self.throw_syntax_error("return in a static initializer block");
                    return Err(-1);
                }

                self.next_token()?;

                if !matches!(self.token.kind, TokenKind::Char(';' | '}')) && !self.got_lf {
                    self.parse_expr()?;
                    self.emit_return(true);
                } else {
                    self.emit_return(false);
                }

                self.parse_expect_semi()?;
            }
            TokenKind::THROW => {
                self.next_token()?;
                if self.got_lf {
                    self.throw_syntax_error("line terminator not allowed after throw");
                    return Err(-1);
                }
                self.parse_expr()?;
                self.emit_op(OpCode::THROW);
                self.parse_expect_semi()?;
            }
            t @ (TokenKind::LET | TokenKind::CONST | TokenKind::VAR) => {
                todo.replace(TodoState::HasLet(t));
            }
            TokenKind::IF => {
                stmt_if(self)?;
            }
            TokenKind::WHILE => {
                stmt_while(self, label_name)?;
            }
            TokenKind::DO => {
                stmt_do(self, label_name)?;
            }
            TokenKind::FOR => {
                stmt_for(self, label_name)?;
            }
            t @ (TokenKind::BREAK | TokenKind::CONTINUE) => {
                self.next_token()?;

                // check for continue or break label
                let label = if !self.got_lf
                    && self.token.kind == TokenKind::IDENT
                    && !self.token.data.is_reserved()
                {
                    self.token.atom().unwrap()
                } else {
                    Atom::NULL
                };

                self.emit_break(label, t == TokenKind::CONTINUE)?;

                if !label.is_null() {
                    self.next_token()?;
                }
                self.parse_expect_semi()?;
            }
            TokenKind::SWITCH => {
                stmt_switch(self, label_name)?;
            }
            TokenKind::TRY => {
                stmt_try(self)?;
            }
            TokenKind::Char(';') => {
                // empty statement
                self.next_token()?;
            }
            TokenKind::WITH => {
                stmt_with(self)?;
            }
            TokenKind::FUNCTION => {
                // ES6 Annex B.3.2 and B.3.3 semantics
                todo.replace(
                    if decl_mask & DECL_MASK_FUNC == 0
                        || (decl_mask & DECL_MASK_OTHER == 0
                            && self.peek_token(false) == TokenKind::Char('*'))
                    {
                        TodoState::FuncDeclError
                    } else {
                        TodoState::ParseFuncVar
                    },
                );
            }
            TokenKind::IDENT => {
                todo.replace(stmt_ident(self, decl_mask)?);
            }
            TokenKind::CLASS => {
                if decl_mask & DECL_MASK_OTHER == 0 {
                    self.throw_syntax_error(
                        "class declarations can't appear in single-statement context",
                    );
                    return Err(-1);
                }
                self.parse_class(false, JSParseExportEnum::JS_PARSE_EXPORT_NONE)?;
            }
            TokenKind::DEBUGGER => {
                // currently no debugger, so just skip the keyword
                self.next_token()?;
                self.parse_expect_semi()?;
            }
            t @ (TokenKind::ENUM | TokenKind::EXPORT | TokenKind::EXTENDS) => {
                let a = t.keyword_atom().unwrap();
                js_unsupported_keyword(self, a);
                return Err(-1);
            }
            _ => {
                todo.replace(TodoState::HasExpr);
            }
        }

        while let Some(st) = todo.take() {
            match st {
                TodoState::HasLet(t) => {
                    stmt_let_const_var(self, t, decl_mask)?;
                }
                TodoState::HasExpr => {
                    hasexpr(self)?;
                }
                TodoState::ParseFuncVar => {
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_VAR,
                        FunctionKind::Normal,
                        self.token.ptr,
                        self.token.line_num,
                    )?;
                }
                TodoState::FuncDeclError => {
                    self.throw_syntax_error(
                        "function declarations can't appear in single-statement context",
                    );
                    return Err(-1);
                }
            }
        }

        Ok(())
    }

    #[inline(always)]
    fn parse_statement(&mut self) -> Result<(), i32> {
        self.parse_stmt_or_decl(0)
    }

    fn set_eval_ret_undefined(&mut self) {
        if let Some(vidx) = self.cur_func().eval_ret_idx {
            self.emit_op(OpCode::UNDEFINED);
            self.emit_op(OpCode::PUT_LOC);
            self.emit_u16(vidx);
        }
    }
}

#[inline(always)]
fn js_unsupported_keyword(s: &mut JSParseState, atom: Atom) -> i32 {
    s.throw_syntax_error(&format!(
        "unsupported keyword: {}",
        atom.to_utf8(s.ctx.rt_ref())
    ));
    -1
}

// test if the current token is a label. Use simplistic look-ahead scanner
fn is_label(s: &JSParseState) -> bool {
    s.token.kind == TokenKind::IDENT
        && !s.token.data.is_reserved()
        && s.peek_token(false) == TokenKind::Char(':')
}

// test if the current token is a let keyword. Use simplistic look-ahead scanner
fn is_let(s: &mut JSParseState, decl_mask: i32) -> Result<bool, i32> {
    let mut res = Ok(false);

    if s.token_is_pseudo_keyword(Atom::CONST_let) {
        let pos = s.get_pos();

        s.next_token()?;
        let tok = s.token.kind;

        if tok == TokenKind::Char('[') {
            // let [ is a syntax restriction:
            // it never introduces an ExpressionStatement
            res = Ok(true);
        } else if tok == TokenKind::Char('{')
            || (tok == TokenKind::IDENT && !s.token.data.is_reserved())
            || matches!(tok, TokenKind::LET | TokenKind::YIELD | TokenKind::AWAIT)
        {
            // Check for possible ASI if not scanning for Declaration
            // XXX: should also check that `{` introduces a BindingPattern,
            // but Firefox does not and rejects eval("let=1;let\n{if(1)2;}")
            if s.last_line_num == s.token.line_num || (decl_mask & DECL_MASK_OTHER) != 0 {
                res = Ok(true);
            }
        }

        s.seek_token(&pos)?;

        // experimental?
        // {
        //     let tok = peek_token(s, TRUE);
        //     if tok == b'{' as i32 || tok == TOK_IDENT || peek_token(s, FALSE) == b'[' as i32 {
        //         res = TRUE;
        //     }
        // }
    }

    res
}

// #[deprecated(note = "use JSParseState::set_eval_ret_undefined() instead")]
// pub fn set_eval_ret_undefined(s: &mut JSParseState) {
//     s.set_eval_ret_undefined();
// }

// #[deprecated(note = "use JSParseState::parse_block() instead")]
// pub unsafe fn js_parse_block(s: &mut JSParseState) -> Result<(), i32> {
//     s.parse_block()
// }

// #[deprecated(note = "use JSParseState::parse_statement() instead")]
// pub unsafe fn js_parse_statement(s: &mut JSParseState) -> Result<(), i32> {
//     s.parse_statement()
// }

// #[inline(always)]
// unsafe fn stmt_return(s: &mut JSParseState) -> Result<(), i32> {
//     s.next_token()?;

//     if s.got_lf {
//         s.throw_syntax_error("line terminator not allowed after throw");
//         return Err(-1);
//     }
//     if s.parse_expr().is_err() {
//         return Err(-1);
//     }
//     s.emit_op(OpCode::THROW);
//     if s.parse_expect_semi().is_err() {
//         return Err(-1);
//     }

//     Ok(())
// }

fn stmt_do(s: &mut JSParseState, label_name: Atom) -> Result<(), i32> {
    let (label_cont, label_break, label1) = {
        let fd = s.cur_func_mut();
        (
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
        )
    };

    let mut break_entry = BlockEnv {
        label_name,
        label_break,
        label_cont,
        ..Default::default()
    };
    s.cur_func_mut().push_break_entry(&mut break_entry);

    s.next_token()?;
    s.emit_label(label1);
    s.set_eval_ret_undefined();
    s.parse_statement()?;

    s.emit_label(label_cont);

    s.parse_expect(TokenKind::WHILE)?;
    s.parse_expr_parens()?;

    // Insert semicolon if missing
    if s.token.kind == TokenKind::Char(';') {
        s.next_token()?;
    }
    s.emit_goto(OpCode::IF_TRUE, label1);
    s.emit_label(label_break);

    s.cur_func_mut().pop_break_entry();
    Ok(())
}

fn stmt_for(s: &mut JSParseState, label_name: Atom) -> Result<(), i32> {
    s.next_token()?;
    s.set_eval_ret_undefined();

    let mut bits = 0;
    let mut is_async = false;

    if s.token.kind == TokenKind::Char('(') {
        s.parse_skip_parens_token(&mut bits, false)?;
    } else if s.token.kind == TokenKind::AWAIT {
        if !matches!(
            s.cur_func().attrs.func_kind,
            FunctionKind::Async | FunctionKind::AsyncGenerator
        ) {
            s.throw_syntax_error("for await is only valid in asynchronous functions");
            return Err(-1);
        }
        is_async = true;
        s.next_token()?;
        s.cur_func_mut().has_await = true;
    }

    s.parse_expect(TokenKind::Char('('))?;

    if (bits & SKIP_HAS_SEMI) == 0 {
        // parse for/in or for/of
        return parse_for_in_of(s, label_name, is_async);
    }

    let block_scope_level = s.cur_func().scope_level;

    // create scope for the lexical variables declared in the initial,
    // test and increment expressions
    s.push_scope()?;

    // initial expression
    let mut tok = s.token.kind;
    if tok != TokenKind::Char(';') {
        if is_let(s, DECL_MASK_OTHER)? {
            tok = TokenKind::LET;
        }

        if matches!(tok, TokenKind::VAR | TokenKind::LET | TokenKind::CONST) {
            s.next_token()?;
            s.parse_var(0, tok, false)?;
        } else {
            s.parse_expr2(0)?;
            s.emit_op(OpCode::DROP);
        }

        // close the closures before the first iteration
        let scope_level = s.cur_func().scope_level;
        s.close_scopes(scope_level, block_scope_level);
    }

    s.parse_expect(TokenKind::Char(';'))?;

    let (mut label_test, mut label_cont, label_body, label_break) = {
        let fd = s.cur_func_mut();
        (
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
        )
    };

    // push break entry: be is allocated on stack
    let mut break_entry = BlockEnv {
        label_name,
        label_break,
        label_cont,
        ..Default::default()
    };
    s.cur_func_mut().push_break_entry(&mut break_entry);

    // test expression
    if s.token.kind == TokenKind::Char(';') {
        // no test expression
        label_test = label_body;
    } else {
        s.emit_label(label_test);
        s.parse_expr()?;
        s.emit_goto(OpCode::IF_FALSE, label_break);
    }

    s.parse_expect(TokenKind::Char(';'))?;

    let pos_cont = if s.token.kind == TokenKind::Char(')') {
        // no end expression
        label_cont = label_test;
        break_entry.label_cont = label_cont;
        0 // avoid warning
    } else {
        // skip the end expression
        s.emit_goto(OpCode::GOTO, label_body);

        let p = s.cur_func().byte_code.len();
        s.emit_label(label_cont);

        s.parse_expr()?;

        s.emit_op(OpCode::DROP);
        if label_test != label_body {
            s.emit_goto(OpCode::GOTO, label_test);
        }

        p
    };

    s.parse_expect(TokenKind::Char(')'))?;

    let pos_body = s.cur_func().byte_code.len();
    s.emit_label(label_body);
    s.parse_statement()?;

    // close the closures before the next iteration
    // XXX: check continue case
    let scope_level = s.cur_func().scope_level;
    s.close_scopes(scope_level, block_scope_level);

    if OPTIMIZE && label_test != label_body && label_cont != label_test {
        // move the increment code here
        let bc = &mut s.cur_func_mut().byte_code;
        let chunk_size = pos_body - pos_cont;
        let offset = bc.len() - pos_cont;

        bc.extend(std::iter::repeat_n(OpCode::NOP as u8, chunk_size));
        for i in (pos_cont..pos_body).rev() {
            bc[i + offset] = bc[i];
        }
        for item in bc.iter_mut().take(pos_body).skip(pos_cont) {
            *item = OpCode::NOP as u8;
        }

        // increment part ends with a goto
        s.cur_func_mut().last_opcode_pos = bc.len() as i32 - 5;

        // relocate labels
        for lb in &mut s.cur_func_mut().label_slots[label_cont as usize..] {
            if lb.pos >= pos_cont as i32 && lb.pos < pos_body as i32 {
                lb.pos += offset as i32;
            }
        }
    } else {
        s.emit_goto(OpCode::GOTO, label_cont);
    }

    s.emit_label(label_break);

    s.cur_func_mut().pop_break_entry();
    s.pop_scope();
    Ok(())
}

fn stmt_while(s: &mut JSParseState, label_name: Atom) -> Result<(), i32> {
    let (label_cont, label_break) = {
        let fd = s.cur_func_mut();
        (fd.add_label()? as i32, fd.add_label()? as i32)
    };
    let mut break_entry = BlockEnv {
        label_name,
        label_break,
        label_cont,
        ..Default::default()
    };
    s.cur_func_mut().push_break_entry(&mut break_entry);

    s.next_token()?;
    s.set_eval_ret_undefined();
    s.emit_label(label_cont);
    s.parse_expr_parens()?;

    s.emit_goto(OpCode::IF_FALSE, label_break);
    s.parse_statement()?;
    s.emit_goto(OpCode::GOTO, label_cont);
    s.emit_label(label_break);

    s.cur_func_mut().pop_break_entry();
    Ok(())
}

fn stmt_if(s: &mut JSParseState) -> Result<(), i32> {
    s.next_token()?;

    // create a new scope for `let f;if(1) function f(){}`
    s.push_scope()?;
    s.set_eval_ret_undefined();
    s.parse_expr_parens()?;

    let mut label1 = s.emit_goto(OpCode::IF_FALSE, -1);
    let mask = if s.cur_func().attrs.exec_mode.contains(ExecMode::STRICT) {
        0
    } else {
        DECL_MASK_FUNC // Annex B.3.4
    };

    s.parse_stmt_or_decl(mask)?;

    if s.token.kind == TokenKind::ELSE {
        let label2 = s.emit_goto(OpCode::GOTO, -1);
        s.next_token()?;

        s.emit_label(label1);
        s.parse_stmt_or_decl(mask)?;

        label1 = label2;
    }
    s.emit_label(label1);

    s.pop_scope();
    Ok(())
}

fn stmt_try(s: &mut JSParseState) -> Result<(), i32> {
    s.set_eval_ret_undefined();
    s.next_token()?;

    let (label_catch, label_catch2, label_finally, label_end) = {
        let fd = s.cur_func_mut();
        (
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
        )
    };

    s.emit_goto(OpCode::CATCH, label_catch);

    {
        let mut block_env = BlockEnv {
            label_finally,
            drop_count: 1,
            ..Default::default()
        };
        s.cur_func_mut().push_break_entry(&mut block_env);
        s.parse_block()?;
        s.cur_func_mut().pop_break_entry();
    }

    if s.cur_func().is_live_code() {
        // drop the catch offset
        s.emit_op(OpCode::DROP);
        // must push dummy value to keep same stack size
        s.emit_op(OpCode::UNDEFINED);
        s.emit_goto(OpCode::GOSUB, label_finally);
        s.emit_op(OpCode::DROP);
        s.emit_goto(OpCode::GOTO, label_end);
    }

    if s.token.kind == TokenKind::CATCH {
        s.next_token()?;
        s.push_scope()?; // catch variable
        s.emit_label(label_catch);

        if s.token.kind == TokenKind::Char('{') {
            // support optional-catch-binding feature
            s.emit_op(OpCode::DROP); // pop the exception object
        } else {
            s.parse_expect(TokenKind::Char('('))?;

            if s.token.kind != TokenKind::IDENT || s.token.data.is_reserved() {
                if matches!(s.token.kind, TokenKind::Char('[') | TokenKind::Char('{')) {
                    // XXX: LET is not completely correct
                    s.parse_destructuring_element(TokenKind::LET, false, true, -1, true, false)?;
                } else {
                    s.throw_syntax_error("identifier expected");
                    return Err(-1);
                }
            } else {
                // must dup the atom because later next_token() will free it
                let name = s.token.atom().unwrap();
                s.next_token()?;

                s.define_var(name, TokenKind::CATCH, false)?;

                // store the exception value in the catch variable
                s.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR);
                s.emit_atom(name);
                s.emit_u16(s.cur_func().scope_level as _);
            }

            s.parse_expect(TokenKind::Char(')'))?;
        }

        // XXX: should keep the address to nop it out if there is no finally block
        s.emit_goto(OpCode::CATCH, label_catch2);

        s.push_scope()?; // catch block

        {
            let mut be = BlockEnv {
                label_finally,
                drop_count: 1,
                ..Default::default()
            };
            s.cur_func_mut().push_break_entry(&mut be);
            s.parse_block()?;
            s.cur_func_mut().pop_break_entry();
        }

        s.pop_scope(); // catch block
        s.pop_scope(); // catch variable

        if s.cur_func().is_live_code() {
            // drop the catch2 offset
            s.emit_op(OpCode::DROP);
            // XXX: should keep the address to nop it out if there is no finally block
            // must push dummy value to keep same stack size
            s.emit_op(OpCode::UNDEFINED);
            s.emit_goto(OpCode::GOSUB, label_finally);
            s.emit_op(OpCode::DROP);
            s.emit_goto(OpCode::GOTO, label_end);
        }
        // catch exceptions thrown in the catch block to execute the
        // finally clause and rethrow the exception
        s.emit_label(label_catch2);
        // catch value is at TOS, no need to push undefined
        s.emit_goto(OpCode::GOSUB, label_finally);
        s.emit_op(OpCode::THROW);
    } else if s.token.kind == TokenKind::FINALLY {
        // finally without catch : execute the finally clause
        // and rethrow the exception
        s.emit_label(label_catch);
        // catch value is at TOS, no need to push undefined
        s.emit_goto(OpCode::GOSUB, label_finally);
        s.emit_op(OpCode::THROW);
    } else {
        s.throw_syntax_error("expecting catch or finally");
        return Err(-1);
    }

    s.emit_label(label_finally);

    if s.token.kind == TokenKind::FINALLY {
        s.next_token()?;

        // on the stack: ret_value gosub_ret_value
        let mut be = BlockEnv {
            drop_count: 2,
            ..Default::default()
        };
        s.cur_func_mut().push_break_entry(&mut be);

        let mut saved_eval_ret_idx: u16 = 0;

        if let Some(vidx) = s.cur_func().eval_ret_idx {
            // 'finally' updates eval_ret only if not a normal termination
            saved_eval_ret_idx = s.cur_func_mut().add_var(s.ctx, Atom::CONST__ret_)?;

            s.emit_op(OpCode::GET_LOC);
            s.emit_u16(vidx);
            s.emit_op(OpCode::PUT_LOC);
            s.emit_u16(saved_eval_ret_idx as _);
            s.set_eval_ret_undefined();
        }

        s.parse_block()?;

        if let Some(vidx) = s.cur_func().eval_ret_idx {
            s.emit_op(OpCode::GET_LOC);
            s.emit_u16(saved_eval_ret_idx);
            s.emit_op(OpCode::PUT_LOC);
            s.emit_u16(vidx);
        }

        s.cur_func_mut().pop_break_entry();
    }

    s.emit_op(OpCode::RET);
    s.emit_label(label_end);

    Ok(())
}

#[inline(always)]
fn stmt_let_const_var(s: &mut JSParseState, tok: TokenKind, decl_mask: i32) -> Result<(), i32> {
    if matches!(tok, TokenKind::LET | TokenKind::CONST) && (decl_mask & DECL_MASK_OTHER) == 0 {
        s.throw_syntax_error("lexical declarations can't appear in single-statement context");
        return Err(-1);
    }

    s.next_token()?;
    s.parse_var(1, tok, false)?;
    s.parse_expect_semi()?;

    Ok(())
}

#[inline(always)]
fn stmt_switch(s: &mut JSParseState, label_name: Atom) -> Result<(), i32> {
    s.next_token()?;
    s.set_eval_ret_undefined();
    s.parse_expr_parens()?;

    s.push_scope()?;

    let label_break = s.cur_func_mut().add_label()? as i32;
    let mut break_entry = BlockEnv {
        label_name,
        label_break,
        drop_count: 1,
        ..Default::default()
    };
    s.cur_func_mut().push_break_entry(&mut break_entry);

    s.parse_expect(TokenKind::Char('{'))?;

    let mut default_label_pos = -1;
    let mut label_case = -1;
    let mut label1: i32;

    while s.token.kind != TokenKind::Char('}') {
        if s.token.kind == TokenKind::CASE {
            label1 = if label_case >= 0 {
                // skip the case if needed
                s.emit_goto(OpCode::GOTO, -1)
            } else {
                -1
            };
            s.emit_label(label_case);

            loop {
                // parse a sequence of case clauses
                s.next_token()?;

                s.emit_op(OpCode::DUP);
                s.parse_expr()?;
                s.parse_expect(TokenKind::Char(':'))?;

                s.emit_op(OpCode::STRICT_EQ);
                if s.token.kind == TokenKind::CASE {
                    label1 = s.emit_goto(OpCode::IF_TRUE, label1);
                } else {
                    label_case = s.emit_goto(OpCode::IF_FALSE, -1);
                    s.emit_label(label1);
                    break;
                }
            }
        } else if s.token.kind == TokenKind::DEFAULT {
            s.next_token()?;
            s.parse_expect(TokenKind::Char(':'))?;

            if default_label_pos >= 0 {
                s.throw_syntax_error("duplicate default");
                return Err(-1);
            }
            if label_case < 0 {
                // falling thru direct from switch expression
                label_case = s.emit_goto(OpCode::GOTO, -1);
            }
            // Emit a dummy label opcode. Label will be patched after
            // the end of the switch body. Do not use emit_label(s, 0)
            // because it would clobber label 0 address, preventing
            // proper optimizer operation.
            s.emit_op_temp(OpCodeTemp::LABEL);
            s.emit_u32(0);
            default_label_pos = s.cur_func().byte_code.len() as i32 - 4;
        } else {
            if label_case < 0 {
                // falling thru direct from switch expression
                s.throw_syntax_error("invalid switch statement");
                return Err(-1);
            }
            s.parse_stmt_or_decl(DECL_MASK_ALL)?;
        }
    }

    s.parse_expect(TokenKind::Char('}'))?;

    if default_label_pos >= 0 {
        // Ugly patch for the the `default` label, shameful and risky
        let fd = s.cur_func_mut();
        unsafe {
            write_to_ptr::<u32>(
                fd.byte_code.as_mut_ptr().add(default_label_pos as usize),
                label_case as u32,
            );
        }
        fd.label_slots[label_case as usize].pos = default_label_pos + 4;
    } else {
        s.emit_label(label_case);
    }
    s.emit_label(label_break);
    s.emit_op(OpCode::DROP); // drop the switch expression

    s.cur_func_mut().pop_break_entry();
    s.pop_scope();
    Ok(())
}

#[inline(always)]
fn stmt_with(s: &mut JSParseState) -> Result<(), i32> {
    if s.cur_func().attrs.exec_mode.contains(ExecMode::STRICT) {
        s.throw_syntax_error("invalid keyword: with");
        Err(-1)
    } else {
        s.next_token()?;
        s.parse_expr_parens()?;

        s.push_scope()?;

        let with_idx = s.define_var_cur_func(Atom::CONST__with_, JSVarDefEnum::JS_VAR_DEF_WITH)?;

        s.emit_op(OpCode::TO_OBJECT);
        s.emit_op(OpCode::PUT_LOC);
        s.emit_u16(with_idx as _);

        s.set_eval_ret_undefined();
        s.parse_statement()?;

        // Popping scope drops lexical context for the with object variable
        s.pop_scope();

        Ok(())
    }
}

fn stmt_ident(s: &mut JSParseState, decl_mask: i32) -> Result<TodoState, i32> {
    if s.token.data.is_reserved() {
        s.throw_reserved_identifier_error();
        return Err(-1);
    }
    // Determine if `let` introduces a Declaration or an ExpressionStatement
    if is_let(s, decl_mask)? {
        return Ok(TodoState::HasLet(TokenKind::LET));
    }
    if s.token_is_pseudo_keyword(Atom::CONST_async) && s.peek_token(true) == TokenKind::FUNCTION {
        if decl_mask & DECL_MASK_OTHER == 0 {
            s.throw_syntax_error("function declarations can't appear in single-statement context");
            return Err(-1);
        }
        return Ok(TodoState::ParseFuncVar);
    }

    Ok(TodoState::HasExpr)
}

fn hasexpr(s: &mut JSParseState) -> Result<(), i32> {
    s.parse_expr()?;

    if let Some(vidx) = s.cur_func().eval_ret_idx {
        // store the expression value so that it can be returned by eval()
        s.emit_op(OpCode::PUT_LOC);
        s.emit_u16(vidx);
    } else {
        // drop the result
        s.emit_op(OpCode::DROP);
    }

    s.parse_expect_semi()?;
    Ok(())
}

// XXX: handle IteratorClose when exiting the loop before the
// enumeration is done
fn parse_for_in_of(s: &mut JSParseState, label_name: Atom, is_async: bool) -> Result<(), i32> {
    // save current scope label
    let block_scope_level = s.cur_func().scope_level;

    // create 4 labels
    let (label_cont, label_body, label_break, label_next) = {
        let fd = s.cur_func_mut();
        (
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
        )
    };

    // create scope for the lexical variables declared in the enumeration
    // expressions. XXX: Not completely correct because of weird capturing
    // semantics in `for (i of o) a.push(function(){return i})`
    s.push_scope()?;

    // local for_in scope starts here so individual elements
    // can be closed in statement.
    let mut break_entry = BlockEnv {
        label_name,
        label_break,
        label_cont,
        drop_count: 1,
        ..Default::default()
    };
    s.cur_func_mut().push_break_entry(&mut break_entry);
    break_entry.scope_level = block_scope_level;

    let label_expr = s.emit_goto(OpCode::GOTO, -1);
    let pos_next = s.cur_func().byte_code.len();
    s.emit_label(label_next);

    let tok = if is_let(s, DECL_MASK_OTHER)? {
        TokenKind::LET
    } else {
        s.token.kind
    };

    let var_name: Atom;
    let mut has_destructuring = false;

    if matches!(tok, TokenKind::VAR | TokenKind::LET | TokenKind::CONST) {
        s.next_token()?;

        if s.token.kind != TokenKind::IDENT || s.token.data.is_reserved() {
            if matches!(s.token.kind, TokenKind::Char('[' | '{')) {
                s.parse_destructuring_element(tok, false, true, -1, false, false)?;
                has_destructuring = true;
            } else {
                s.throw_syntax_error("variable name expected");
                return Err(-1);
            }
            var_name = Atom::NULL;
        } else {
            var_name = s.token.atom().unwrap();
            s.next_token()?;

            s.define_var(var_name, tok, false)?;
            s.emit_op_temp(if matches!(tok, TokenKind::CONST | TokenKind::LET) {
                OpCodeTemp::SCOPE_PUT_VAR_INIT
            } else {
                OpCodeTemp::SCOPE_PUT_VAR
            });
            s.emit_atom(var_name);
            s.emit_u16(s.cur_func().scope_level as _);
        }
    } else if unlikely(
        !is_async
            && s.token_is_pseudo_keyword(Atom::CONST_async)
            && s.peek_token(false) == TokenKind::OF,
    ) {
        s.throw_syntax_error("'for of' expression cannot start with 'async'");
        return Err(-1);
    } else {
        let mut skip_bits: i32 = 0;
        if matches!(s.token.kind, TokenKind::Char('[' | '{'))
            && matches!(
                s.parse_skip_parens_token(&mut skip_bits, false)?,
                TokenKind::IN | TokenKind::OF
            )
        {
            s.parse_destructuring_element(
                TokenKind::Char('\0'),
                false,
                true,
                skip_bits & SKIP_HAS_ELLIPSIS,
                true,
                false,
            )?;
        } else {
            s.parse_left_hand_side_expr()?;

            let mut lvalue_label: i32 = 0;
            let mut opcode: i32 = 0;
            let mut scope: i32 = 0;
            let mut name = Atom::NULL;

            s.get_lvalue(
                &mut opcode,
                &mut scope,
                &mut name,
                &mut lvalue_label,
                std::ptr::null_mut(),
                false,
                TokenKind::FOR,
            )?;
            s.put_lvalue(
                opcode,
                scope,
                name,
                lvalue_label,
                PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM,
                false,
            );
        }

        var_name = Atom::NULL;
    }

    s.emit_goto(OpCode::GOTO, label_body);

    let pos_expr = s.cur_func().byte_code.len();
    s.emit_label(label_expr);

    let mut has_initializer = false;

    if s.token.kind == TokenKind::Char('=') {
        // XXX: potential scoping issue if inside `with` statement
        has_initializer = true;
        // parse and evaluate initializer prior to evaluating the
        // object (only used with "for in" with a non lexical variable
        // in non strict mode)
        s.next_token()?;
        s.parse_assign_expr2(0)?;

        if !var_name.is_null() {
            s.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR);
            s.emit_atom(var_name);
            s.emit_u16(s.cur_func().scope_level as _);
        }
    }

    let mut is_for_of = false;

    if s.token_is_pseudo_keyword(Atom::CONST_of) {
        // for...of
        is_for_of = true;
        break_entry.has_iterator = true;
        break_entry.drop_count += 2;
        if has_initializer {
            s.throw_syntax_error(&format!(
                "a declaration in the head of a for-{} loop can't have an initializer",
                if is_for_of { "of" } else { "in" }
            ));
            return Err(-1);
        }
    } else if s.token.kind == TokenKind::IN {
        if is_async {
            s.throw_syntax_error("'for await' loop should be used with 'of'");
            return Err(-1);
        }
        if has_initializer
            && (tok != TokenKind::VAR
                || s.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
                || has_destructuring)
        {
            s.throw_syntax_error(&format!(
                "a declaration in the head of a for-{} loop can't have an initializer",
                if is_for_of { "of" } else { "in" }
            ));
            return Err(-1);
        }
    } else {
        s.throw_syntax_error("expected 'of' or 'in' in for control expression");
        return Err(-1);
    }

    s.next_token()?;

    if is_for_of {
        s.parse_assign_expr()?;
    } else {
        s.parse_expr()?;
    }

    // close the scope after having evaluated the expression so that
    // the TDZ values are in the closures
    {
        let scope_level = s.cur_func().scope_level;
        s.close_scopes(scope_level, block_scope_level);
    }

    s.emit_op(if is_for_of {
        if is_async {
            OpCode::FOR_AWAIT_OF_START
        } else {
            OpCode::FOR_OF_START
        }
        // on stack: enum_rec
    } else {
        OpCode::FOR_IN_START
        // on stack: enum_obj
    });
    s.emit_goto(OpCode::GOTO, label_cont);

    s.parse_expect(TokenKind::Char(')'))?;

    if OPTIMIZE {
        let cur_func = s.cur_func_mut();

        // move the `next` code here
        let bc = &mut cur_func.byte_code;
        let chunk_size = pos_expr - pos_next;
        let offset = bc.len() - pos_next;

        // extend bytecode buf
        bc.extend(std::iter::repeat_n(OpCode::NOP as u8, chunk_size));

        // align bytes ranges pos_next..pos_expr to buf bottom, fill gap with NOP
        for i in (pos_next..pos_expr).rev() {
            bc[i + offset] = bc[i];
            bc[i] = OpCode::NOP as u8;
        }

        // `next` part ends with a goto
        cur_func.last_opcode_pos = bc.len() as i32 - 5;

        // relocate labels
        for lb in &mut cur_func.label_slots[label_cont as usize..] {
            if lb.pos >= pos_next as i32 && lb.pos < pos_expr as i32 {
                lb.pos += offset as i32;
            }
        }
    }

    s.emit_label(label_body);
    s.parse_statement()?;

    let scope_level = s.cur_func().scope_level;
    s.close_scopes(scope_level, block_scope_level);

    s.emit_label(label_cont);
    if is_for_of {
        if is_async {
            // call the next method
            // stack: iter_obj next catch_offset
            s.emit_op(OpCode::DUP3);
            s.emit_op(OpCode::DROP);
            s.emit_op(OpCode::CALL_METHOD);
            s.emit_u16(0);
            // get the result of the promise
            s.emit_op(OpCode::AWAIT);
            // unwrap the value and done values
            s.emit_op(OpCode::ITERATOR_GET_VALUE_DONE);
        } else {
            s.emit_op(OpCode::FOR_OF_NEXT);
            s.emit_u8(0);
        }
    } else {
        s.emit_op(OpCode::FOR_IN_NEXT);
    }
    // on stack: enum_rec / enum_obj value bool
    s.emit_goto(OpCode::IF_FALSE, label_next);
    // drop the undefined value from for_xx_next
    s.emit_op(OpCode::DROP);

    s.emit_label(label_break);
    s.emit_op(if is_for_of {
        OpCode::ITERATOR_CLOSE // close and drop enum_rec
    } else {
        OpCode::DROP
    });

    s.cur_func_mut().pop_break_entry();
    s.pop_scope();
    Ok(())
}
