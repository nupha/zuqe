use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, consts::*},
    bytecode::{OpCode, OpCodeTemp},
    context::JS_MODE_STRICT,
    function::{
        BlockEnv, JSFunctionKindEnum, JSVarDefEnum, OPTIMIZE, pop_break_entry, push_break_entry,
    },
    parse::emit::emit_return,
    utils::write_to_ptr,
};

use super::{
    class::js_parse_class,
    destructing::js_parse_destructuring_element,
    emit::{
        emit_atom_dup, emit_goto, emit_label, emit_op, emit_op_temp, emit_u16, js_is_live_code,
    },
    esm::JSParseExportEnum,
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    parse::js_parse_skip_parens_token,
    state::{JSParseState, SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI},
    token::{
        CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_COLON, CHAR_CURLY_L, CHAR_CURLY_R, CHAR_EQUAL,
        CHAR_SEMI, CHAR_SQUARE_L, TOK_AWAIT, TOK_BREAK, TOK_CASE, TOK_CATCH, TOK_CLASS, TOK_CONST,
        TOK_CONTINUE, TOK_DEBUGGER, TOK_DEFAULT, TOK_DO, TOK_ELSE, TOK_ENUM, TOK_EXPORT,
        TOK_EXTENDS, TOK_FINALLY, TOK_FOR, TOK_FUNCTION, TOK_IDENT, TOK_IF, TOK_IN, TOK_LET,
        TOK_OF, TOK_RETURN, TOK_SWITCH, TOK_THROW, TOK_TRY, TOK_VAR, TOK_WHILE, TOK_WITH,
        TOK_YIELD,
    },
};

pub const DECL_MASK_FUNC: i32 = 1 << 0; /* allow normal function declaration */
/* ored with DECL_MASK_FUNC if function declarations are allowed with a label */
pub const DECL_MASK_FUNC_WITH_LABEL: i32 = 1 << 1;
pub const DECL_MASK_OTHER: i32 = 1 << 2; /* all other declarations */
pub const DECL_MASK_ALL: i32 = DECL_MASK_FUNC | DECL_MASK_FUNC_WITH_LABEL | DECL_MASK_OTHER;

enum StmtState {
    HasLet(i32),
    HasExpr,
    ParseFuncVar,
    FuncDeclError,
}

