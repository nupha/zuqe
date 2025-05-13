use std::{ffi::CStr, ops::DerefMut};

use smallvec::SmallVec;

use crate::{
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_ATOM_NULL},
    bytecode::{opcodes, OpCode, OpCodeTemp},
    context::{JS_MODE_STRICT, JS_MODE_STRIP},
    error::JS_THROW_ERROR_DELETE_SUPER,
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::{JSFunctionDef, JSFunctionKindEnum, JSVarDefEnum},
    module::JSExportTypeEnum,
    parse::expr_lvalue::PutLValueEnum,
    unicode::{unicode_from_utf8, unicode_to_utf8, UTF8_CHAR_LEN_MAX},
    utils::get_u32,
    value::JS_FreeValue,
};

use super::{
    destructing::js_parse_destructuring_element,
    emit::{
        emit_atom_dup, emit_goto, emit_label, emit_op, emit_op_temp, emit_push_const, emit_return,
        emit_u16, emit_u8, get_prev_opcode,
    },
    error::{js_parse_error, js_parse_error_reserved_identifier},
    esm::{add_export_entry, js_parse_export},
    function::JSParseFunctionEnum,
    obj::set_object_name,
    regexp::js_parse_regexp,
    state::{
        js_parse_get_pos, JSParsePos, JSParseState, PF_POW_FORBIDDEN, SKIP_HAS_ASSIGNMENT,
        SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI,
    },
    statement::{js_parse_statement_or_decl, DECL_MASK_ALL},
    token::{
        is_regexp_allowed, peek_token, token_is_pseudo_keyword, update_token_ident, CHAR_BRACKET_L,
        CHAR_BRACKET_R, CHAR_CURLY_L, CHAR_CURLY_R, CHAR_DOT, CHAR_EQUAL, CHAR_SEMI, CHAR_SLASH,
        CHAR_SQUARE_L, CHAR_SQUARE_R, TOK_CATCH, TOK_CLASS, TOK_CONST, TOK_DEBUGGER, TOK_DEC,
        TOK_DELETE, TOK_DIV_ASSIGN, TOK_DO, TOK_ELLIPSIS, TOK_ENUM, TOK_EOF, TOK_EXPORT, TOK_FALSE,
        TOK_FIRST_KEYWORD, TOK_FOR, TOK_FUNCTION, TOK_IDENT, TOK_IF, TOK_IMPORT, TOK_INC,
        TOK_INTERFACE, TOK_LAST_KEYWORD, TOK_LET, TOK_NEW, TOK_NULL, TOK_NUMBER, TOK_OF,
        TOK_PACKAGE, TOK_PRIVATE, TOK_PROTECTED, TOK_PUBLIC, TOK_REGEXP, TOK_RETURN, TOK_STATIC,
        TOK_STRING, TOK_SUPER, TOK_SWITCH, TOK_TEMPLATE, TOK_THIS, TOK_THROW, TOK_TRUE, TOK_TRY,
        TOK_TYPEOF, TOK_VAR, TOK_WHILE, TOK_WITH,
    },
    var::define_var,
};

