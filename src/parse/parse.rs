use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, JSAtomRc, consts::*},
    bytecode::{OpCode, OpCodeTemp},
    context::JS_MODE_STRICT,
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::JSFunctionKindEnum,
    module::ExportType,
    string::{UTF8_CHAR_LEN_MAX, unicode_from_utf8, unicode_to_utf8},
};

use super::{
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    state::{JSParseState, SKIP_HAS_ASSIGNMENT, SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI},
    statement::DECL_MASK_ALL,
    token::{
        CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_CURLY_L, CHAR_CURLY_R, CHAR_DOT, CHAR_EQUAL,
        CHAR_SEMI, CHAR_SLASH, CHAR_SQUARE_L, CHAR_SQUARE_R, TOK_CONST, TOK_DEC, TOK_DIV_ASSIGN,
        TOK_ELLIPSIS, TOK_EOF, TOK_EXPORT, TOK_FALSE, TOK_FUNCTION, TOK_IDENT, TOK_IMPORT, TOK_INC,
        TOK_LET, TOK_NULL, TOK_NUMBER, TOK_OF, TOK_REGEXP, TOK_STRING, TOK_TEMPLATE, TOK_THIS,
        TOK_TRUE, TOK_VAR, token_keyword_to_str,
    },
};

impl JSParseState<'_> {
    /// buffer ptr skip shebang.
    pub fn skip_shebang(&mut self) {
        self.buf_ptr = skip_shebang(self.buf_ptr, self.buf_end);
    }

    pub fn parse_program(&mut self) -> Result<(), i32> {
        self.next_token()?;
        self.parse_directives()?;

        {
            let fd = self.cur_func_mut();
            fd.is_global_var = matches!(fd.eval_type, JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE)
                || fd.js_mode & JS_MODE_STRICT as u8 == 0;
        }

        if !self.is_module {
            // hidden variable <ret> for the return value
            let ctx = self.ctx.clone();
            let fd = self.cur_func_mut();
            let idx = fd.add_var(&ctx, JS_ATOM__ret_)?;
            fd.eval_ret_idx.replace(idx);
        }

        while self.token.val != TOK_EOF {
            self.parse_source_element()?;
        }

        if !self.is_module {
            // return the value of the hidden variable fd.eval_ret_idx
            self.emit_op(OpCode::GET_LOC);
            self.emit_u16(self.cur_func().eval_ret_idx.unwrap());
            self.emit_return(true);
        } else {
            self.emit_return(false);
        }

        Ok(())
    }

    pub fn parse_source_element(&mut self) -> Result<(), i32> {
        let is_module = self.cur_func().module.is_some();

        match self.token.val {
            TOK_FUNCTION => {
                // function
                self.parse_function_decl(
                    JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                    JSFunctionKindEnum::JS_FUNC_NORMAL,
                    JS_ATOM_NULL,
                    self.token.ptr,
                    self.token.line_num,
                )
            }
            TOK_EXPORT if is_module => self.parse_export(),
            // peek_token is needed to avoid confusion with 'import()' or 'import.meta'
            TOK_IMPORT
                if is_module && !matches!(self.peek_token(false), CHAR_BRACKET_L | CHAR_DOT) =>
            {
                self.parse_import()
            }
            _ => {
                if self.token_is_pseudo_keyword(JS_ATOM_async)
                    && self.peek_token(true) == TOK_FUNCTION
                {
                    // async function
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        JSFunctionKindEnum::JS_FUNC_NORMAL,
                        JS_ATOM_NULL,
                        self.token.ptr,
                        self.token.line_num,
                    )
                } else {
                    self.parse_statement_or_decl(DECL_MASK_ALL)
                }
            }
        }
    }

    /// returns (atom, has_escape)
    pub fn parse_ident(
        &mut self,
        input: *mut *const u8,
        first_char: char,
        is_private: bool,
    ) -> Result<(JSAtom, bool), i32> {
        let mut buf = Vec::<u8>::with_capacity(64);

        if is_private {
            buf.push(b'#');
        }

        let mut has_escape = false;
        let mut c = first_char;
        let mut p = unsafe { *input };
        let mut p1;

        loop {
            p1 = p;

            if c.is_ascii() {
                buf.push(c as u8);
            } else {
                unicode_to_utf8(&mut buf, c as u32);
            }

            unsafe {
                // next char
                c = (*p1) as char;
                p1 = p1.add(1);
            }

            if c == '\\' && unsafe { *p1 } == b'u' {
                // escape \uXXXX
                let mut p_temp = p1;
                let cp = unsafe { self.parse_string_escape(&mut p_temp, 1)? };
                c = if let Some(u) = char::from_u32(cp) {
                    u
                } else {
                    self.throw_syntax_error("invalid char for ident");
                    return Err(-1);
                };
                p1 = p_temp;
                has_escape = true;
            } else if !c.is_ascii() {
                let mut p_temp = p;
                c = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p_temp)?;
                p1 = p_temp;
            }

            if !Self::is_ident_next(c) {
                break;
            }

            p = p1;

            if buf.capacity() - buf.len() <= UTF8_CHAR_LEN_MAX {
                buf.try_reserve_exact(32).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
            }
        }

        let atom = JSAtom::new(self.ctx.rt_mut(), unsafe {
            std::str::from_utf8_unchecked(&buf)
        })?;

        unsafe {
            *input = p; // consume input string: move pointer forward
        }

        Ok((atom, has_escape))
    }

    /// test if `c` is allowed in non-first char of an ident
    #[inline]
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
    #[inline]
    pub(super) fn is_ident_first(c: char) -> bool {
        if c.is_ascii() {
            matches!(c,  '$' | 'A'..='Z' | 'a'..='z' | '_')
        } else {
            #[cfg(feature = "all_unicode")]
            {
                unicode_xid::UnicodeXID::is_xid_start(c)
            }
            #[cfg(not(feature = "all_unicode"))]
            {
                !c.is_whitespace()
            }
        }
    }

    // allowed parse_flags: PF_IN_ACCEPTED
    pub fn parse_var(&mut self, parse_flags: i32, tok: i32, export: bool) -> Result<(), i32> {
        loop {
            if self.token.val == TOK_IDENT {
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }

                let atom = self.token.atom().unwrap();
                if atom == JS_ATOM_let && matches!(tok, TOK_LET | TOK_CONST) {
                    self.throw_syntax_error("'let' is not a valid lexical identifier");
                    return Err(-1);
                }

                let name = JSAtomRc::clone_from(self.ctx.rt(), atom);

                self.next_token()?;
                self.define_var(*name, tok)?;

                if export {
                    self.add_module_export(
                        None,
                        *name,
                        *name,
                        ExportType::JS_EXPORT_TYPE_LOCAL {
                            var_idx: u16::MAX,
                            var_ref: None,
                        },
                    )?;
                }

                if self.token.val == '=' as i32 {
                    self.next_token()?;

                    if tok == TOK_VAR {
                        // Must make a reference for proper `with` semantics
                        let mut opcode: i32 = 0;
                        let mut scope: i32 = 0;
                        let mut label: i32 = 0;
                        let mut name1: JSAtom = JS_ATOM_NULL;

                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom_dup(*name);
                        self.emit_u16(self.cur_func().scope_level as u16);

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
                            JS_FreeAtom(&self.ctx, name1);
                            return Err(-1);
                        }

                        if self.parse_assign_expr2(parse_flags).is_err() {
                            JS_FreeAtom(&self.ctx, name1);
                            return Err(-1);
                        }

                        self.set_object_name(*name);

                        self.put_lvalue(
                            opcode,
                            scope,
                            name1,
                            label,
                            PutLValueEnum::PUT_LVALUE_NOKEEP,
                            false,
                        );
                    } else {
                        self.parse_assign_expr2(parse_flags)?;
                        self.set_object_name(*name);
                        self.emit_op_temp(if tok == TOK_CONST || tok == TOK_LET {
                            OpCodeTemp::SCOPE_PUT_VAR_INIT
                        } else {
                            OpCodeTemp::SCOPE_PUT_VAR
                        });
                        self.emit_atom_dup(*name);
                        self.emit_u16(self.cur_func().scope_level as u16);
                    }
                } else {
                    if tok == TOK_CONST {
                        self.throw_syntax_error("missing initializer for const variable");
                        return Err(-1);
                    } else if tok == TOK_LET {
                        // initialize lexical variable upon entering its scope
                        self.emit_op(OpCode::UNDEFINED);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom_dup(*name);
                        self.emit_u16(self.cur_func().scope_level as u16);
                    }
                }
            } else {
                let mut skip_bits: i32 = 0;

                if matches!(self.token.val, CHAR_SQUARE_L | CHAR_CURLY_L) // '[' '{'
                    && self.parse_skip_parens_token(&mut skip_bits, false)? == '=' as i32
                {
                    // destructing
                    self.emit_op(OpCode::UNDEFINED);
                    self.parse_destructuring_element(
                        tok,
                        false,
                        true,
                        skip_bits & SKIP_HAS_ELLIPSIS,
                        true,
                    )?;
                } else {
                    self.throw_syntax_error("variable name expected");
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

    /// expect current token to be `tok`. if yes, advance to next token.
    pub fn parse_expect(&mut self, tok: i32) -> Result<i32, i32> {
        if self.token.val != tok {
            if tok > 0 {
                // is char
                self.throw_syntax_error(&format!(
                    "expecting '{}'",
                    std::char::from_u32(tok as u32).unwrap()
                ));
            } else {
                // is token
                self.throw_syntax_error(&format!(
                    "expecting '{}'",
                    token_keyword_to_str(tok).unwrap_or("<unknown>")
                ));
            }

            Err(-1)
        } else {
            self.next_token()
        }
    }

    pub fn parse_expect_semi(&mut self) -> Result<i32, i32> {
        if self.token.val != CHAR_SEMI {
            if matches!(self.token.val, TOK_EOF | CHAR_CURLY_R) || self.got_lf {
                // automatic insertion of ';'
                Ok(self.token.val)
            } else {
                self.throw_syntax_error("expecting ';'");
                Err(-1)
            }
        } else {
            self.next_token()
        }
    }

    // XXX: improve speed with early bailout
    // XXX: no longer works if regexps are present. Could use previous
    //   regexp parsing heuristics to handle most cases
    pub fn parse_skip_parens_token(
        &mut self,
        pbits: &mut i32,
        no_line_terminator: bool,
    ) -> Result<i32, i32> {
        let mut tok: i32 = TOK_EOF;
        let mut bits: i32 = 0;

        // protect from underflow
        let mut state = [0u8; 256];
        let mut level = 1_usize;

        let pos = self.get_pos();
        let mut last_tok = 0;

        loop {
            match self.token.val {
                // '(',  '[',  '{'
                CHAR_BRACKET_L | CHAR_SQUARE_L | CHAR_CURLY_L => {
                    if level >= state.len() {
                        break;
                    }
                    state[level] = self.token.val as u8;
                    level += 1;
                }
                // ')'
                CHAR_BRACKET_R => {
                    level -= 1;
                    if state[level] != b'(' {
                        break;
                    }
                }
                // ']'
                CHAR_SQUARE_R => {
                    level -= 1;
                    if state[level] != b'[' {
                        break;
                    }
                }
                // '}'
                CHAR_CURLY_R => {
                    level -= 1;
                    let c = state[level] as i32;
                    if c == '`' as i32 {
                        // continue the parsing of the template
                        self.clear_atom_in_token();
                        // Resume TOK_TEMPLATE parsing (s->token.line_num and s->token.ptr are OK)
                        self.got_lf = false;
                        self.last_line_num = self.token.line_num;
                        if self.parse_template_part(self.buf_ptr).is_err() {
                            break;
                        } else {
                            continue;
                        }
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
                t @ (TOK_DIV_ASSIGN | CHAR_SLASH) => {
                    if is_regexp_allowed(last_tok) {
                        self.buf_ptr = unsafe {
                            self.buf_ptr
                                .offset(if t == TOK_DIV_ASSIGN { -2 } else { -1 })
                        };
                        if self.parse_regexp().is_err() {
                            let _ = self.ctx.take_exception(); //clear the exception
                            break;
                        }
                    }
                }
                TOK_TEMPLATE => {
                    // handle_template:
                    if self.token.data.string_sep().unwrap() != '`' as i32 {
                        // '${' inside the template : closing '}' and continue
                        // parsing the template
                        if level >= 256 {
                            break;
                        }
                        state[level] = b'`';
                        level += 1;
                    }
                }
                TOK_EOF => {
                    break;
                }
                TOK_ELLIPSIS => {
                    if level == 2 {
                        bits |= SKIP_HAS_ELLIPSIS;
                    }
                }

                _ => {}
            }

            // last_tok is only used to recognize regexps
            last_tok = if self.token.val == TOK_IDENT
                && (self.token_is_pseudo_keyword(JS_ATOM_of)
                    || self.token_is_pseudo_keyword(JS_ATOM_yield))
            {
                TOK_OF
            } else {
                self.token.val
            };

            if self.next_token().is_err() {
                // XXX: should clear the exception generated by next_token()
                break;
            }
            if level <= 1 {
                tok = self.token.val;
                if self.token_is_pseudo_keyword(JS_ATOM_of) {
                    tok = TOK_OF;
                }
                if no_line_terminator && self.last_line_num != self.token.line_num {
                    tok = '\n' as i32;
                }
                break;
            }
        }

        // done:
        *pbits = bits;
        self.seek_token(&pos)?;
        Ok(tok)
    }
}

pub(super) fn skip_shebang(start_ptr: *const u8, end_ptr: *const u8) -> *const u8 {
    let mut p = start_ptr;

    if unsafe { *p == b'#' && *p.add(1) == b'!' } {
        unsafe {
            p = p.add(2);
        }
        while p < end_ptr {
            match unsafe { *p } {
                b'\n' | b'\r' => {
                    break;
                }
                b if b >= 0x80 => {
                    if let Ok(c) = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p) {
                        if matches!(c, '\u{2028}' | '\u{2029}') {
                            break;
                        }
                    } else {
                        p = unsafe { p.add(1) }; // skip invalid UTF-8
                    }
                }
                _ => {
                    p = unsafe { p.add(1) };
                }
            }
        }
    }

    p
}

// // if the current token is an identifier or keyword, reparse it
// // according to the current function type
// pub unsafe fn reparse_ident_token(s: *mut JSParseState) {
//     if (*s).token.val == TOK_IDENT
//         || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&(*s).token.val)
//     {
//         (*s).update_token_ident();
//     }
// }

/// return TRUE if a regexp literal is allowed after token `tok`
#[inline(always)]
pub const fn is_regexp_allowed(tok: i32) -> bool {
    // regexp may occur after
    !matches!(
        tok,
        TOK_NUMBER
            | TOK_STRING
            | TOK_REGEXP
            | TOK_DEC
            | TOK_INC
            | TOK_NULL
            | TOK_FALSE
            | TOK_TRUE
            | TOK_THIS
            | TOK_IDENT
            | CHAR_BRACKET_R
            | CHAR_SQUARE_R
            | CHAR_CURLY_R /* XXX: regexp may occur after */
    )
}