pub unsafe fn js_parse_statement_or_decl(s: &mut JSParseState, decl_mask: i32) -> i32 {
    let mut label_name: JSAtom = JS_ATOM_NULL;

    macro_rules! fail {
        () => {
            JS_FreeAtom(&s.ctx, label_name);
            return -1;
        };
    }

    // specific label handling
    // XXX: support multiple labels on loop statements
    if is_label(s) {
        label_name = s.token.data.ident_atom().unwrap().dup(&s.ctx.rt());

        let mut be = s.cur_func().top_break;
        while !be.is_null() {
            if (*be).label_name == label_name {
                s.throw_syntax_error("duplicate label name");
                fail!();
            }
            be = (*be).prev;
        }

        if s.next_token().is_err() {
            fail!();
        }
        if s.parse_expect(CHAR_COLON).is_err() {
            fail!();
        }

        if !matches!(s.token.val, TOK_FOR | TOK_DO | TOK_WHILE) {
            // labelled regular statement
            let mut break_entry: BlockEnv = std::mem::zeroed();
            let label_break = s.cur_func_mut().add_label().map_or(-1, |l| l as i32);
            push_break_entry(
                s.cur_func_mut(),
                &mut break_entry,
                label_name,
                label_break,
                -1,
                0,
            );

            let mask = if s.cur_func().js_mode & JS_MODE_STRICT as u8 == 0
                && (decl_mask & DECL_MASK_FUNC_WITH_LABEL) != 0
            {
                DECL_MASK_FUNC | DECL_MASK_FUNC_WITH_LABEL
            } else {
                0
            };
            if js_parse_statement_or_decl(s, mask) != 0 {
                fail!();
            }
            emit_label(s, label_break);

            pop_break_entry(s.cur_func_mut());

            // done
            JS_FreeAtom(&s.ctx, label_name);
            return 0;
        }
    }

    let mut state: Option<StmtState> = None;

    match s.token.val {
        // '{'
        CHAR_CURLY_L => {
            if js_parse_block(s).is_err() {
                fail!();
            }
        }
        TOK_RETURN => {
            let fd = s.cur_func();
            if fd.is_eval {
                s.throw_syntax_error("return not in a function");
                fail!();
            }
            if fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
                s.throw_syntax_error("return in a static initializer block");
                fail!();
            }
            if s.next_token().is_err() {
                fail!();
            }

            if !matches!(s.token.val, CHAR_SEMI | CHAR_CURLY_R) && !s.got_lf {
                if s.parse_expr().is_err() {
                    fail!();
                }
                emit_return(s, true);
            } else {
                emit_return(s, false);
            }
            if s.parse_expect_semi().is_err() {
                fail!();
            }
        }
        TOK_THROW => {
            if s.next_token().is_err() {
                fail!();
            }
            if s.got_lf {
                s.throw_syntax_error("line terminator not allowed after throw");
                fail!();
            }
            if s.parse_expr().is_err() {
                fail!();
            }
            emit_op(s, OpCode::THROW);
            if s.parse_expect_semi().is_err() {
                fail!();
            }
        }
        tok @ (TOK_LET | TOK_CONST | TOK_VAR) => {
            state.replace(StmtState::HasLet(tok));
        }
        TOK_IF => {
            if stmt_if(s).is_err() {
                fail!();
            }
        }
        TOK_WHILE => {
            if stmt_while(s, label_name).is_err() {
                fail!();
            }
        }
        TOK_DO => {
            if stmt_do(s, label_name).is_err() {
                fail!();
            }
        }
        TOK_FOR => {
            if stmt_for(s, label_name).is_err() {
                fail!();
            }
        }
        tok @ (TOK_BREAK | TOK_CONTINUE) => {
            if s.next_token().is_err() {
                fail!();
            }
            let label = if !s.got_lf && s.token.val == TOK_IDENT && !s.token.data.is_reserved() {
                s.token.data.ident_atom().unwrap()
            } else {
                JS_ATOM_NULL
            };
            if s.emit_break(label, tok == TOK_CONTINUE) != 0 {
                fail!();
            }
            if label != JS_ATOM_NULL {
                if s.next_token().is_err() {
                    fail!();
                }
            }
            if s.parse_expect_semi().is_err() {
                fail!();
            }
        }
        TOK_SWITCH => {
            if stmt_switch(s, label_name).is_err() {
                fail!();
            }
        }
        TOK_TRY => {
            if stmt_try(s).is_err() {
                fail!();
            }
        }
        // ';'
        CHAR_SEMI => {
            // empty statement
            if s.next_token().is_err() {
                fail!();
            }
        }
        TOK_WITH => {
            if stmt_with(s).is_err() {
                fail!();
            }
        }
        TOK_FUNCTION => {
            // ES6 Annex B.3.2 and B.3.3 semantics
            if (decl_mask & DECL_MASK_FUNC == 0)
                || (decl_mask & DECL_MASK_OTHER == 0 && s.peek_token(false) == '*' as i32)
            {
                state.replace(StmtState::FuncDeclError);
            } else {
                state.replace(StmtState::ParseFuncVar);
            }
        }
        TOK_IDENT => {
            if let Ok(st) = stmt_ident(s, decl_mask) {
                state.replace(st);
            } else {
                fail!();
            }
        }
        TOK_CLASS => {
            if (decl_mask & DECL_MASK_OTHER) == 0 {
                s.throw_syntax_error("class declarations can't appear in single-statement context");
                fail!();
            }
            if js_parse_class(s, false, JSParseExportEnum::JS_PARSE_EXPORT_NONE) != 0 {
                return -1;
            }
        }
        TOK_DEBUGGER => {
            // currently no debugger, so just skip the keyword
            if s.next_token().is_err() {
                fail!();
            }
            if s.parse_expect_semi().is_err() {
                fail!();
            }
        }
        TOK_ENUM | TOK_EXPORT | TOK_EXTENDS => {
            js_unsupported_keyword(s, s.token.data.ident_atom().unwrap());
            fail!();
        }
        _ => {
            state.replace(StmtState::HasExpr);
        }
    }

    while let Some(st) = state.take() {
        match st {
            StmtState::HasLet(t) => {
                if stmt_let_const_var(s, t, decl_mask).is_err() {
                    fail!();
                }
            }
            StmtState::HasExpr => {
                if hasexpr(s).is_err() {
                    fail!();
                }
            }
            StmtState::ParseFuncVar => {
                if s.parse_function_decl(
                    JSParseFunctionEnum::JS_PARSE_FUNC_VAR,
                    JSFunctionKindEnum::JS_FUNC_NORMAL,
                    JS_ATOM_NULL,
                    s.token.ptr,
                    s.token.line_num,
                )
                .is_err()
                {
                    fail!();
                }
            }
            StmtState::FuncDeclError => {
                s.throw_syntax_error(
                    "function declarations can't appear in single-statement context",
                );
                fail!();
            }
        }
    }

    JS_FreeAtom(&s.ctx, label_name);
    0
}