impl JSParseState<'_> {
    pub unsafe fn parse_program(&mut self) -> Result<(), i32> {
        let fd = self.cur_func.as_mut().unwrap();

        self.next_token()?;
        self.parse_directives()?;

        fd.is_global_var = matches!(fd.eval_type, JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE)
            || fd.js_mode & JS_MODE_STRICT as u8 == 0;

        if !self.is_module {
            // hidden variable <ret> for the return value
            fd.eval_ret_idx = Some(fd.add_var(self.ctx.deref_mut(), JS_ATOM__ret_)?);
        }

        while self.token.val != TOK_EOF {
            self.parse_source_element()?;
        }

        if !self.is_module {
            // return the value of the hidden variable fd.eval_ret_idx
            emit_op(self, OpCode::GET_LOC);
            emit_u16(self, fd.eval_ret_idx.unwrap());
            emit_return(self, true);
        } else {
            emit_return(self, false);
        }

        Ok(())
    }

    pub unsafe fn parse_directives(&mut self) -> Result<(), i32> {
        if self.token.val != TOK_STRING {
            return Ok(());
        }

        let mut pos: JSParsePos = std::mem::zeroed();
        js_parse_get_pos(self, &mut pos);

        while self.token.val == TOK_STRING {
            let token_str = CStr::from_ptr(self.token.ptr.offset(1).cast())
                .to_str()
                .unwrap();
            let len: usize = (self.buf_ptr.offset_from(self.token.ptr) - 2) as usize;
            let text = format!("{:.len$}", token_str);

            self.next_token()?;

            let mut has_semi = false;
            match self.token.val {
                // ';'
                CHAR_SEMI => {
                    self.next_token()?;
                    has_semi = true;
                }
                // '}'
                CHAR_CURLY_R | TOK_EOF => {
                    has_semi = true;
                }
                TOK_NUMBER | TOK_STRING | TOK_TEMPLATE | TOK_IDENT | TOK_REGEXP | TOK_DEC
                | TOK_INC | TOK_NULL | TOK_FALSE | TOK_TRUE | TOK_IF | TOK_RETURN | TOK_VAR
                | TOK_THIS | TOK_DELETE | TOK_TYPEOF | TOK_NEW | TOK_DO | TOK_WHILE | TOK_FOR
                | TOK_SWITCH | TOK_THROW | TOK_TRY | TOK_FUNCTION | TOK_DEBUGGER | TOK_WITH
                | TOK_CLASS | TOK_CONST | TOK_ENUM | TOK_EXPORT | TOK_IMPORT | TOK_SUPER
                | TOK_INTERFACE | TOK_LET | TOK_PACKAGE | TOK_PRIVATE | TOK_PROTECTED
                | TOK_PUBLIC | TOK_STATIC => {
                    // automatic insertion of ';'
                    if self.got_lf {
                        has_semi = true;
                    }
                }
                _ => {}
            }
            if !has_semi {
                break;
            }

            match text.as_str() {
                "use strict" => {
                    (*self.cur_func).has_use_strict = true;
                    (*self.cur_func).js_mode |= JS_MODE_STRICT as u8;
                }
                "use strip" => {
                    (*self.cur_func).js_mode |= JS_MODE_STRIP as u8;
                }
                _ => {}
            }
        }

        self.seek_token(&pos)
    }

    pub unsafe fn parse_source_element(&mut self) -> Result<(), i32> {
        let tok = self.token.val;
        let fd = self.cur_func.as_ref().unwrap();

        if tok == TOK_FUNCTION
            || (token_is_pseudo_keyword(self, JS_ATOM_async)
                && peek_token(self, true) == TOK_FUNCTION)
        {
            // 'function' or 'async function'
            self.parse_function_decl(
                JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                JSFunctionKindEnum::JS_FUNC_NORMAL,
                JS_ATOM_NULL,
                self.token.ptr,
                self.token.line_num,
            )?;
        } else if tok == TOK_EXPORT && !fd.module.is_null() {
            if js_parse_export(self) != 0 {
                return Err(-1);
            }
        } else if tok == TOK_IMPORT
            && !fd.module.is_null()
            && !matches!(peek_token(self, false), CHAR_BRACKET_L | CHAR_DOT)
        {
            // the peek_token is needed to avoid confusion with 'import()' or 'import.meta'
            self.parse_import()?;
        } else {
            if js_parse_statement_or_decl(self, DECL_MASK_ALL) != 0 {
                return Err(-1);
            }
        }

        Ok(())
    }

    /// returns (atom, has_escape) result
    pub unsafe fn parse_ident(
        &mut self,
        input: *mut *const u8,
        first_char: char,
        is_private: bool,
    ) -> Result<(JSAtom, bool), i32> {
        let mut buf = SmallVec::<[u8; 128]>::new();

        if is_private {
            buf.push(b'#');
        }

        let mut has_escape = false;
        let mut c = first_char;
        let mut p = *input;
        let mut p1;

        loop {
            p1 = p;

            if c.is_ascii() {
                buf.push(c as u8);
            } else {
                let n = unicode_to_utf8(buf.as_mut_ptr().add(buf.len()), c as u32) as usize;
                buf.set_len(buf.len() + n);
            }

            // next char
            c = (*p1) as char;
            p1 = p1.add(1);

            if c == '\\' && *p1 == b'u' as u8 {
                // escape \uXXXX
                let mut p_temp = p1;
                let cp = self.parse_string_escape(&mut p_temp, 1)?;
                c = if let Some(u) = char::from_u32(cp) {
                    u
                } else {
                    js_parse_error(self, c"invalid char for ident");
                    return Err(-1);
                };
                p1 = p_temp;
                has_escape = true;
            } else if c.is_ascii() {
                let mut p_temp = p;
                c = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p_temp)?;
                p1 = p_temp;
            }

            if !Self::is_ident_next(c) {
                break;
            }

            p = p1;

            if buf.capacity() - buf.len() <= UTF8_CHAR_LEN_MAX {
                if buf.try_reserve_exact(32).is_err() {
                    return Err(-1);
                }
            }
        }

        let atom = self
            .ctx
            .deref_mut()
            .create_atom(std::str::from_utf8_unchecked(&buf))?;

        *input = p; // consume input string: move pointer forward

        Ok((atom, has_escape))
    }

    /// test if `c` is allowed in non-first char of an ident
    #[inline(always)]
    pub fn is_ident_next(c: char) -> bool {
        if c.is_ascii() {
            matches!(c, '0'..='9' | 'A'..='Z' | 'a'..='z' | '_' | '$')
        } else {
            // ZWNJ and ZWJ are accepted in identifiers
            #[cfg(feature = "all_unicode")]
            {
                unicode_xid::UnicodeXID::is_xid_continue(c) || matches!(c, '\u{200C}' | '\u{200D}')
            }
            #[cfg(not(feature = "all_unicode"))]
            {
                !c.is_whitespace() || matches!(c, '\u{200C}' | '\u{200D}')
            }
        }
    }

    /// test if `c` could be used as first char of an ident
    #[inline(always)]
    pub fn is_ident_first(c: char) -> bool {
        if c.is_ascii() {
            matches!(c,  '$' | 'A'..='Z' | 'a'..='z' | '_')
            // (lre_id_start_table_ascii[(c >> 5) as usize] >> (c & 31)) as i32 & 1
        } else {
            #[cfg(feature = "all_unicode")]
            {
                lre_is_id_start(c)
            }
            #[cfg(not(feature = "all_unicode"))]
            {
                !c.is_whitespace()
            }
        }
    }

    // allowed parse_flags: PF_IN_ACCEPTED
    pub unsafe fn parse_var(
        &mut self,
        parse_flags: i32,
        tok: i32,
        export_flag: bool,
    ) -> Result<(), i32> {
        let ctx = self.ctx;
        let fd: *mut JSFunctionDef = self.cur_func;

        loop {
            if self.token.val == TOK_IDENT {
                if self.token.u.ident.is_reserved {
                    js_parse_error_reserved_identifier(self);
                    return Err(-1);
                }

                if self.token.u.ident.atom == JS_ATOM_let && matches!(tok, TOK_LET | TOK_CONST) {
                    js_parse_error(self, c"'let' is not a valid lexical identifier");
                    return Err(-1);
                }

                #[cfg(debug_assertions)]
                let _name_refcnt = self.token.u.ident.atom.ref_count((*ctx).rt());

                let name = self.token.u.ident.atom.dup(ctx.rt());

                if self.next_token().is_err() {
                    JS_FreeAtom(ctx.as_ptr(), name);
                    return Err(-1);
                }
                if js_define_var(self, name, tok) != 0 {
                    JS_FreeAtom(ctx.as_ptr(), name);
                    return Err(-1);
                }
                if export_flag {
                    if add_export_entry(
                        self,
                        (*fd).module,
                        name,
                        name,
                        JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL,
                    )
                    .is_null()
                    {
                        JS_FreeAtom(ctx.as_ptr(), name);
                        return Err(-1);
                    }
                }

                if self.token.val == '=' as i32 {
                    if self.next_token().is_err() {
                        JS_FreeAtom(ctx.as_ptr(), name);
                        return Err(-1);
                    }
                    if tok == TOK_VAR {
                        // Must make a reference for proper `with` semantics
                        let mut opcode: i32 = 0;
                        let mut scope: i32 = 0;
                        let mut label: i32 = 0;
                        let mut name1: JSAtom = JS_ATOM_NULL;

                        emit_op_temp(self, OpCodeTemp::SCOPE_GET_VAR);
                        emit_atom_dup(self, name);
                        emit_u16(self, (*fd).scope_level as u16);

                        if self
                            .get_lvalue(
                                &mut opcode,
                                &mut scope,
                                &mut name1,
                                &mut label,
                                std::ptr::null_mut(),
                                false,
                                CHAR_EQUAL,
                            )
                            .is_err()
                        {
                            JS_FreeAtom(ctx.as_ptr(), name1);
                            JS_FreeAtom(ctx.as_ptr(), name);
                            return Err(-1);
                        }

                        if self.parse_assign_expr2(parse_flags).is_err() {
                            JS_FreeAtom(ctx.as_ptr(), name1);
                            JS_FreeAtom(ctx.as_ptr(), name);
                            return Err(-1);
                        }

                        set_object_name(self, name);

                        self.put_lvalue(
                            opcode,
                            scope,
                            name1,
                            label,
                            PutLValueEnum::PUT_LVALUE_NOKEEP,
                            false,
                        );
                    } else {
                        if self.parse_assign_expr2(parse_flags).is_err() {
                            JS_FreeAtom(ctx.as_ptr(), name);
                            return Err(-1);
                        }
                        set_object_name(self, name);
                        emit_op_temp(
                            self,
                            if tok == TOK_CONST || tok == TOK_LET {
                                OpCodeTemp::SCOPE_PUT_VAR_INIT
                            } else {
                                OpCodeTemp::SCOPE_PUT_VAR
                            },
                        );
                        emit_atom_dup(self, name);
                        emit_u16(self, (*fd).scope_level as u16);
                    }

                    #[cfg(debug_assertions)]
                    debug_assert_eq!(name.ref_count((*ctx).rt()), _name_refcnt.map(|n| n + 2));
                } else {
                    if tok == TOK_CONST {
                        js_parse_error(self, c"missing initializer for const variable");
                        JS_FreeAtom(ctx.as_ptr(), name);
                        return Err(-1);
                    } else if tok == TOK_LET {
                        // initialize lexical variable upon entering its scope
                        emit_op(self, OpCode::UNDEFINED);
                        emit_op_temp(self, OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        emit_atom_dup(self, name);
                        emit_u16(self, (*fd).scope_level as u16);
                    }
                }

                JS_FreeAtom(ctx.as_ptr(), name);
            } else {
                let mut skip_bits: i32 = 0;

                if (self.token.val == '[' as i32 || self.token.val == '{' as i32)
                    && js_parse_skip_parens_token(self, &mut skip_bits, false) == '=' as i32
                {
                    emit_op(self, OpCode::UNDEFINED);
                    if js_parse_destructuring_element(
                        self,
                        tok,
                        false,
                        true,
                        skip_bits & SKIP_HAS_ELLIPSIS,
                        true,
                    ) < 0
                    {
                        return Err(-1);
                    }
                } else {
                    js_parse_error(self, c"variable name expected");
                    return Err(-1);
                }
            }

            if self.token.val != ',' as i32 {
                break;
            }
            self.next_token()?;
        }

        Ok(())
    }

    pub unsafe fn parse_expect(&mut self, tok: i32) -> Result<(), i32> {
        if self.token.val != tok {
            /* XXX: dump token correctly in all cases */
            debug_assert!(tok > 0);
            js_parse_error(
                self,
                CStr::from_ptr(
                    format!("expecting '{}'\0", std::char::from_u32(tok as u32).unwrap())
                        .as_ptr()
                        .cast(),
                ),
            );
            Err(-1)
        } else {
            self.next_token()
        }
    }

    pub unsafe fn parse_expect_semi(&mut self) -> Result<(), i32> {
        if self.token.val != ';' as i32 {
            if matches!(self.token.val, TOK_EOF | CHAR_CURLY_R) || self.got_lf {
                // automatic insertion of ';'
                Ok(())
            } else {
                js_parse_error(self, c"expecting ';'");
                Err(-1)
            }
        } else {
            self.next_token()
        }
    }
}

