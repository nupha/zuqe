// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    bytecode::{OpCode, OpCodeTemp},
    eval::EvalType,
    function::FunctionKind,
    stack_frame::ExecMode,
    string::{CharCode, Utf8Stream},
};

use super::{
    expr_lvalue::PutLValueEnum,
    function::JSParseFunctionEnum,
    state::{JSParseState, SKIP_HAS_ASSIGNMENT, SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI},
    statement::DECL_MASK_ALL,
    tokenize::TokenKind,
};

impl<'s> JSParseState<'s> {
    pub fn skip_shebang(&mut self) {
        let n = skip_shebang(unsafe {
            std::slice::from_raw_parts(
                self.buf_ptr,
                self.input_end.offset_from(self.buf_ptr) as usize,
            )
        });
        self.buf_ptr = unsafe { self.buf_ptr.add(n) };
    }

    pub fn parse_program(&mut self) -> Result<(), i32> {
        self.next_token()?;
        self.parse_directives()?;

        {
            let fd = self.cur_func_mut();
            fd.is_global_var = matches!(fd.eval_type, EvalType::Global | EvalType::Module)
                || !fd.attrs.exec_mode.contains(ExecMode::STRICT);
        }

        if !self.is_module {
            // hidden variable <ret> for the return value
            let fd = self.cur_func_mut();
            let idx = fd.add_var(&self.ctx, Atom::CONST__ret_)?;
            fd.eval_ret_idx.replace(idx);
        }

        while self.token.kind != TokenKind::EOF {
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

    pub(super) fn parse_source_element(&mut self) -> Result<(), i32> {
        let is_module = self.cur_func().module.is_some();

        match self.token.kind {
            TokenKind::FUNCTION => {
                // function
                self.parse_function_decl(
                    JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                    FunctionKind::Normal,
                    self.token.ptr,
                    self.token.line_num,
                )
            }
            TokenKind::EXPORT if is_module => self.parse_export(),
            // peek_token is needed to avoid confusion with 'import()' or 'import.meta'
            TokenKind::IMPORT
                if is_module
                    && !matches!(
                        self.peek_token(false),
                        TokenKind::Char('(') | TokenKind::Char('.')
                    ) =>
            {
                self.parse_import()
            }
            _ => {
                if self.token_is_pseudo_keyword(Atom::CONST_async)
                    && self.peek_token(true) == TokenKind::FUNCTION
                {
                    // async function
                    self.parse_function_decl(
                        JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                        FunctionKind::Normal,
                        self.token.ptr,
                        self.token.line_num,
                    )
                } else {
                    self.parse_stmt_or_decl(DECL_MASK_ALL)
                }
            }
        }
    }

    /// returns (atom, has_escape, bytes_consumed)
    pub(super) fn parse_ident(
        &mut self,
        bytes: &[u8],
        first_char: char,
        is_private: bool,
    ) -> Result<(Atom, bool, usize), i32> {
        let mut buf = String::with_capacity(64);

        if is_private {
            buf.push('#');
        }
        buf.push(first_char);

        let mut has_escape = false;
        let mut consumed = 0;

        let mut it = std::str::from_utf8(bytes)
            .map_err(|_| {
                self.throw_syntax_error("invalid UTF-8 sequence");
                -1
            })?
            .char_indices()
            .peekable();

        while let Some((pos, c)) = it.next() {
            if c == '\\' && it.peek().is_some_and(|(_, c2)| *c2 == 'u') {
                // format \uXXXX
                let bb = &bytes[pos + 1..]; // start from pos+1 to skip '/'
                consumed += 1; // consumed +1 for '/'

                let (code, adv) = self.parse_string_escape(bb, 1).inspect_err(|_| {
                    self.throw_syntax_error("invalid string escape");
                })?;

                if let Some(uc) = char::from_u32(code) {
                    buf.push(uc);
                    it.nth(adv - 1); // this is 0-based
                    consumed += adv;
                    has_escape = true;
                } else {
                    self.throw_syntax_error("invalid char for ident");
                    return Err(-1);
                };
            } else if !Self::is_ident_next(c) {
                break;
            } else {
                buf.push(c);
                consumed += c.len_utf8();
            }

            if buf.capacity() - buf.len() <= 6 {
                buf.try_reserve(6).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
            }
        }

        let atom = Atom::new(self.ctx.rt_ptr(), &buf)?;
        Ok((atom, has_escape, consumed))
    }

    /// test if `c` is allowed in non-first char of an ident
    #[inline]
    pub(super) fn is_ident_next(c: char) -> bool {
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
    pub(super) fn parse_var(
        &mut self,
        parse_flags: i32,
        tok: TokenKind,
        export: bool,
    ) -> Result<(), i32> {
        loop {
            if self.token.kind == TokenKind::IDENT {
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }

                let name = self.token.atom().unwrap();
                if name == Atom::CONST_let && matches!(tok, TokenKind::LET | TokenKind::CONST) {
                    self.throw_syntax_error("'let' is not a valid lexical identifier");
                    return Err(-1);
                }

                self.next_token()?;
                self.define_var(name, tok, export)?;

                if self.token.kind == TokenKind::Char('=') {
                    self.next_token()?;

                    if tok == TokenKind::VAR {
                        // Must make a reference for proper `with` semantics
                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom(name);
                        self.emit_u16(self.cur_func().scope_level as u16);

                        let mut opcode: i32 = 0;
                        let mut scope: i32 = 0;
                        let mut label: i32 = 0;
                        let mut name1 = Atom::NULL;
                        self.get_lvalue(
                            &mut opcode,
                            &mut scope,
                            &mut name1,
                            &mut label,
                            std::ptr::null_mut(),
                            false,
                            TokenKind::Char('='),
                        )?;

                        self.parse_assign_expr2(parse_flags)?;
                        self.set_object_name(name);
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
                        self.set_object_name(name);
                        self.emit_op_temp(if matches!(tok, TokenKind::CONST | TokenKind::LET) {
                            OpCodeTemp::SCOPE_PUT_VAR_INIT
                        } else {
                            OpCodeTemp::SCOPE_PUT_VAR
                        });
                        self.emit_atom(name);
                        self.emit_u16(self.cur_func().scope_level as u16);
                    }
                } else if tok == TokenKind::CONST {
                    self.throw_syntax_error("missing initializer for const variable");
                    return Err(-1);
                } else if tok == TokenKind::LET {
                    // initialize lexical variable upon entering its scope
                    self.emit_op(OpCode::UNDEFINED);
                    self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                    self.emit_atom(name);
                    self.emit_u16(self.cur_func().scope_level as u16);
                }
            } else {
                let mut skip_bits: i32 = 0;

                if matches!(self.token.kind, TokenKind::Char('[' | '{'))
                    && self.parse_skip_parens_token(&mut skip_bits, false)? == TokenKind::Char('=')
                {
                    // destructing
                    self.emit_op(OpCode::UNDEFINED);
                    self.parse_destructuring_element(
                        tok,
                        false,
                        true,
                        skip_bits & SKIP_HAS_ELLIPSIS,
                        true,
                        export,
                    )?;
                } else {
                    self.throw_syntax_error("variable name expected");
                    return Err(-1);
                }
            }

            if self.token.kind != TokenKind::Char(',') {
                break;
            }
            self.next_token()?;
        }

        Ok(())
    }

    /// Expect current token to be `tok`:
    ///
    /// - If yes, advance to next token;
    /// - If no, throw syntax error.
    pub(super) fn parse_expect(&mut self, tok: TokenKind) -> Result<TokenKind, i32> {
        if self.token.kind != tok {
            if let TokenKind::Char(ch) = tok {
                self.throw_syntax_error(&format!("expecting '{ch}'"));
            } else {
                self.throw_syntax_error(&format!(
                    "expecting '{}'",
                    tok.keyword_str().unwrap_or("<unknown>")
                ));
            }
            Err(-1)
        } else {
            self.next_token()
        }
    }

    pub(super) fn parse_expect_semi(&mut self) -> Result<TokenKind, i32> {
        if self.token.kind != TokenKind::Char(';') {
            if matches!(self.token.kind, TokenKind::EOF | TokenKind::Char('}')) || self.got_lf {
                // automatic insertion of ';'
                Ok(self.token.kind)
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
    pub(super) fn parse_skip_parens_token(
        &mut self,
        pbits: &mut i32,
        no_line_terminator: bool,
    ) -> Result<TokenKind, i32> {
        let mut tok = TokenKind::EOF;
        let mut bits: i32 = 0;

        // protect from underflow
        let mut state = [0_u8; 128];
        let mut level = 1_usize;

        let pos = self.get_pos();
        let mut last_tok = TokenKind::default();

        loop {
            match self.token.kind {
                TokenKind::Char(c @ ('(' | '[' | '{')) => {
                    if level >= state.len() {
                        break;
                    }
                    state[level] = c as u8;
                    level += 1;
                }
                TokenKind::Char(')') => {
                    level -= 1;
                    if state[level] != b'(' {
                        break;
                    }
                }
                // ']'
                TokenKind::Char(']') => {
                    level -= 1;
                    if state[level] != b'[' {
                        break;
                    }
                }
                // '}'
                TokenKind::Char('}') => {
                    level -= 1;
                    let c = state[level];
                    if c == b'`' {
                        // continue the parsing of the template
                        self.clear_token_data();
                        // Resume TEMPLATE parsing (s->token.line_num and s->token.ptr are OK)
                        self.got_lf = false;
                        self.last_line_num = self.token.line_num;

                        if self
                            .parse_template_part(unsafe {
                                std::slice::from_raw_parts(
                                    self.buf_ptr,
                                    self.input_end.offset_from(self.buf_ptr) as usize,
                                )
                            })
                            .is_err()
                        {
                            break;
                        } else {
                            continue;
                        }
                    } else if c != b'{' {
                        break;
                    }
                }
                TokenKind::Char(';') => {
                    if level == 2 {
                        bits |= SKIP_HAS_SEMI;
                    }
                }
                TokenKind::Char('=') => {
                    bits |= SKIP_HAS_ASSIGNMENT;
                }
                TokenKind::DIV_ASSIGN | TokenKind::Char('/') => {
                    if is_regexp_syntax(last_tok) {
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

                            if self.parse_regexp().is_err() {
                                let _ = self.ctx.rt_ptr().take_exception(); //clear the exception
                                break;
                            }
                        }

                        #[cfg(not(feature = "builtin_regexp"))]
                        {
                            self.throw_syntax_error("regex not supported");
                            return Err(-1);
                        }
                    }
                }
                TokenKind::TEMPLATE => {
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
                TokenKind::EOF => {
                    break;
                }
                TokenKind::ELLIPSIS => {
                    if level == 2 {
                        bits |= SKIP_HAS_ELLIPSIS;
                    }
                }

                _ => {}
            }

            // last_tok is only used to recognize regexps
            last_tok = if self.token.kind == TokenKind::IDENT
                && (self.token_is_pseudo_keyword(Atom::CONST_of)
                    || self.token_is_pseudo_keyword(Atom::CONST_yield))
            {
                TokenKind::OF
            } else {
                self.token.kind
            };

            if self.next_token().is_err() {
                // XXX: should clear the exception generated by next_token()
                break;
            }
            if level <= 1 {
                tok = self.token.kind;
                if self.token_is_pseudo_keyword(Atom::CONST_of) {
                    tok = TokenKind::OF;
                }
                if no_line_terminator && self.last_line_num != self.token.line_num {
                    tok = TokenKind::Char('\n');
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

pub(super) fn skip_shebang(buf: &[u8]) -> usize {
    if buf.starts_with(b"#!") {
        Utf8Stream::new(&buf[2..])
            .find_map(|(span, c)| {
                if let CharCode::Char('\n' | '\r' | '\u{2028}' | '\u{2029}') = c {
                    Some(span.end + 2)
                } else {
                    None
                }
            })
            .unwrap_or(buf.len())
    } else {
        0
    }
}

/// return TRUE if a regexp literal is allowed after token `tok`
#[inline(always)]
const fn is_regexp_syntax(tok: TokenKind) -> bool {
    // regexp may occur after
    !matches!(
        tok,
        TokenKind::NUMBER
            | TokenKind::STRING
            | TokenKind::REGEXP
            | TokenKind::DEC
            | TokenKind::INC
            | TokenKind::NULL
            | TokenKind::FALSE
            | TokenKind::TRUE
            | TokenKind::THIS
            | TokenKind::IDENT
            | TokenKind::Char(')' | ']' | '}') /* XXX: regexp may occur after */
    )
}