pub fn js_unsupported_keyword(s: &mut JSParseState, atom: JSAtom) -> i32 {
    s.throw_syntax_error(&format!("unsupported keyword: {}", atom.to_str(s.ctx.rt())));
    -1
}

// test if the current token is a label. Use simplistic look-ahead scanner
pub unsafe fn is_label(s: *mut JSParseState) -> bool {
    (*s).token.val == TOK_IDENT
        && !(*s).token.data.is_reserved()
        && (*s).peek_token(false) == ':' as i32
}

// test if the current token is a let keyword. Use simplistic look-ahead scanner
pub unsafe fn is_let(s: *mut JSParseState, decl_mask: i32) -> Result<bool, i32> {
    let mut res = Ok(false);

    if (*s).token_is_pseudo_keyword(JS_ATOM_let) {
        {
            let pos = (*s).get_pos();

            loop {
                (*s).next_token()?;

                if (*s).token.val == b'[' as i32 {
                    // let [ is a syntax restriction:
                    // it never introduces an ExpressionStatement
                    res = Ok(true);
                    break;
                }
                if (*s).token.val == b'{' as i32
                    || ((*s).token.val == TOK_IDENT && !(*s).token.data.is_reserved())
                    || matches!((*s).token.val, TOK_LET | TOK_YIELD | TOK_AWAIT)
                {
                    // Check for possible ASI if not scanning for Declaration
                    // XXX: should also check that `{` introduces a BindingPattern,
                    // but Firefox does not and rejects eval("let=1;let\n{if(1)2;}")
                    if (*s).last_line_num == (*s).token.line_num
                        || (decl_mask & DECL_MASK_OTHER) != 0
                    {
                        res = Ok(true);
                        break;
                    }
                    break;
                }
                break;
            }

            (*s).seek_token(&pos)?;
        }

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

pub unsafe fn set_eval_ret_undefined(s: &mut JSParseState) {
    if let Some(vidx) = (*s).cur_func().eval_ret_idx {
        s.emit_op(OpCode::UNDEFINED);
        s.emit_op(OpCode::PUT_LOC);
        s.emit_u16(vidx);
    }
}

pub unsafe fn js_parse_block(s: &mut JSParseState) -> Result<(), i32> {
    s.parse_expect(CHAR_CURLY_L)?;

    if s.token.val != CHAR_CURLY_R {
        s.push_scope()?;
        loop {
            if js_parse_statement_or_decl(s, DECL_MASK_ALL) != 0 {
                return Err(-1);
            }
            if s.token.val == CHAR_CURLY_R {
                break;
            }
        }
        s.pop_scope();
    }

    s.next_token()
}

pub unsafe fn js_parse_statement(s: &mut JSParseState) -> Result<(), i32> {
    if js_parse_statement_or_decl(s, 0) == -1 {
        Err(-1)
    } else {
        Ok(())
    }
}

#[inline(always)]
unsafe fn stmt_return(s: &mut JSParseState) -> Result<(), i32> {
    s.next_token()?;

    if s.got_lf {
        s.throw_syntax_error("line terminator not allowed after throw");
        return Err(-1);
    }
    if s.parse_expr().is_err() {
        return Err(-1);
    }
    s.emit_op(OpCode::THROW);
    if s.parse_expect_semi().is_err() {
        return Err(-1);
    }

    Ok(())
}

#[inline(always)]
unsafe fn stmt_do(s: &mut JSParseState, label_name: JSAtom) -> Result<(), i32> {
    let (label_cont, label_break, label1) = {
        let fd = s.cur_func_mut();

        (
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
        )
    };

    let mut break_entry: BlockEnv = std::mem::zeroed();

    push_break_entry(
        s.cur_func_mut(),
        &mut break_entry,
        label_name,
        label_break,
        label_cont,
        0,
    );

    s.next_token()?;

    s.emit_label(label1);

    set_eval_ret_undefined(s);

    js_parse_statement(s)?;

    s.emit_label(label_cont);

    s.parse_expect(TOK_WHILE)?;
    s.parse_expr_parens()?;

    // Insert semicolon if missing
    if s.token.val == CHAR_SEMI {
        s.next_token()?;
    }
    s.emit_goto(OpCode::IF_TRUE, label1);
    s.emit_label(label_break);

    pop_break_entry(s.cur_func_mut());

    Ok(())
}

#[inline(always)]
unsafe fn stmt_for(s: &mut JSParseState, label_name: JSAtom) -> Result<(), i32> {
    s.next_token()?;

    set_eval_ret_undefined(s);

    let mut bits = 0;
    let mut is_async = false;

    if s.token.val == '(' as i32 {
        js_parse_skip_parens_token(s, &mut bits, false);
    } else if s.token.val == TOK_AWAIT {
        if matches!(
            s.cur_func().func_kind,
            JSFunctionKindEnum::JS_FUNC_ASYNC | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
        ) {
            s.throw_syntax_error("for await is only valid in asynchronous functions");
            return Err(-1);
        }
        is_async = true;
        s.next_token()?;
        s.cur_func_mut().has_await = true;
    }

    s.parse_expect(CHAR_BRACKET_L)?;

    if (bits & SKIP_HAS_SEMI) == 0 {
        // parse for/in or for/of
        return js_parse_for_in_of(s, label_name, is_async);
    }

    let block_scope_level = s.cur_func().scope_level;

    // create scope for the lexical variables declared in the initial,
    // test and increment expressions
    s.push_scope()?;

    // initial expression
    let mut tok = s.token.val;
    if tok != CHAR_SEMI {
        if is_let(s, DECL_MASK_OTHER)? {
            tok = TOK_LET;
        }

        if matches!(tok, TOK_VAR | TOK_LET | TOK_CONST) {
            s.next_token()?;
            s.parse_var(0, tok, false)?;
        } else {
            s.parse_expr2(0)?;
            emit_op(s, OpCode::DROP);
        }

        // close the closures before the first iteration
        let scope_level = s.cur_func().scope_level;
        s.close_scopes(scope_level, block_scope_level);
    }

    s.parse_expect(CHAR_SEMI)?;

    let (mut label_test, mut label_cont, label_body, label_break) = {
        let fd = s.cur_func_mut();

        (
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
            fd.add_label()? as i32,
        )
    };

    let mut break_entry: BlockEnv = std::mem::zeroed();

    push_break_entry(
        s.cur_func_mut(),
        &mut break_entry,
        label_name,
        label_break,
        label_cont,
        0,
    );

    // test expression
    if s.token.val == CHAR_SEMI {
        // no test expression
        label_test = label_body;
    } else {
        s.emit_label(label_test);
        s.parse_expr()?;
        s.emit_goto(OpCode::IF_FALSE, label_break);
    }

    s.parse_expect(CHAR_SEMI)?;

    let pos_cont = if s.token.val == CHAR_BRACKET_R {
        // no end expression
        break_entry.label_cont = label_cont;
        label_cont = label_test;
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

    s.parse_expect(CHAR_BRACKET_R)?;

    let pos_body = s.cur_func().byte_code.len();
    s.emit_label(label_body);
    js_parse_statement(s)?;

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
        // original code
        // for i in pos_cont..pos_body {
        //     bc[i] = OpCode::NOP as u8;
        // }

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

    pop_break_entry(s.cur_func_mut());
    s.pop_scope();

    Ok(())
}

#[inline(always)]
unsafe fn stmt_while(s: &mut JSParseState, label_name: JSAtom) -> Result<(), i32> {
    let mut break_entry: BlockEnv = std::mem::zeroed();

    let (label_cont, label_break) = {
        let fd = s.cur_func_mut();
        (fd.add_label()? as i32, fd.add_label()? as i32)
    };

    push_break_entry(
        s.cur_func_mut(),
        &mut break_entry,
        label_name,
        label_break,
        label_cont,
        0,
    );

    s.next_token()?;

    set_eval_ret_undefined(s);

    s.emit_label(label_cont);
    s.parse_expr_parens()?;

    s.emit_goto(OpCode::IF_FALSE, label_break);

    js_parse_statement(s)?;

    s.emit_goto(OpCode::GOTO, label_cont);

    s.emit_label(label_break);

    pop_break_entry(s.cur_func_mut());

    Ok(())
}

#[inline(always)]
unsafe fn stmt_if(s: &mut JSParseState) -> Result<(), i32> {
    s.next_token()?;

    // create a new scope for `let f;if(1) function f(){}`
    s.push_scope()?;
    set_eval_ret_undefined(s);
    s.parse_expr_parens()?;

    let mut label1 = s.emit_goto(OpCode::IF_FALSE, -1);
    let mask = if s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0 {
        0
    } else {
        DECL_MASK_FUNC // Annex B.3.4
    };
    if js_parse_statement_or_decl(s, mask) != 0 {
        return Err(-1);
    }

    if s.token.val == TOK_ELSE {
        let label2 = s.emit_goto(OpCode::GOTO, -1);
        s.next_token()?;

        s.emit_label(label1);
        if js_parse_statement_or_decl(s, mask) != 0 {
            return Err(-1);
        }

        label1 = label2;
    }
    s.emit_label(label1);

    s.pop_scope();
    Ok(())
}

#[inline(always)]
unsafe fn stmt_try(s: &mut JSParseState) -> Result<(), i32> {
    set_eval_ret_undefined(s);
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

    let mut block_env: BlockEnv = std::mem::zeroed();

    emit_goto(s, OpCode::CATCH, label_catch);

    push_break_entry(s.cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 1);
    block_env.label_finally = label_finally;

    js_parse_block(s)?;
    pop_break_entry(s.cur_func_mut());

    if js_is_live_code(s) {
        // drop the catch offset
        s.emit_op(OpCode::DROP);
        // must push dummy value to keep same stack size
        s.emit_op(OpCode::UNDEFINED);
        s.emit_goto(OpCode::GOSUB, label_finally);
        s.emit_op(OpCode::DROP);
        s.emit_goto(OpCode::GOTO, label_end);
    }

    if s.token.val == TOK_CATCH {
        s.next_token()?;
        s.push_scope()?; // catch variable
        s.emit_label(label_catch);

        if s.token.val == CHAR_CURLY_L {
            // support optional-catch-binding feature
            s.emit_op(OpCode::DROP); // pop the exception object
        } else {
            s.parse_expect(CHAR_BRACKET_L)?;

            if s.token.val != TOK_IDENT || s.token.data.is_reserved() {
                if matches!(s.token.val, CHAR_SQUARE_L | CHAR_CURLY_L) {
                    // XXX: TOK_LET is not completely correct
                    if js_parse_destructuring_element(s, TOK_LET, false, true, -1, true) < 0 {
                        return Err(-1);
                    }
                } else {
                    s.throw_syntax_error("identifier expected");
                    return Err(-1);
                }
            } else {
                let name = s.token.data.ident_atom().unwrap().dup(&s.ctx.rt());
                if s.next_token().is_err() || s.define_var(name, TOK_CATCH).is_err() {
                    JS_FreeAtom(&s.ctx, name);
                    return Err(-1);
                }
                // store the exception value in the catch variable
                s.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR);
                s.emit_u32(name.0);
                s.emit_u16(s.cur_func().scope_level as _);
            }

            s.parse_expect(CHAR_BRACKET_R)?;
        }

        // XXX: should keep the address to nop it out if there is no finally block
        s.emit_goto(OpCode::CATCH, label_catch2);

        s.push_scope()?; // catch block
        push_break_entry(s.cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 1);
        block_env.label_finally = label_finally;
        js_parse_block(s)?;
        pop_break_entry(s.cur_func_mut());

        s.pop_scope(); // catch block
        s.pop_scope(); // catch variable

        if js_is_live_code(s) {
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
    } else if s.token.val == TOK_FINALLY {
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

    if s.token.val == TOK_FINALLY {
        s.next_token()?;

        // on the stack: ret_value gosub_ret_value
        push_break_entry(s.cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 2);

        let mut saved_eval_ret_idx: u16 = 0;

        if let Some(vidx) = s.cur_func().eval_ret_idx {
            // 'finally' updates eval_ret only if not a normal termination
            let ctx = s.ctx.clone();
            saved_eval_ret_idx = s.cur_func_mut().add_var(&ctx, JS_ATOM__ret_)?;

            s.emit_op(OpCode::GET_LOC);
            s.emit_u16(vidx);
            s.emit_op(OpCode::PUT_LOC);
            s.emit_u16(saved_eval_ret_idx as _);
            set_eval_ret_undefined(s);
        }

        js_parse_block(s)?;

        if let Some(vidx) = s.cur_func().eval_ret_idx {
            s.emit_op(OpCode::GET_LOC);
            s.emit_u16(saved_eval_ret_idx);
            s.emit_op(OpCode::PUT_LOC);
            s.emit_u16(vidx);
        }
        pop_break_entry(s.cur_func_mut());
    }
    s.emit_op(OpCode::RET);
    s.emit_label(label_end);

    Ok(())
}

#[inline(always)]
unsafe fn stmt_let_const_var(s: &mut JSParseState, tok: i32, decl_mask: i32) -> Result<(), i32> {
    if matches!(tok, TOK_LET | TOK_CONST) {
        if (decl_mask & DECL_MASK_OTHER) == 0 {
            s.throw_syntax_error("lexical declarations can't appear in single-statement context");
            return Err(-1);
        }
    }

    s.next_token()?;
    s.parse_var(1, tok, false)?;
    s.parse_expect_semi()?;
    Ok(())
}

#[inline(always)]
unsafe fn stmt_switch(s: &mut JSParseState, label_name: JSAtom) -> Result<(), i32> {
    s.next_token()?;

    set_eval_ret_undefined(s);
    s.parse_expr_parens()?;

    s.push_scope()?;

    let label_break = s.cur_func_mut().add_label()? as i32;
    let mut break_entry: BlockEnv = std::mem::zeroed();

    push_break_entry(
        s.cur_func_mut(),
        &mut break_entry,
        label_name,
        label_break,
        -1,
        1,
    );

    s.parse_expect(CHAR_CURLY_L)?;

    let mut default_label_pos = -1;
    let mut label_case = -1;
    let mut label1: i32;

    while s.token.val != CHAR_CURLY_R {
        if s.token.val == TOK_CASE {
            label1 = -1;
            if label_case >= 0 {
                // skip the case if needed
                label1 = emit_goto(s, OpCode::GOTO, -1);
            }
            emit_label(s, label_case);
            label_case = -1;

            loop {
                // parse a sequence of case clauses
                s.next_token()?;

                s.emit_op(OpCode::DUP);
                s.parse_expr()?;
                s.parse_expect(CHAR_COLON)?;

                s.emit_op(OpCode::STRICT_EQ);
                if s.token.val == TOK_CASE {
                    label1 = s.emit_goto(OpCode::IF_TRUE, label1);
                } else {
                    label_case = s.emit_goto(OpCode::IF_FALSE, -1);
                    s.emit_label(label1);
                    break;
                }
            }
        } else if s.token.val == TOK_DEFAULT {
            s.next_token()?;
            s.parse_expect(CHAR_COLON)?;

            if default_label_pos >= 0 {
                s.throw_syntax_error("duplicate default");
                return Err(-1);
            }
            if label_case < 0 {
                // falling thru direct from switch expression
                label_case = emit_goto(s, OpCode::GOTO, -1);
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
            if js_parse_statement_or_decl(s, DECL_MASK_ALL) != 0 {
                return Err(-1);
            }
        }
    }

    s.parse_expect(CHAR_CURLY_R)?;

    if default_label_pos >= 0 {
        // Ugly patch for the the `default` label, shameful and risky
        let fd = s.cur_func_mut();
        write_to_ptr::<u32>(
            fd.byte_code.as_mut_ptr().add(default_label_pos as usize),
            label_case as u32,
        );
        fd.label_slots[label_case as usize].pos = default_label_pos + 4;
    } else {
        s.emit_label(label_case);
    }
    s.emit_label(label_break);
    s.emit_op(OpCode::DROP); // drop the switch expression

    pop_break_entry(s.cur_func_mut());
    s.pop_scope();

    Ok(())
}

#[inline(always)]
unsafe fn stmt_with(s: &mut JSParseState) -> Result<(), i32> {
    if s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0 {
        s.throw_syntax_error("invalid keyword: with");
        Err(-1)
    } else {
        s.next_token()?;
        s.parse_expr_parens()?;

        s.push_scope()?;

        let with_idx = s.define_var_cur_func(JS_ATOM__with_, JSVarDefEnum::JS_VAR_DEF_WITH)?;

        s.emit_op(OpCode::TO_OBJECT);
        s.emit_op(OpCode::PUT_LOC);
        s.emit_u16(with_idx as _);

        set_eval_ret_undefined(s);
        js_parse_statement(s)?;

        // Popping scope drops lexical context for the with object variable
        s.pop_scope();

        Ok(())
    }
}

#[inline(always)]
unsafe fn stmt_ident(s: &mut JSParseState, decl_mask: i32) -> Result<StmtState, i32> {
    if s.token.data.is_reserved() {
        s.throw_reserved_identifier_error();
        return Err(-1);
    }
    // Determine if `let` introduces a Declaration or an ExpressionStatement
    if is_let(s, decl_mask)? {
        return Ok(StmtState::HasLet(TOK_LET));
    }
    if (*s).token_is_pseudo_keyword(JS_ATOM_async) && s.peek_token(true) == TOK_FUNCTION {
        if decl_mask & DECL_MASK_OTHER == 0 {
            s.throw_syntax_error("function declarations can't appear in single-statement context");
            return Err(-1);
        }
        return Ok(StmtState::ParseFuncVar);
    }

    Ok(StmtState::HasExpr)
}

#[inline(always)]
unsafe fn hasexpr(s: &mut JSParseState) -> Result<(), i32> {
    s.parse_expr()?;

    if let Some(vidx) = s.cur_func().eval_ret_idx {
        // store the expression value so that it can be returned by eval()
        s.emit_op(OpCode::PUT_LOC);
        s.emit_u16(vidx);
    } else {
        // drop the result
        s.emit_op(OpCode::DROP);
    }

    s.parse_expect_semi()
}

// XXX: handle IteratorClose when exiting the loop before the
// enumeration is done
pub unsafe fn js_parse_for_in_of(
    s: &mut JSParseState,
    label_name: JSAtom,
    is_async: bool,
) -> Result<(), i32> {
    let block_scope_level = s.cur_func().scope_level; // save current scope label

    let mut var_name = JS_ATOM_NULL;
    let mut has_initializer = false;
    let mut is_for_of = false;
    let mut has_destructuring = false;
    let mut opcode: i32 = 0;
    let mut scope: i32 = 0;

    let mut break_entry: BlockEnv = std::mem::zeroed();

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
    push_break_entry(
        s.cur_func_mut(),
        &mut break_entry,
        label_name,
        label_break,
        label_cont,
        1,
    );
    break_entry.scope_level = block_scope_level;

    let label_expr = emit_goto(s, OpCode::GOTO, -1);

    let pos_next = s.cur_func().byte_code.len();
    emit_label(s, label_next);

    let tok = if is_let(s, DECL_MASK_OTHER)? {
        TOK_LET
    } else {
        s.token.val
    };

    if matches!(tok, TOK_VAR | TOK_LET | TOK_CONST) {
        (*s).next_token()?;

        if s.token.val != TOK_IDENT || s.token.data.is_reserved() {
            if s.token.val == '[' as i32 || s.token.val == '{' as i32 {
                if js_parse_destructuring_element(s, tok, false, true, -1, false) < 0 {
                    return Err(-1);
                }
                has_destructuring = true;
            } else {
                s.throw_syntax_error("variable name expected");
                return Err(-1);
            }
            var_name = JS_ATOM_NULL;
        } else {
            var_name = s.token.data.ident_atom().unwrap().dup(&s.ctx.rt());
            if s.next_token().is_err() {
                JS_FreeAtom(&s.ctx, var_name);
                return Err(-1);
            }
            if s.define_var(var_name, tok).is_err() {
                JS_FreeAtom(&s.ctx, var_name);
                return Err(-1);
            }
            emit_op_temp(
                s,
                if tok == TOK_CONST || tok == TOK_LET {
                    OpCodeTemp::SCOPE_PUT_VAR_INIT
                } else {
                    OpCodeTemp::SCOPE_PUT_VAR
                },
            );
            emit_atom_dup(s, var_name);
            emit_u16(s, s.cur_func().scope_level as _);
        }
    } else if !is_async && s.token_is_pseudo_keyword(JS_ATOM_async) && s.peek_token(false) == TOK_OF
    {
        s.throw_syntax_error("'for of' expression cannot start with 'async'");
        return Err(-1);
    } else {
        let mut skip_bits: i32 = 0;

        if matches!(s.token.val, CHAR_SQUARE_L | CHAR_CURLY_L)
            && matches!(
                js_parse_skip_parens_token(s, &mut skip_bits, false),
                TOK_IN | TOK_OF
            )
        {
            if js_parse_destructuring_element(
                s,
                0,
                false,
                true,
                skip_bits & SKIP_HAS_ELLIPSIS,
                true,
            ) < 0
            {
                return Err(-1);
            }
        } else {
            s.parse_left_hand_side_expr()?;

            let mut lvalue_label: i32 = 0;
            s.get_lvalue(
                &mut opcode,
                &mut scope,
                &mut var_name,
                &mut lvalue_label,
                std::ptr::null_mut(),
                false,
                TOK_FOR,
            )?;

            s.put_lvalue(
                opcode,
                scope,
                var_name,
                lvalue_label,
                PutLValueEnum::PUT_LVALUE_NOKEEP_BOTTOM,
                false,
            );
        }

        var_name = JS_ATOM_NULL;
    }

    emit_goto(s, OpCode::GOTO, label_body);

    let pos_expr = s.cur_func().byte_code.len();
    emit_label(s, label_expr);

    if s.token.val == CHAR_EQUAL {
        // XXX: potential scoping issue if inside `with` statement
        has_initializer = true;
        // parse and evaluate initializer prior to evaluating the
        // object (only used with "for in" with a non lexical variable
        // in non strict mode)
        if s.next_token().is_err() || s.parse_assign_expr2(0).is_err() {
            JS_FreeAtom(&s.ctx, var_name);
            return Err(-1);
        }
        if var_name != JS_ATOM_NULL {
            emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR);
            emit_atom_dup(s, var_name);
            emit_u16(s, s.cur_func().scope_level as _);
        }
    }
    JS_FreeAtom(&s.ctx, var_name);

    if s.token_is_pseudo_keyword(JS_ATOM_of) {
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
    } else if s.token.val == TOK_IN {
        if is_async {
            s.throw_syntax_error("'for await' loop should be used with 'of'");
            return Err(-1);
        }
        if has_initializer
            && (tok != TOK_VAR
                || s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0
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

    s.parse_expect(CHAR_BRACKET_R)?;

    if OPTIMIZE {
        // move the `next` code here
        let bc = &mut s.cur_func_mut().byte_code;
        let chunk_size = pos_expr - pos_next;
        let offset = bc.len() - pos_next;

        bc.extend(std::iter::repeat_n(OpCode::NOP as u8, chunk_size));
        for i in (pos_expr..pos_next).rev() {
            bc[i + offset] = bc[i];
        }
        for item in bc.iter_mut().take(pos_next).skip(pos_expr) {
            *item = OpCode::NOP as u8;
        }
        // for i in pos_expr..pos_next {
        //     bc[i] = OpCode::NOP as u8;
        // }

        // `next` part ends with a goto
        s.cur_func_mut().last_opcode_pos = bc.len() as i32 - 5;

        // relocate labels
        for lb in &mut s.cur_func_mut().label_slots[label_cont as usize..] {
            if lb.pos >= pos_next as i32 && lb.pos < pos_expr as i32 {
                lb.pos += offset as i32;
            }
        }
        // for i in label_cont..(*s.cur_func).label_count as i32 {
        //     let ls = (*s.cur_func)
        //         .label_slots
        //         .offset(i as isize)
        //         .as_mut()
        //         .unwrap();

        //     if ls.pos >= pos_next as i32 && ls.pos < pos_expr as i32 {
        //         ls.pos += offset as i32;
        //     }
        // }
    }

    s.emit_label(label_body);
    js_parse_statement(s)?;

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
    if is_for_of {
        // close and drop enum_rec
        s.emit_op(OpCode::ITERATOR_CLOSE);
    } else {
        s.emit_op(OpCode::DROP);
    }
    pop_break_entry(s.cur_func_mut());
    s.pop_scope();

    Ok(())
}