pub unsafe fn skip_shebang(pp: *mut *const u8, buf_end: *const u8) {
    let mut p = *pp;

    if *p.offset(0) == b'#' && *p.offset(1) == b'!' {
        p = p.offset(2);
        while p < buf_end {
            match *p {
                b'\n' | b'\r' => {
                    break;
                }
                b if b >= 0x80 => {
                    if let Ok(c) = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p) {
                        if matches!(c, '\u{2028}' | '\u{2029}') {
                            break;
                        }
                    } else {
                        p = p.offset(1); // skip invalid UTF-8
                    }
                }
                _ => {
                    p = p.offset(1);
                }
            }
        }
        *pp = p;
    }
}

#[deprecated(note = "use JSParseState::parse_program() instead")]
pub unsafe fn js_parse_program(s: &mut JSParseState) -> i32 {
    if s.parse_program().is_err() {
        -1
    } else {
        0
    }
}

#[deprecated(note = "use JSParseState::parse_directives() instead")]
pub unsafe fn js_parse_directives(s: *mut JSParseState) -> i32 {
    if (*s).parse_directives().is_err() {
        -1
    } else {
        0
    }
}

#[deprecated(note = "use JSParseState::parse_source_element() instead")]
pub unsafe fn js_parse_source_element(s: *mut JSParseState) -> i32 {
    if (*s).parse_source_element().is_err() {
        -1
    } else {
        0
    }
}

