use smallvec::SmallVec;

use crate::{
    atom::{JS_ATOM_NULL, JS_FreeAtom, JSAtom, consts::*},
    bytecode::{OpCode, OpCodeTemp},
    context::{JS_MODE_STRICT, JS_MODE_STRIP},
    eval::{JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE},
    function::JSFunctionKindEnum,
    module::JSExportTypeEnum,
    parse::{
        expr_lvalue::PutLValueEnum,
        token::{TokenData, token_keyword_to_str},
    },
    unicode::{UTF8_CHAR_LEN_MAX, unicode_from_utf8, unicode_to_utf8},
};

use super::{
    esm::{add_export_entry, js_parse_export},
    function::JSParseFunctionEnum,
    regexp::js_parse_regexp,
    state::{JSParseState, SKIP_HAS_ASSIGNMENT, SKIP_HAS_ELLIPSIS, SKIP_HAS_SEMI},
    statement::{DECL_MASK_ALL, js_parse_statement_or_decl},
    token::{
        CHAR_BRACKET_L, CHAR_BRACKET_R, CHAR_CURLY_L, CHAR_CURLY_R, CHAR_DOT, CHAR_EQUAL,
        CHAR_SEMI, CHAR_SLASH, CHAR_SQUARE_L, CHAR_SQUARE_R, TOK_CLASS, TOK_CONST, TOK_DEBUGGER,
        TOK_DEC, TOK_DELETE, TOK_DIV_ASSIGN, TOK_DO, TOK_ELLIPSIS, TOK_ENUM, TOK_EOF, TOK_EXPORT,
        TOK_FALSE, TOK_FOR, TOK_FUNCTION, TOK_IDENT, TOK_IF, TOK_IMPORT, TOK_INC, TOK_INTERFACE,
        TOK_LET, TOK_NEW, TOK_NULL, TOK_NUMBER, TOK_OF, TOK_PACKAGE, TOK_PRIVATE, TOK_PROTECTED,
        TOK_PUBLIC, TOK_REGEXP, TOK_RETURN, TOK_STATIC, TOK_STRING, TOK_SUPER, TOK_SWITCH,
        TOK_TEMPLATE, TOK_THIS, TOK_THROW, TOK_TRUE, TOK_TRY, TOK_TYPEOF, TOK_VAR, TOK_WHILE,
        TOK_WITH, is_regexp_allowed,
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

    pub fn parse_directives(&mut self) -> Result<(), i32> {
        if !matches!(self.token.data, TokenData::Str { .. }) {
            return Ok(());
        }

        let pos = self.get_pos();

        while let TokenData::Str { str, .. } = &self.token.data {
            debug_assert_eq!(self.token.val, TOK_STRING, "{:?}", self.token);
            let text: String = str.string().unwrap().content().to_str().into_owned();

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
                    self.cur_func_mut().has_use_strict = true;
                    self.cur_func_mut().js_mode |= JS_MODE_STRICT as u8;
                }
                "use strip" => {
                    self.cur_func_mut().js_mode |= JS_MODE_STRIP as u8;
                }
                _ => {}
            }
        }

        self.seek_token(&pos)
    }

    pub fn parse_source_element(&mut self) -> Result<(), i32> {
        let tok = self.token.val;

        if tok == TOK_FUNCTION
            || (self.token_is_pseudo_keyword(JS_ATOM_async)
                && self.peek_token(true) == TOK_FUNCTION)
        {
            // 'function' or 'async function'
            self.parse_function_decl(
                JSParseFunctionEnum::JS_PARSE_FUNC_STATEMENT,
                JSFunctionKindEnum::JS_FUNC_NORMAL,
                JS_ATOM_NULL,
                self.token.ptr,
                self.token.line_num,
            )?;
        } else if tok == TOK_EXPORT && self.cur_func().module.is_some() {
            if unsafe { js_parse_export(self) != 0 } {
                return Err(-1);
            }
        } else if tok == TOK_IMPORT
            && self.cur_func().module.is_some()
            && !matches!(self.peek_token(false), CHAR_BRACKET_L | CHAR_DOT)
        {
            // the peek_token is needed to avoid confusion with 'import()' or 'import.meta'
            self.parse_import()?;
        } else {
            if unsafe { js_parse_statement_or_decl(self, DECL_MASK_ALL) != 0 } {
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

            if c == '\\' && *p1 == b'u' {
                // escape \uXXXX
                let mut p_temp = p1;
                let cp = self.parse_string_escape(&mut p_temp, 1)?;
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
                if buf.try_reserve_exact(32).is_err() {
                    return Err(-1);
                }
            }
        }

        let atom = self.ctx.create_atom(std::str::from_utf8_unchecked(&buf))?;

        *input = p; // consume input string: move pointer forward

        Ok((atom, has_escape))
    }

    /// test if `c` is allowed in non-first char of an ident
    #[inline(always)]
    pub const fn is_ident_next(c: char) -> bool {
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
    pub const fn is_ident_first(c: char) -> bool {
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
        loop {
            if self.token.val == TOK_IDENT {
                if self.token.data.is_reserved() {
                    self.throw_reserved_identifier_error();
                    return Err(-1);
                }

                if self.token.data.ident_atom().unwrap() == JS_ATOM_let
                    && matches!(tok, TOK_LET | TOK_CONST)
                {
                    self.throw_syntax_error("'let' is not a valid lexical identifier");
                    return Err(-1);
                }

                #[cfg(debug_assertions)]
                let _name_refcnt = self
                    .token
                    .data
                    .ident_atom()
                    .unwrap()
                    .ref_count(self.ctx.rt());

                let name = self.token.data.ident_atom().unwrap().dup(&self.ctx.rt());

                if self.next_token().is_err() {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }
                if self.define_var(name, tok).is_err() {
                    JS_FreeAtom(&self.ctx, name);
                    return Err(-1);
                }

                if export_flag {
                    let md = self.cur_func().module.clone().unwrap();
                    add_export_entry(
                        self,
                        &md,
                        name,
                        name,
                        JSExportTypeEnum::JS_EXPORT_TYPE_LOCAL {
                            var_idx: u16::MAX,
                            var_ref: None,
                        },
                    )
                    .inspect_err(|_| {
                        JS_FreeAtom(&self.ctx, name);
                    })?;
                }

                if self.token.val == '=' as i32 {
                    if self.next_token().is_err() {
                        JS_FreeAtom(&self.ctx, name);
                        return Err(-1);
                    }
                    if tok == TOK_VAR {
                        // Must make a reference for proper `with` semantics
                        let mut opcode: i32 = 0;
                        let mut scope: i32 = 0;
                        let mut label: i32 = 0;
                        let mut name1: JSAtom = JS_ATOM_NULL;

                        self.emit_op_temp(OpCodeTemp::SCOPE_GET_VAR);
                        self.emit_atom_dup(name);
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
                            JS_FreeAtom(&self.ctx, name);
                            return Err(-1);
                        }

                        if self.parse_assign_expr2(parse_flags).is_err() {
                            JS_FreeAtom(&self.ctx, name1);
                            JS_FreeAtom(&self.ctx, name);
                            return Err(-1);
                        }

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
                        if self.parse_assign_expr2(parse_flags).is_err() {
                            JS_FreeAtom(&self.ctx, name);
                            return Err(-1);
                        }
                        self.set_object_name(name);
                        self.emit_op_temp(if tok == TOK_CONST || tok == TOK_LET {
                            OpCodeTemp::SCOPE_PUT_VAR_INIT
                        } else {
                            OpCodeTemp::SCOPE_PUT_VAR
                        });
                        self.emit_atom_dup(name);
                        self.emit_u16(self.cur_func().scope_level as u16);
                    }
                } else {
                    if tok == TOK_CONST {
                        self.throw_syntax_error("missing initializer for const variable");
                        JS_FreeAtom(&self.ctx, name);
                        return Err(-1);
                    } else if tok == TOK_LET {
                        // initialize lexical variable upon entering its scope
                        self.emit_op(OpCode::UNDEFINED);
                        self.emit_op_temp(OpCodeTemp::SCOPE_PUT_VAR_INIT);
                        self.emit_atom_dup(name);
                        self.emit_u16(self.cur_func().scope_level as u16);
                    }
                }

                JS_FreeAtom(&self.ctx, name);
            } else {
                let mut skip_bits: i32 = 0;

                if (self.token.val == '[' as i32 || self.token.val == '{' as i32)
                    && js_parse_skip_parens_token(self, &mut skip_bits, false) == '=' as i32
                {
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

    pub fn parse_expect(&mut self, tok: i32) -> Result<(), i32> {
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

    pub fn parse_expect_semi(&mut self) -> Result<(), i32> {
        if self.token.val != CHAR_SEMI {
            if matches!(self.token.val, TOK_EOF | CHAR_CURLY_R) || self.got_lf {
                // automatic insertion of ';'
                Ok(())
            } else {
                self.throw_syntax_error("expecting ';'");
                Err(-1)
            }
        } else {
            self.next_token()
        }
    }
}

pub fn skip_shebang(start_ptr: *const u8, end_ptr: *const u8) -> *const u8 {
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

    let pos = (*s).get_pos();
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
                    (*s).clear_atom_value();
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
                    if js_parse_regexp(s.as_mut().unwrap()) != 0 {
                        // XXX: should clear the exception
                        break;
                    }
                }
            }

            TOK_TEMPLATE => {
                // handle_template:
                if (*s).token.data.string_sep().unwrap() != '`' as i32 {
                    // '${' inside the template : closing '}' and continue
                    // parsing the template
                    if level >= 256 {
                        break;
                    }
                    state[level] = b'`';
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
                    if js_parse_regexp(s.as_mut().unwrap()) != 0 {
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
            && ((*s).token_is_pseudo_keyword(JS_ATOM_of)
                || (*s).token_is_pseudo_keyword(JS_ATOM_yield))
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
            if (*s).token_is_pseudo_keyword(JS_ATOM_of) {
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

// // if the current token is an identifier or keyword, reparse it
// // according to the current function type
// pub unsafe fn reparse_ident_token(s: *mut JSParseState) {
//     if (*s).token.val == TOK_IDENT
//         || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&(*s).token.val)
//     {
//         (*s).update_token_ident();
//     }
// }
