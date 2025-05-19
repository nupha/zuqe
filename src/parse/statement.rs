use std::{ffi::CStr, ops::DerefMut};

use crate::{
    atom::{consts::*, JSAtom, JS_DupAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{OpCode, OpCodeTemp},
    context::JS_MODE_STRICT,
    function::{pop_break_entry, BlockEnv, JSFunctionKindEnum, JSVarDefEnum, OPTIMIZE},
    parse::emit::emit_return,
    utils::write_to_ptr,
};

use super::{
    class::js_parse_class,
    destructing::js_parse_destructuring_element,
    emit::{
        emit_atom_dup, emit_break, emit_goto, emit_label, emit_op, emit_op_temp, emit_u16,
        emit_u32, emit_u8, js_is_live_code,
    },
    error::js_parse_error_reserved_identifier,
    esm::JSParseExportEnum,
    expr_lvalue::PutLValueEnum,
    function::{push_break_entry, JSParseFunctionEnum},
    parse::{js_define_var, js_parse_skip_parens_token},
    state::{JSParseState, SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI},
    token::{
        peek_token, token_is_pseudo_keyword, CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_COLON,
        CHAR_CURLY_L, CHAR_CURLY_R, CHAR_EQUAL, CHAR_SEMI, CHAR_SQUARE_L, TOK_AWAIT, TOK_BREAK,
        TOK_CASE, TOK_CATCH, TOK_CLASS, TOK_CONST, TOK_CONTINUE, TOK_DEBUGGER, TOK_DEFAULT, TOK_DO,
        TOK_ELSE, TOK_ENUM, TOK_EXPORT, TOK_EXTENDS, TOK_FINALLY, TOK_FOR, TOK_FUNCTION, TOK_IDENT,
        TOK_IF, TOK_IN, TOK_LET, TOK_OF, TOK_RETURN, TOK_SWITCH, TOK_THROW, TOK_TRY, TOK_VAR,
        TOK_WHILE, TOK_WITH, TOK_YIELD,
    },
    var::define_var,
};

pub const DECL_MASK_FUNC: i32 = 1 << 0; /* allow normal function declaration */
/* ored with DECL_MASK_FUNC if function declarations are allowed with a label */
pub const DECL_MASK_FUNC_WITH_LABEL: i32 = 1 << 1;
pub const DECL_MASK_OTHER: i32 = 1 << 2; /* all other declarations */
pub const DECL_MASK_ALL: i32 = DECL_MASK_FUNC | DECL_MASK_FUNC_WITH_LABEL | DECL_MASK_OTHER;

pub unsafe fn js_parse_statement_or_decl(s: &mut JSParseState, decl_mask: i32) -> i32 {
    let mut label_name: JSAtom = JS_ATOM_NULL;

    macro_rules! fail {
        () => {
            JS_FreeAtom(s.ctx.as_ptr(), label_name);
            return -1;
        };
    }

    // specific label handling
    // XXX: support multiple labels on loop statements
    if is_label(s) {
        label_name = JS_DupAtom(s.ctx.as_ptr(), s.token.u.ident.atom);

        let mut be = s.cur_func().top_break;
        while !be.is_null() {
            if (*be).label_name == label_name {
                s.throw_syntax_error(c"duplicate label name");
                fail!();
            }
            be = (*be).prev;
        }

        if s.next_token().is_err() {
            fail!();
        } else if s.parse_expect(CHAR_COLON).is_err() {
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
            JS_FreeAtom(s.ctx.as_ptr(), label_name);
            return 0;
        }
    }

    match s.token.val {
        // '{'
        0x7b => {
            if js_parse_block(s).is_err() {
                fail!();
            }
        }
        TOK_RETURN => {
            let fd = s.cur_func();
            if fd.is_eval {
                s.throw_syntax_error(c"return not in a function");
                fail!();
            }
            if fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT {
                s.throw_syntax_error(c"return in a static initializer block");
                fail!();
            }
            if s.next_token().is_err() {
                fail!();
            }
            if !matches!(s.token.val as u8, b';' | b'}') && !(*s).got_lf {
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
                s.throw_syntax_error(c"line terminator not allowed after throw");
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
            if stmt_let_const_var(s, tok, decl_mask).is_err() {
                fail!();
            }
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
            let label = if !s.got_lf && s.token.val == TOK_IDENT && !s.token.u.ident.is_reserved {
                s.token.u.ident.atom
            } else {
                JS_ATOM_NULL
            };
            if emit_break(s, label, tok == TOK_CONTINUE) != 0 {
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
        0x3b => {
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
            if stmt_function(s, decl_mask).is_err() {
                fail!();
            }
        }
        TOK_IDENT => {
            if stmt_ident(s, decl_mask).is_err() {
                fail!();
            }
        }
        TOK_CLASS => {
            if (decl_mask & DECL_MASK_OTHER) == 0 {
                s.throw_syntax_error(
                    c"class declarations can't appear in single-statement context",
                );
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
            js_unsupported_keyword(s, s.token.u.ident.atom);
            fail!();
        }

        _ => {
            if hasexpr(s).is_err() {
                fail!();
            }
        }
    }

    JS_FreeAtom(s.ctx.as_ptr(), label_name);
    return 0;
}

pub fn js_unsupported_keyword(s: &mut JSParseState, atom: JSAtom) -> i32 {
    s.throw_syntax_error(
        CStr::from_bytes_with_nul(
            format!("unsupported keyword: {}\0", atom.to_str(s.ctx.rt())).as_bytes(),
        )
        .unwrap(),
    );
    -1
}

// test if the current token is a label. Use simplistic look-ahead scanner
pub unsafe fn is_label(s: *mut JSParseState) -> bool {
    (*s).token.val == TOK_IDENT
        && !(*s).token.u.ident.is_reserved
        && peek_token(s, false) == ':' as i32
}

// test if the current token is a let keyword. Use simplistic look-ahead scanner
pub unsafe fn is_let(s: *mut JSParseState, decl_mask: i32) -> Result<bool, i32> {
    let mut res = Ok(false);

    if token_is_pseudo_keyword(s, JS_ATOM_let) {
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
                    || ((*s).token.val == TOK_IDENT && !(*s).token.u.ident.is_reserved)
                    || (*s).token.val == TOK_LET
                    || (*s).token.val == TOK_YIELD
                    || (*s).token.val == TOK_AWAIT
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
        emit_op(s, OpCode::UNDEFINED);
        emit_op(s, OpCode::PUT_LOC);
        emit_u16(s, vidx);
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

unsafe fn stmt_return(s: &mut JSParseState) -> Result<(), i32> {
    s.next_token()?;

    if s.got_lf {
        s.throw_syntax_error(c"line terminator not allowed after throw");
        return Err(-1);
    }
    if s.parse_expr().is_err() {
        return Err(-1);
    }
    emit_op(s, OpCode::THROW);
    if s.parse_expect_semi().is_err() {
        return Err(-1);
    }

    Ok(())
}

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

    emit_label(s, label1);

    set_eval_ret_undefined(s);

    js_parse_statement(s)?;

    emit_label(s, label_cont);

    s.parse_expect(TOK_WHILE)?;
    s.parse_expr_parens()?;

    // Insert semicolon if missing
    if s.token.val == CHAR_SEMI {
        s.next_token()?;
    }
    emit_goto(s, OpCode::IF_TRUE, label1);
    emit_label(s, label_break);

    pop_break_entry(s.cur_func_mut());

    Ok(())
}

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
            s.throw_syntax_error(c"for await is only valid in asynchronous functions");
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
        emit_label(s, label_test);
        s.parse_expr()?;
        emit_goto(s, OpCode::IF_FALSE, label_break);
    }

    s.parse_expect(CHAR_SEMI)?;

    let pos_cont = if s.token.val == CHAR_BRACKET_R {
        // no end expression
        break_entry.label_cont = label_cont;
        label_cont = label_test;
        0 // avoid warning
    } else {
        // skip the end expression
        emit_goto(s, OpCode::GOTO, label_body);

        let p = s.cur_func().byte_code.len();
        emit_label(s, label_cont);

        s.parse_expr()?;

        emit_op(s, OpCode::DROP);
        if label_test != label_body {
            emit_goto(s, OpCode::GOTO, label_test);
        }

        p
    };

    s.parse_expect(CHAR_BRACKET_R)?;

    let pos_body = s.cur_func().byte_code.len();
    emit_label(s, label_body);
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

        bc.extend(std::iter::repeat(OpCode::NOP as u8).take(chunk_size));
        for i in (pos_cont..pos_body).rev() {
            bc[i + offset] = bc[i];
        }
        for i in pos_cont..pos_body {
            bc[i] = OpCode::NOP as u8;
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
        emit_goto(s, OpCode::GOTO, label_cont);
    }

    emit_label(s, label_break);

    pop_break_entry(s.cur_func_mut());
    s.pop_scope();

    Ok(())
}

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

    emit_label(s, label_cont);
    s.parse_expr_parens()?;

    emit_goto(s, OpCode::IF_FALSE, label_break);

    js_parse_statement(s)?;

    emit_goto(s, OpCode::GOTO, label_cont);

    emit_label(s, label_break);

    pop_break_entry(s.cur_func_mut());

    Ok(())
}

unsafe fn stmt_if(s: &mut JSParseState) -> Result<(), i32> {
    s.next_token()?;

    // create a new scope for `let f;if(1) function f(){}`
    s.push_scope()?;
    set_eval_ret_undefined(s);
    s.parse_expr_parens()?;

    let mut label1 = emit_goto(s, OpCode::IF_FALSE, -1);
    let mask = if s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0 {
        0
    } else {
        DECL_MASK_FUNC // Annex B.3.4
    };
    if js_parse_statement_or_decl(s, mask) != 0 {
        return Err(-1);
    }

    if s.token.val == TOK_ELSE {
        let label2 = emit_goto(s, OpCode::GOTO, -1);
        s.next_token()?;

        emit_label(s, label1);
        if js_parse_statement_or_decl(s, mask) != 0 {
            return Err(-1);
        }

        label1 = label2;
    }
    emit_label(s, label1);

    s.pop_scope();
    Ok(())
}

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

    let mut ctx = s.ctx;
    let mut block_env: BlockEnv = std::mem::zeroed();

    emit_goto(s, OpCode::CATCH, label_catch);

    push_break_entry(s.cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 1);
    block_env.label_finally = label_finally;

    js_parse_block(s)?;
    pop_break_entry(s.cur_func_mut());

    if js_is_live_code(s) {
        // drop the catch offset
        emit_op(s, OpCode::DROP);
        // must push dummy value to keep same stack size
        emit_op(s, OpCode::UNDEFINED);
        emit_goto(s, OpCode::GOSUB, label_finally);
        emit_op(s, OpCode::DROP);
        emit_goto(s, OpCode::GOTO, label_end);
    }

    if s.token.val == TOK_CATCH {
        s.next_token()?;
        s.push_scope()?; // catch variable
        emit_label(s, label_catch);

        if s.token.val == CHAR_CURLY_L {
            // support optional-catch-binding feature
            emit_op(s, OpCode::DROP); // pop the exception object
        } else {
            s.parse_expect(CHAR_BRACKET_L)?;

            if !(s.token.val == TOK_IDENT && !s.token.u.ident.is_reserved) {
                if matches!(s.token.val, CHAR_SQUARE_L | CHAR_CURLY_L) {
                    // XXX: TOK_LET is not completely correct
                    if js_parse_destructuring_element(s, TOK_LET, false, true, -1, true) < 0 {
                        return Err(-1);
                    }
                } else {
                    s.throw_syntax_error(c"identifier expected");
                    return Err(-1);
                }
            } else {
                let name = JS_DupAtom(s.ctx.as_ptr(), s.token.u.ident.atom);
                if s.next_token().is_err() || js_define_var(s, name, TOK_CATCH) < 0 {
                    JS_FreeAtom(s.ctx.as_ptr(), name);
                    return Err(-1);
                }
                // store the exception value in the catch variable
                emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR);
                emit_u32(s, name.0);
                emit_u16(s, s.cur_func().scope_level as _);
            }

            s.parse_expect(CHAR_BRACKET_R)?;
        }

        // XXX: should keep the address to nop it out if there is no finally block
        emit_goto(s, OpCode::CATCH, label_catch2);

        s.push_scope()?; // catch block
        push_break_entry(s.cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 1);
        block_env.label_finally = label_finally;
        js_parse_block(s)?;
        pop_break_entry(s.cur_func_mut());

        s.pop_scope(); // catch block
        s.pop_scope(); // catch variable

        if js_is_live_code(s) {
            // drop the catch2 offset
            emit_op(s, OpCode::DROP);
            // XXX: should keep the address to nop it out if there is no finally block
            // must push dummy value to keep same stack size
            emit_op(s, OpCode::UNDEFINED);
            emit_goto(s, OpCode::GOSUB, label_finally);
            emit_op(s, OpCode::DROP);
            emit_goto(s, OpCode::GOTO, label_end);
        }
        // catch exceptions thrown in the catch block to execute the
        // finally clause and rethrow the exception
        emit_label(s, label_catch2);
        // catch value is at TOS, no need to push undefined
        emit_goto(s, OpCode::GOSUB, label_finally);
        emit_op(s, OpCode::THROW);
    } else if s.token.val == TOK_FINALLY {
        // finally without catch : execute the finally clause
        // and rethrow the exception
        emit_label(s, label_catch);
        // catch value is at TOS, no need to push undefined
        emit_goto(s, OpCode::GOSUB, label_finally);
        emit_op(s, OpCode::THROW);
    } else {
        s.throw_syntax_error(c"expecting catch or finally");
        return Err(-1);
    }

    emit_label(s, label_finally);

    if s.token.val == TOK_FINALLY {
        s.next_token()?;

        // on the stack: ret_value gosub_ret_value
        push_break_entry(s.cur_func_mut(), &mut block_env, JS_ATOM_NULL, -1, -1, 2);

        let mut saved_eval_ret_idx: u16 = 0;

        if let Some(vidx) = s.cur_func().eval_ret_idx {
            // 'finally' updates eval_ret only if not a normal termination
            saved_eval_ret_idx = s.cur_func_mut().add_var(ctx.deref_mut(), JS_ATOM__ret_)?;

            emit_op(s, OpCode::GET_LOC);
            emit_u16(s, vidx);
            emit_op(s, OpCode::PUT_LOC);
            emit_u16(s, saved_eval_ret_idx as _);
            set_eval_ret_undefined(s);
        }

        js_parse_block(s)?;

        if let Some(vidx) = s.cur_func().eval_ret_idx {
            emit_op(s, OpCode::GET_LOC);
            emit_u16(s, saved_eval_ret_idx);
            emit_op(s, OpCode::PUT_LOC);
            emit_u16(s, vidx);
        }
        pop_break_entry(s.cur_func_mut());
    }
    emit_op(s, OpCode::RET);
    emit_label(s, label_end);

    Ok(())
}

unsafe fn stmt_let_const_var(s: &mut JSParseState, tok: i32, decl_mask: i32) -> Result<(), i32> {
    if matches!(tok, TOK_LET | TOK_CONST) {
        if (decl_mask & DECL_MASK_OTHER) == 0 {
            s.throw_syntax_error(c"lexical declarations can't appear in single-statement context");
            return Err(-1);
        }
    }

    s.next_token()?;
    s.parse_var(1, tok, false)?;
    s.parse_expect_semi()?;

    Ok(())
}

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

                emit_op(s, OpCode::DUP);
                s.parse_expr()?;
                s.parse_expect(CHAR_COLON)?;

                emit_op(s, OpCode::STRICT_EQ);
                if s.token.val == TOK_CASE {
                    label1 = emit_goto(s, OpCode::IF_TRUE, label1);
                } else {
                    label_case = emit_goto(s, OpCode::IF_FALSE, -1);
                    emit_label(s, label1);
                    break;
                }
            }
        } else if s.token.val == TOK_DEFAULT {
            s.next_token()?;
            s.parse_expect(CHAR_COLON)?;

            if default_label_pos >= 0 {
                s.throw_syntax_error(c"duplicate default");
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
            emit_op_temp(s, OpCodeTemp::LABEL);
            emit_u32(s, 0);
            default_label_pos = s.cur_func().byte_code.len() as i32 - 4;
        } else {
            if label_case < 0 {
                // falling thru direct from switch expression
                s.throw_syntax_error(c"invalid switch statement");
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
        emit_label(s, label_case);
    }
    emit_label(s, label_break);
    emit_op(s, OpCode::DROP); // drop the switch expression

    pop_break_entry(s.cur_func_mut());
    s.pop_scope();

    Ok(())
}

unsafe fn stmt_with(s: &mut JSParseState) -> Result<(), i32> {
    if s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0 {
        s.throw_syntax_error(c"invalid keyword: with");
        Err(-1)
    } else {
        let with_idx: i32;

        s.next_token()?;
        s.parse_expr_parens()?;

        s.push_scope()?;

        with_idx = define_var(
            s,
            s.cur_func_mut(),
            JS_ATOM__with_,
            JSVarDefEnum::JS_VAR_DEF_WITH,
        );
        if with_idx < 0 {
            return Err(-1);
        }

        emit_op(s, OpCode::TO_OBJECT);
        emit_op(s, OpCode::PUT_LOC);
        emit_u16(s, with_idx as _);

        set_eval_ret_undefined(s);
        js_parse_statement(s)?;

        // Popping scope drops lexical context for the with object variable
        s.pop_scope();

        Ok(())
    }
}

unsafe fn stmt_function(s: &mut JSParseState, decl_mask: i32) -> Result<(), i32> {
    // ES6 Annex B.3.2 and B.3.3 semantics
    if (decl_mask & DECL_MASK_FUNC) == 0 {
        s.throw_syntax_error(c"function declarations can't appear in single-statement context");
        return Err(-1);
    }
    if decl_mask & DECL_MASK_OTHER == 0 && peek_token(s, false) == '*' as i32 {
        s.throw_syntax_error(c"function declarations can't appear in single-statement context");
        return Err(-1);
    }

    s.parse_function_decl(
        JSParseFunctionEnum::JS_PARSE_FUNC_VAR,
        JSFunctionKindEnum::JS_FUNC_NORMAL,
        JS_ATOM_NULL,
        s.token.ptr,
        s.token.line_num,
    )?;

    Ok(())
}

unsafe fn stmt_ident(s: &mut JSParseState, decl_mask: i32) -> Result<(), i32> {
    if s.token.u.ident.is_reserved {
        js_parse_error_reserved_identifier(s);
        return Err(-1);
    }
    // Determine if `let` introduces a Declaration or an ExpressionStatement
    if is_let(s, decl_mask)? {
        return stmt_let_const_var(s, TOK_LET, decl_mask);
    }

    if token_is_pseudo_keyword(s, JS_ATOM_async) && peek_token(s, true) == TOK_FUNCTION {
        if (decl_mask & DECL_MASK_OTHER) == 0 {
            s.throw_syntax_error(c"function declarations can't appear in single-statement context");
            return Err(-1);
        }

        // label parse_func_var:
        s.parse_function_decl(
            JSParseFunctionEnum::JS_PARSE_FUNC_VAR,
            JSFunctionKindEnum::JS_FUNC_NORMAL,
            JS_ATOM_NULL,
            s.token.ptr,
            s.token.line_num,
        )?;
        return Ok(());
    }

    hasexpr(s)
}

unsafe fn hasexpr(s: &mut JSParseState) -> Result<(), i32> {
    s.parse_expr()?;

    if let Some(vidx) = s.cur_func().eval_ret_idx {
        // store the expression value so that it can be returned by eval()
        emit_op(s, OpCode::PUT_LOC);
        emit_u16(s, vidx);
    } else {
        // drop the result
        emit_op(s, OpCode::DROP);
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
    let ctx = s.ctx;
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

        if !(s.token.val == TOK_IDENT && !s.token.u.ident.is_reserved) {
            if s.token.val == '[' as i32 || s.token.val == '{' as i32 {
                if js_parse_destructuring_element(s, tok, false, true, -1, false) < 0 {
                    return Err(-1);
                }
                has_destructuring = true;
            } else {
                s.throw_syntax_error(c"variable name expected");
                return Err(-1);
            }
            var_name = JS_ATOM_NULL;
        } else {
            var_name = JS_DupAtom(ctx.as_ptr(), s.token.u.ident.atom);
            if (*s).next_token().is_err() {
                JS_FreeAtom(s.ctx.as_ptr(), var_name);
                return Err(-1);
            }
            if js_define_var(s, var_name, tok) != 0 {
                JS_FreeAtom(s.ctx.as_ptr(), var_name);
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
    } else if !is_async
        && token_is_pseudo_keyword(s, JS_ATOM_async)
        && peek_token(s, false) == TOK_OF
    {
        s.throw_syntax_error(c"'for of' expression cannot start with 'async'");
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
            JS_FreeAtom(ctx.as_ptr(), var_name);
            return Err(-1);
        }
        if var_name != JS_ATOM_NULL {
            emit_op_temp(s, OpCodeTemp::SCOPE_PUT_VAR);
            emit_atom_dup(s, var_name);
            emit_u16(s, s.cur_func().scope_level as _);
        }
    }
    JS_FreeAtom(ctx.as_ptr(), var_name);

    if token_is_pseudo_keyword(s, JS_ATOM_of) {
        is_for_of = true;
        break_entry.has_iterator = true;
        break_entry.drop_count += 2;
        if has_initializer {
            s.throw_syntax_error(
                CStr::from_bytes_with_nul(
                    format!(
                        "a declaration in the head of a for-{} loop can't have an initializer\0",
                        if is_for_of { "of" } else { "in" }
                    )
                    .as_bytes(),
                )
                .unwrap(),
            );
            return Err(-1);
        }
    } else if s.token.val == TOK_IN {
        if is_async {
            s.throw_syntax_error(c"'for await' loop should be used with 'of'");
            return Err(-1);
        }
        if has_initializer
            && (tok != TOK_VAR
                || s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0
                || has_destructuring)
        {
            s.throw_syntax_error(CStr::from_ptr(
                format!(
                    "a declaration in the head of a for-{} loop can't have an initializer\0",
                    if is_for_of { "of" } else { "in" }
                )
                .as_ptr()
                .cast(),
            ));
            return Err(-1);
        }
    } else {
        s.throw_syntax_error(c"expected 'of' or 'in' in for control expression");
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

    emit_op(
        s,
        if is_for_of {
            if is_async {
                OpCode::FOR_AWAIT_OF_START
            } else {
                OpCode::FOR_OF_START
            }
            // on stack: enum_rec
        } else {
            OpCode::FOR_IN_START
            // on stack: enum_obj
        },
    );
    emit_goto(s, OpCode::GOTO, label_cont);

    s.parse_expect(CHAR_BRACKET_R)?;

    if OPTIMIZE {
        // move the `next` code here
        let bc = &mut s.cur_func_mut().byte_code;
        let chunk_size = pos_expr - pos_next;
        let offset = bc.len() - pos_next;

        bc.extend(std::iter::repeat(OpCode::NOP as u8).take(chunk_size));
        for i in (pos_expr..pos_next).rev() {
            bc[i + offset] = bc[i];
        }
        for i in pos_expr..pos_next {
            bc[i] = OpCode::NOP as u8;
        }

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

    emit_label(s, label_body);
    js_parse_statement(s)?;

    let scope_level = s.cur_func().scope_level;
    s.close_scopes(scope_level, block_scope_level);

    emit_label(s, label_cont);
    if is_for_of {
        if is_async {
            // call the next method
            // stack: iter_obj next catch_offset
            emit_op(s, OpCode::DUP3);
            emit_op(s, OpCode::DROP);
            emit_op(s, OpCode::CALL_METHOD);
            emit_u16(s, 0);
            // get the result of the promise
            emit_op(s, OpCode::AWAIT);
            // unwrap the value and done values
            emit_op(s, OpCode::ITERATOR_GET_VALUE_DONE);
        } else {
            emit_op(s, OpCode::FOR_OF_NEXT);
            emit_u8(s, 0);
        }
    } else {
        emit_op(s, OpCode::FOR_IN_NEXT);
    }
    // on stack: enum_rec / enum_obj value bool
    emit_goto(s, OpCode::IF_FALSE, label_next);
    // drop the undefined value from for_xx_next
    emit_op(s, OpCode::DROP);

    emit_label(s, label_break);
    if is_for_of {
        // close and drop enum_rec
        emit_op(s, OpCode::ITERATOR_CLOSE);
    } else {
        emit_op(s, OpCode::DROP);
    }
    pop_break_entry(s.cur_func_mut());
    s.pop_scope();

    Ok(())
}