// allowed parse_flags: PF_IN_ACCEPTED
#[deprecated(note = "use JSParseState::parse_var() instead")]
pub unsafe fn js_parse_var(
    s: *mut JSParseState,
    parse_flags: i32,
    tok: i32,
    export_flag: bool,
) -> i32 {
    (*s).parse_var(parse_flags, tok, export_flag)
        .map_or(-1, |_| 0)
}

pub unsafe fn js_define_var(s: *mut JSParseState, name: JSAtom, tok: i32) -> i32 {
    let fd = (*s).cur_func;

    if name == JS_ATOM_yield && (*fd).func_kind == JSFunctionKindEnum::JS_FUNC_GENERATOR {
        return js_parse_error(s, c"yield is a reserved identifier");
    }
    if (name == JS_ATOM_arguments || name == JS_ATOM_eval)
        && ((*fd).js_mode & JS_MODE_STRICT as u8 != 0)
    {
        return js_parse_error(s, c"invalid variable name in strict mode");
    }
    if (name == JS_ATOM_let || name == JS_ATOM_undefined) && (tok == TOK_LET || tok == TOK_CONST) {
        return js_parse_error(s, c"invalid lexical variable name");
    }

    let var_def_type = match tok {
        TOK_LET => JSVarDefEnum::JS_VAR_DEF_LET,
        TOK_CONST => JSVarDefEnum::JS_VAR_DEF_CONST,
        TOK_VAR => JSVarDefEnum::JS_VAR_DEF_VAR,
        TOK_CATCH => JSVarDefEnum::JS_VAR_DEF_CATCH,
        _ => {
            unreachable!()
        }
    };

    if define_var(s, fd, name, var_def_type) < 0 {
        -1
    } else {
        0
    }
}

#[deprecated(note = "use JSParseState::parse_expect_semi() instead")]
pub unsafe fn js_parse_expect_semi(s: *mut JSParseState) -> i32 {
    (*s).parse_expect_semi().map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::parse_expect() instead")]
pub unsafe fn js_parse_expect(s: *mut JSParseState, tok: i32) -> i32 {
    (*s).parse_expect(tok).map_or(-1, |_| 0)
}

// XXX: improve speed with early bailout
// XXX: no longer works if regexps are present. Could use previous
//   regexp parsing heuristics to handle most cases
pub unsafe fn js_parse_skip_parens_token(
    s: *mut JSParseState,
    pbits: *mut i32,
    no_line_terminator: bool,
) -> i32 {
    let mut tok: i32 = TOK_EOF;
    let mut tok_len: i32;
    let mut bits: i32 = 0;

    // protect from underflow
    let mut state = [0u8; 256];
    let mut level = 1_usize;

    let mut pos: JSParsePos = JSParsePos::default();
    js_parse_get_pos(s, &mut pos);
    let mut last_tok = 0;

    loop {
        match (*s).token.val {
            // '(',  '[',  '{'
            CHAR_BRACKET_L | CHAR_SQUARE_L | CHAR_CURLY_L => {
                if level >= state.len() {
                    break;
                }
                state[level] = (*s).token.val as u8;
                level += 1;
            }
            // ')'
            CHAR_BRACKET_R => {
                level -= 1;
                if state[level] != '(' as u8 {
                    break;
                }
            }
            // ']'
            CHAR_SQUARE_R => {
                level -= 1;
                if state[level] != '[' as u8 {
                    break;
                }
            }
            // '}'
            CHAR_CURLY_R => {
                level -= 1;
                let c = state[level] as i32;
                if c == '`' as i32 {
                    // continue the parsing of the template
                    (*s).free_token();
                    // Resume TOK_TEMPLATE parsing (s->token.line_num and
                    // s->token.ptr are OK)
                    (*s).got_lf = false;
                    (*s).last_line_num = (*s).token.line_num;
                    if (*s).parse_template_part((*s).buf_ptr).is_err() {
                        break;
                    }
                    // C2RS: goto handle_template; 转换为直接跳转到 handle_template 标签的逻辑
                    continue;
                } else if c != '{' as i32 {
                    break;
                }
            }
            // ';'
            CHAR_SEMI => {
                if level == 2 {
                    bits |= SKIP_HAS_SEMI;
                }
            }
            // '='
            CHAR_EQUAL => {
                bits |= SKIP_HAS_ASSIGNMENT;
            }
            // '/'
            CHAR_SLASH => {
                tok_len = 1;
                // parse_regexp:
                if is_regexp_allowed(last_tok) {
                    (*s).buf_ptr = (*s).buf_ptr.offset(-tok_len as _);
                    if js_parse_regexp(s) != 0 {
                        // XXX: should clear the exception
                        break;
                    }
                }
            }

            TOK_TEMPLATE => {
                // handle_template:
                if (*s).token.u.str.sep != '`' as i32 {
                    // '${' inside the template : closing '}' and continue
                    // parsing the template
                    if level >= 256 {
                        break;
                    }
                    state[level] = '`' as u8;
                    level += 1;
                }
            }
            TOK_ELLIPSIS => {
                if level == 2 {
                    bits |= SKIP_HAS_ELLIPSIS;
                }
            }
            TOK_DIV_ASSIGN => {
                tok_len = 2;
                if is_regexp_allowed(last_tok) {
                    (*s).buf_ptr = (*s).buf_ptr.offset(-tok_len as _);
                    if js_parse_regexp(s) != 0 {
                        // XXX: should clear the exception
                        break;
                    }
                }
                continue;
            }
            TOK_EOF => {
                break;
            }

            _ => {}
        }

        // last_tok is only used to recognize regexps
        last_tok = if (*s).token.val == TOK_IDENT
            && (token_is_pseudo_keyword(s, JS_ATOM_of) || token_is_pseudo_keyword(s, JS_ATOM_yield))
        {
            TOK_OF
        } else {
            (*s).token.val
        };

        if (*s).next_token().is_err() {
            // XXX: should clear the exception generated by next_token()
            break;
        }
        if level <= 1 {
            tok = (*s).token.val;
            if token_is_pseudo_keyword(s, JS_ATOM_of) {
                tok = TOK_OF;
            }
            if no_line_terminator && (*s).last_line_num != (*s).token.line_num {
                tok = '\n' as i32;
            }
            break;
        }
    }

    // done:
    if !pbits.is_null() {
        *pbits = bits;
    }
    if (*s).seek_token(&pos).is_err() {
        return -1;
    }

    tok
}

#[deprecated(note = "use JSParseState::parse_template_part() instead")]
pub unsafe fn js_parse_template_part(s: *mut JSParseState, p: *const u8) -> i32 {
    (*s).parse_template_part(p).map_or(-1, |_| 0)
}

#[deprecated(note = "use JSParseState::push_state() instead")]
pub unsafe fn push_scope(s: *mut JSParseState) -> i32 {
    (*s).push_scope().unwrap_or(-1)
}

#[deprecated(note = "use JSParseState::pop_state() instead")]
pub unsafe fn pop_scope(s: *mut JSParseState) {
    (*s).pop_scope();
}

#[deprecated(note = "use JSParseState::close_scopes() instead")]
pub unsafe fn close_scopes(s: *mut JSParseState, scope: i32, scope_stop: i32) {
    (*s).close_scopes(scope, scope_stop);
}

// XXX: remove
pub unsafe fn has_with_scope(s: *mut JSFunctionDef, mut scope_level: i32) -> bool {
    let mut s = s;

    // check if scope chain contains a with statement
    while !s.is_null() {
        let mut scope_idx = (*s).scopes[scope_level as usize].first;
        while scope_idx >= 0 {
            let vd = (*s).vars.offset(scope_idx as isize).as_mut().unwrap();
            if vd.var_name == JS_ATOM__with_ {
                return true;
            }
            scope_idx = vd.scope_next;
        }
        // check parent scopes
        scope_level = (*s).parent_scope_level;
        s = (*s).parent;
    }

    false
}

// 'c' is the first character. Return JS_ATOM_NULL in case of error
#[deprecated(note = "use JSParseState::parse_ident() instead")]
pub unsafe fn parse_ident(
    s: *mut JSParseState,
    pp: *mut *const u8,
    pident_has_escape: *mut bool,
    c: char,
    is_private: bool,
) -> JSAtom {
    let (a, esc) = (*s)
        .parse_ident(pp, c, is_private)
        .unwrap_or((JS_ATOM_NULL, false));
    *pident_has_escape = esc;
    a
}

pub unsafe fn js_parse_delete(s: *mut JSParseState) -> i32 {
    let fd = (*s).cur_func;
    let name: JSAtom;

    if (*s).next_token().is_err() {
        return -1;
    }
    if (*s).parse_unary(PF_POW_FORBIDDEN).is_err() {
        return -1;
    }

    match get_prev_opcode(fd) {
        opcode @ (opcodes::OP_get_field | opcodes::op_get_field_opt_chain) => {
            let opt_chain_label: i32 = if opcode == opcodes::op_get_field_opt_chain {
                get_u32(
                    (*fd)
                        .byte_code
                        .as_ptr()
                        .offset((*fd).last_opcode_pos as isize + 1 + 4 + 1),
                ) as i32
            } else {
                -1
            };

            name = JSAtom(get_u32(
                (*fd)
                    .byte_code
                    .as_ptr()
                    .offset((*fd).last_opcode_pos as isize + 1),
            ));

            assert!((*fd).last_opcode_pos > 0);
            (*fd).byte_code.truncate((*fd).last_opcode_pos as _);

            let val = name.to_value((*s).ctx.deref_mut(), false);
            let ret = emit_push_const(s, val, true);
            JS_FreeValue((*s).ctx.as_ptr(), val);
            JS_FreeAtom((*s).ctx.as_ptr(), name);

            if ret != 0 {
                return ret;
            }

            emit_op(s, OpCode::DELETE);

            if opt_chain_label >= 0 {
                let next_label = emit_goto(s, OpCode::GOTO, -1);
                emit_label(s, opt_chain_label);
                // if the optional chain is not taken, return 'true'
                emit_op(s, OpCode::DROP);
                emit_op(s, OpCode::PUSH_TRUE);
                emit_label(s, next_label);
            }

            (*fd).last_opcode_pos = -1;
        }
        opcodes::OP_get_array_el => {
            //(*fd).byte_code.size = (*fd).last_opcode_pos;

            // equivelent of  (*fd).byte_code.size = (*fd).last_opcode_pos;
            assert!((*fd).last_opcode_pos >= 0);
            (*fd).byte_code.truncate((*fd).last_opcode_pos as _);

            (*fd).last_opcode_pos = -1;
            emit_op(s, OpCode::DELETE);
        }
        opcodes::op_get_array_el_opt_chain => {
            let opt_chain_label: i32;
            let next_label: i32;

            let opt_chain_label = get_u32(
                (*fd)
                    .byte_code
                    .as_ptr()
                    .offset((*fd).last_opcode_pos as isize + 1 + 1),
            );

            (*fd).byte_code.truncate((*fd).last_opcode_pos as _); // (*fd).byte_code.size = (*fd).last_opcode_pos;

            emit_op(s, OpCode::DELETE);
            next_label = emit_goto(s, OpCode::GOTO, -1);
            emit_label(s, opt_chain_label as _);
            // if the optional chain is not taken, return 'true'
            emit_op(s, OpCode::DROP);
            emit_op(s, OpCode::PUSH_TRUE);
            emit_label(s, next_label);
            (*fd).last_opcode_pos = -1;
        }
        opcodes::op_scope_get_var => {
            // 'delete this': this is not a reference
            name = JSAtom(get_u32(
                (*fd)
                    .byte_code
                    .as_ptr()
                    .offset((*fd).last_opcode_pos as isize + 1),
            ));
            if name == JS_ATOM_this || name == JS_ATOM_new_target {
                // C2RS: goto ret_true 转换为直接执行 ret_true 的逻辑
                emit_op(s, OpCode::DROP);
                emit_op(s, OpCode::PUSH_TRUE);
                return 0;
            }
            if (*fd).js_mode & JS_MODE_STRICT as u8 != 0 {
                return js_parse_error(s, c"cannot delete a direct reference in strict mode");
            } else {
                (*fd).byte_code[(*fd).last_opcode_pos as usize] =
                    OpCodeTemp::SCOPE_DELETE_VAR as u8;
            }
        }
        opcodes::op_scope_get_private_field => {
            return js_parse_error(s, c"cannot delete a private class field");
        }
        opcodes::OP_get_super_value => {
            (*fd).byte_code.truncate((*fd).last_opcode_pos as _); // (*fd).byte_code.size = (*fd).last_opcode_pos;
            (*fd).last_opcode_pos = -1;
            emit_op(s, OpCode::THROW_ERROR);
            emit_atom_dup(s, JS_ATOM_NULL);
            emit_u8(s, JS_THROW_ERROR_DELETE_SUPER);
        }
        _ => {
            // ret_true:
            emit_op(s, OpCode::DROP);
            emit_op(s, OpCode::PUSH_TRUE);
        }
    }

    0
}

// if the current token is an identifier or keyword, reparse it
// according to the current function type
pub unsafe fn reparse_ident_token(s: *mut JSParseState) {
    if (*s).token.val == TOK_IDENT
        || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&(*s).token.val)
    {
        (*s).token.val = TOK_IDENT;
        (*s).token.u.ident.is_reserved = false;
        update_token_ident(s);
    }
}
