use std::ffi::CStr;

use crate::{
    atom::{JS_FreeAtom, JSAtom, consts::*},
    context::JS_MODE_STRICT,
    function::JSFunctionKindEnum,
    regexp::is_digit,
    runtime::js_check_stack_overflow,
    unicode::{UTF8_CHAR_LEN_MAX, unicode_from_utf8},
    value::{
        ATOD_ACCEPT_BIN_OCT, ATOD_ACCEPT_LEGACY_OCTAL, ATOD_ACCEPT_SUFFIX, ATOD_ACCEPT_UNDERSCORES,
        JSValue, js_atof,
    },
};

use super::{
    function::JSParseFunctionEnum,
    state::{JSParsePos, JSParseState},
};

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

pub const TOK_FIRST_KEYWORD: i32 = TOK_NULL;
pub const TOK_LAST_KEYWORD: i32 = TOK_AWAIT;

pub const CHAR_DOT: i32 = '.' as i32;
pub const CHAR_COMMA: i32 = ',' as i32;
pub const CHAR_COLON: i32 = ':' as i32;
pub const CHAR_SEMI: i32 = ';' as i32;
pub const CHAR_PLUS: i32 = '+' as i32;
pub const CHAR_MINUS: i32 = '-' as i32;
pub const CHAR_SLASH: i32 = '/' as i32;
pub const CHAR_STAR: i32 = '*' as i32;
pub const CHAR_PERCENT: i32 = '%' as i32;
pub const CHAR_EQUAL: i32 = '=' as i32;
pub const CHAR_CURLY_L: i32 = '{' as i32;
pub const CHAR_CURLY_R: i32 = '}' as i32;
pub const CHAR_SQUARE_L: i32 = '[' as i32;
pub const CHAR_SQUARE_R: i32 = ']' as i32;
pub const CHAR_BRACKET_L: i32 = '(' as i32;
pub const CHAR_BRACKET_R: i32 = ')' as i32;
pub const CHAR_DOLLAR: i32 = '$' as i32;
pub const CHAR_NL: i32 = '\n' as i32;
pub const CHAR_CR: i32 = '\r' as i32;
pub const CHAR_BACK_SLASH: i32 = '\\' as i32;
pub const CHAR_BACKTICK: i32 = '`' as i32;

#[derive(Clone)]
pub struct JSToken {
    pub val: i32,
    pub line_num: i32, // line number of token start
    pub ptr: *const u8,
    pub data: TokenData,
}

#[derive(Debug, Clone)]
pub enum TokenData {
    Str {
        str: JSValue,
        sep: i32,
    },
    Num(JSValue),
    Ident {
        atom: JSAtom,
        has_escape: bool,
        is_reserved: bool,
    },
    Regexp {
        body: JSValue,
        flags: JSValue,
    },
}

impl TokenData {
    /// if data is ident type, returns atom.
    /// Returned atom is not duplicated. Caller must dup if need to reference it.
    pub fn ident_atom(&self) -> Option<JSAtom> {
        if let Self::Ident { atom, .. } = self {
            Some(*atom) // not duplicated here!
        } else {
            None
        }
    }

    /// token data is ident type, and is reserved key word.
    pub fn is_reserved(&self) -> bool {
        if let Self::Ident { is_reserved, .. } = self {
            *is_reserved
        } else {
            false
        }
    }

    /// token data is ident type, and has escape.
    pub fn ident_has_escape(&self) -> bool {
        if let Self::Ident { has_escape, .. } = self {
            *has_escape
        } else {
            false
        }
    }

    /// if data is string type, returns string value.
    pub fn string(&self) -> Option<&JSValue> {
        if let Self::Str { str, .. } = self {
            Some(str)
        } else {
            None
        }
    }

    pub fn string_sep(&self) -> Option<i32> {
        if let Self::Str { sep, .. } = self {
            Some(*sep)
        } else {
            None
        }
    }

    /// if data is number type, returns number value.
    pub fn number(&self) -> Option<&JSValue> {
        if let Self::Num(n) = self {
            Some(n)
        } else {
            None
        }
    }

    pub fn regexp_body(&self) -> Option<&JSValue> {
        if let Self::Regexp { body, .. } = self {
            Some(body)
        } else {
            None
        }
    }
    pub fn regexp_flags(&self) -> Option<&JSValue> {
        if let Self::Regexp { flags, .. } = self {
            Some(flags)
        } else {
            None
        }
    }
}

impl<'s> JSParseState<'s> {
    pub fn peek_token(&self, no_line_terminator: bool) -> i32 {
        let mut p = self.buf_ptr;
        unsafe { simple_next_token(std::ptr::addr_of_mut!(p), no_line_terminator) }
    }

    pub fn next_token(&mut self) -> Result<(), i32> {
        if js_check_stack_overflow(&self.ctx.rt(), 0) {
            self.throw_syntax_error("stack overflow");
            return Err(-1);
        }

        self.free_token();

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum State {
            DefaultToken(char),
            SkipLineComment,
            HasIdent(char),
            LineTerminator,
            ParseNumber,
            Fail,
        }
        let mut state: Option<State> = None;

        let mut p = self.buf_ptr;
        self.last_ptr = self.buf_ptr;
        self.got_lf = false;
        self.last_line_num = self.token.line_num;

        'redo: loop {
            self.token.line_num = self.line_num;
            self.token.ptr = p;

            match unsafe { (*p) as char } {
                '\0' => {
                    if p >= self.buf_end {
                        self.token.val = TOK_EOF;
                    } else {
                        state = Some(State::DefaultToken('\0'));
                    }
                }
                '`' => unsafe {
                    if self.parse_template_part(p.offset(1)).is_err() {
                        state = Some(State::Fail);
                    } else {
                        p = self.buf_ptr;
                    }
                },
                c @ ('\'' | '\"') => unsafe {
                    if self
                        .parse_string(c, true, p.offset(1), None, &mut p)
                        .is_err()
                    {
                        state = Some(State::Fail);
                    }
                },
                c @ ('\r' | '\n') => unsafe {
                    if c == '\r' && *p.offset(1) == b'\n' {
                        // \r\n sequence
                        p = p.offset(1);
                    }
                    p = p.offset(1);
                    state = Some(State::LineTerminator);
                },
                // 0x0c='\f', 0x0b='\v'
                '\u{000c}' | '\u{000b}' | ' ' | '\t' => unsafe {
                    p = p.add(1);
                    continue 'redo;
                },
                '/' => unsafe {
                    match *p.offset(1) {
                        b'*' => {
                            // block comment
                            p = p.offset(2);
                            'comment_loop: loop {
                                if *p == 0 && p >= self.buf_end {
                                    self.throw_syntax_error("unexpected end of comment");
                                    self.token.val = TOK_ERROR;
                                    return Err(-1);
                                }
                                if *p == b'*' && *p.offset(1) == b'/' {
                                    p = p.offset(2);
                                    break 'comment_loop;
                                }
                                if *p == b'\n' {
                                    self.line_num += 1;
                                    self.got_lf = true; /* considered as LF for ASI */
                                    p = p.offset(1);
                                } else if *p == b'\r' {
                                    self.got_lf = true; /* considered as LF for ASI */
                                    p = p.offset(1);
                                } else if *p >= 0x80 {
                                    // 处理Unicode字符
                                    if let Ok(uc) =
                                        unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p)
                                    {
                                        if matches!(uc, '\u{2028}' | '\u{2029}') {
                                            self.got_lf = true; /* considered as LF for ASI */
                                        }
                                    } else {
                                        p = p.add(1); // skip invalud utf-8
                                    }
                                } else {
                                    p = p.add(1);
                                }
                            }
                            continue 'redo;
                        }
                        b'/' => {
                            // line comment
                            p = p.add(2);
                            state = Some(State::SkipLineComment);
                        }
                        b'=' => {
                            // /=
                            p = p.add(2);
                            self.token.val = TOK_DIV_ASSIGN;
                        }
                        _ => {
                            p = p.add(1);
                            self.token.val = b'/' as i32;
                        }
                    } // starts with '/'
                },
                '\\' => unsafe {
                    state = Some(State::DefaultToken('\\'));

                    if *p.add(1) == b'u' {
                        let mut p1 = p.add(1);
                        let cp = self.parse_string_escape(&mut p1, 1)?;
                        if let Some(c1) = char::from_u32(cp) {
                            if Self::is_ident_first(c1) {
                                p = p1;
                                state = Some(State::HasIdent(c1));
                            } else {
                                // syntax error?
                                self.throw_syntax_error("syntax error");
                                return Err(-1);
                            }
                        } else {
                            self.throw_syntax_error("syntax error");
                            return Err(-1);
                        }
                    }
                },
                ch @ ('a'..='z' | 'A'..='Z' | '_' | '$') => unsafe {
                    // identifier
                    p = p.add(1);
                    state = Some(State::HasIdent(ch));
                },
                '#' => unsafe {
                    // private name
                    p = p.add(1);
                    let c = (*p) as char;
                    let mut p1 = p.add(1);

                    let c1 = if c == '\\' && *p1 == b'u' {
                        let cp = self.parse_string_escape(&mut p1, 1)?;
                        char::from_u32(cp).unwrap()
                    } else if !c.is_ascii() {
                        unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as _, &mut p1)?
                    } else {
                        c
                    };

                    if !Self::is_ident_first(c1) {
                        self.throw_syntax_error("invalid first charactor of private name");
                        state = Some(State::Fail);
                    } else {
                        p = p1;
                        if let Ok((a, _)) = self.parse_ident(&mut p, c1, true) {
                            if let TokenData::Ident { atom, .. } = &mut self.token.data {
                                *atom = a;
                            } else {
                                unreachable!()
                            };
                            self.token.val = TOK_PRIVATE_NAME;
                        } else {
                            state = Some(State::Fail);
                        }
                    }
                },
                '.' => unsafe {
                    if *p.add(1) == b'.' && *p.add(2) == b'.' {
                        p = p.add(3);
                        self.token.val = TOK_ELLIPSIS;
                    } else if (*p.add(1)).is_ascii_digit() {
                        state = Some(State::ParseNumber);
                    } else {
                        state = Some(State::DefaultToken('.'))
                    }
                },
                '0' => unsafe {
                    // in strict mode, octal literals are not accepted
                    if is_digit(*p.add(1)) && self.cur_func().js_mode & JS_MODE_STRICT as u8 != 0 {
                        self.throw_syntax_error("octal literals are deprecated in strict mode");
                        state = Some(State::Fail);
                    } else {
                        state = Some(State::ParseNumber);
                    }
                },
                '1'..='9' => {
                    state = Some(State::ParseNumber);
                }
                '*' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_MUL_ASSIGN;
                    } else if *p.offset(1) == b'*' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_POW_ASSIGN;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_POW;
                        }
                    } else {
                        state = Some(State::DefaultToken('*'));
                    }
                },
                '%' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_MOD_ASSIGN;
                    } else {
                        state = Some(State::DefaultToken('%'));
                    }
                },
                '+' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_PLUS_ASSIGN;
                    } else if *p.offset(1) == b'+' {
                        p = p.offset(2);
                        self.token.val = TOK_INC;
                    } else {
                        state = Some(State::DefaultToken('+'));
                    }
                },
                '-' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_MINUS_ASSIGN;
                    } else if *p.offset(1) == b'-' {
                        if self.allow_html_comments
                            && *p.offset(2) == b'>'
                            && self.last_line_num != self.line_num
                        {
                            /* Annex B: `-->` at beginning of line is an html comment end.
                              It extends to the end of the line.
                            */
                            p = self.next_token_skip_line_comment(p);
                            continue 'redo;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_DEC;
                        }
                    } else {
                        state = Some(State::DefaultToken('-'));
                    }
                },
                '<' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_LTE;
                    } else if *p.offset(1) == b'<' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_SHL_ASSIGN;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_SHL;
                        }
                    } else if self.allow_html_comments
                        && *p.offset(1) == b'!'
                        && *p.offset(2) == b'-'
                        && *p.offset(3) == b'-'
                    {
                        /* Annex B: handle `<!--` single line html comments */
                        p = self.next_token_skip_line_comment(p);
                        continue 'redo;
                    } else {
                        state = Some(State::DefaultToken('<'));
                    }
                },
                '>' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_GTE;
                    } else if *p.offset(1) == b'>' {
                        if *p.offset(2) == b'>' {
                            if *p.offset(3) == b'=' {
                                p = p.offset(4);
                                self.token.val = TOK_SHR_ASSIGN;
                            } else {
                                p = p.offset(3);
                                self.token.val = TOK_SHR;
                            }
                        } else if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_SHR_ASSIGN;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_SAR;
                        }
                    } else {
                        state = Some(State::DefaultToken('>'));
                    }
                },
                '=' => unsafe {
                    if *p.offset(1) == b'=' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_STRICT_EQ;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_EQ;
                        }
                    } else if *p.offset(1) == b'>' {
                        p = p.offset(2);
                        self.token.val = TOK_ARROW;
                    } else {
                        self.token.val = '=' as i32;
                        p = p.offset(1);
                    }
                },
                '!' => unsafe {
                    if *p.offset(1) == b'=' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_STRICT_NEQ;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_NEQ;
                        }
                    } else {
                        state = Some(State::DefaultToken('!'));
                    }
                },
                '&' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_AND_ASSIGN;
                    } else if *p.offset(1) == b'&' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_LAND_ASSIGN;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_LAND;
                        }
                    } else {
                        state = Some(State::DefaultToken('&'));
                    }
                },
                '^' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_XOR_ASSIGN;
                    } else {
                        state = Some(State::DefaultToken('^'))
                    }
                },
                '|' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_OR_ASSIGN;
                    } else if *p.offset(1) == b'|' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_LOR_ASSIGN;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_LOR;
                        }
                    } else {
                        state = Some(State::DefaultToken('|'));
                    }
                },
                '?' => unsafe {
                    if *p.offset(1) == b'?' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_DOUBLE_QUESTION_MARK_ASSIGN;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_DOUBLE_QUESTION_MARK;
                        }
                    } else if *p.offset(1) == b'.'
                        && !(*p.offset(2) >= b'0' && *p.offset(2) <= b'9')
                    {
                        p = p.offset(2);
                        self.token.val = TOK_QUESTION_MARK_DOT;
                    } else {
                        state = Some(State::DefaultToken('?'));
                    }
                },

                c if !c.is_ascii() => unsafe {
                    match unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p)? {
                        '\u{2028}' | '\u{2029}' => {
                            // C2RS: 处理行终止符
                            // XXX: should avoid incrementing line_number, but needed to handle HTML comments
                            self.got_lf = true;
                            self.line_num += 1;
                            continue 'redo;
                        }
                        code => {
                            if code.is_whitespace() {
                                continue 'redo;
                            } else if JSParseState::is_ident_first(code) {
                                state = Some(State::HasIdent(code));
                            } else {
                                self.throw_syntax_error("unexpected character");
                                state = Some(State::Fail);
                            }
                        }
                    }
                },
                c => {
                    state = Some(State::DefaultToken(c));
                }
            } // match ch

            // handle token type
            if let Some(what) = state.take() {
                match what {
                    State::DefaultToken(ch) => unsafe {
                        self.token.val = ch as i32;
                        p = p.offset(1);
                    },
                    State::HasIdent(first_char) => unsafe {
                        if let Ok((atom, has_escape)) = self.parse_ident(&mut p, first_char, false)
                        {
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape,
                                is_reserved: false,
                            };
                            self.token.val = TOK_IDENT;
                            // check if ident is a reserved word token.
                            // if yes convert token value to TOK_XXX
                            update_token_ident(self);
                        } else {
                            self.token.val = TOK_ERROR;
                            return Err(-1);
                        }
                    },
                    State::LineTerminator => {
                        self.got_lf = true;
                        self.line_num += 1;
                        continue 'redo;
                    }
                    State::SkipLineComment => unsafe {
                        p = self.next_token_skip_line_comment(p);
                        continue 'redo;
                    },
                    State::ParseNumber => unsafe {
                        // NOTE: p may be changed after calling next_token_parse_number()
                        if self
                            .next_token_parse_number(std::ptr::addr_of_mut!(p))
                            .is_err()
                        {
                            self.token.val = TOK_ERROR;
                            return Err(-1);
                        }
                    },
                    State::Fail => {
                        self.token.val = TOK_ERROR;
                        return Err(-1);
                    }
                }
            }

            break;
        } // loop

        self.buf_ptr = p;
        Ok(())
    }

    #[inline(always)]
    fn next_token_parse_number(&mut self, p: *mut *const u8) -> Result<(), i32> {
        let flags = ATOD_ACCEPT_BIN_OCT
            | ATOD_ACCEPT_LEGACY_OCTAL
            | ATOD_ACCEPT_UNDERSCORES
            | ATOD_ACCEPT_SUFFIX;

        let ret = unsafe { js_atof(&self.ctx, *p, p, 0, flags) };
        if ret.is_exception() {
            let exc_obj = self.ctx.take_exception();
            self.throw(exc_obj);
            return Err(-1);
        } else if let JSValue::Float64(d) = ret {
            if d.is_nan() {
                self.throw_syntax_error("invalid number literal");
                return Err(-1);
            }
        }

        // reject `10instanceof Number`
        if let Some(c) = unsafe { CStr::from_ptr(*p.cast()) }
            .to_str()
            .map_err(|_| {
                self.throw_syntax_error("invalid UTF-8 sequence");
                -1
            })?
            .chars()
            .next()
        {
            if Self::is_ident_next(c) {
                self.throw_syntax_error("invalid number literal");
                return Err(-1);
            }
        }

        self.token.val = TOK_NUMBER;
        self.token.data = TokenData::Num(ret);

        Ok(())
    }

    /// returns new pointer
    unsafe fn next_token_skip_line_comment(&self, mut p: *const u8) -> *const u8 {
        loop {
            if (*p == 0 && p >= self.buf_end) || matches!(*p, b'\r' | b'\n') {
                break;
            }

            if *p >= 0x80 {
                // 处理Unicode行终止符
                if let Ok(uc) = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p) {
                    if matches!(uc, '\u{2028}' | '\u{2029}') {
                        break;
                    }
                } else {
                    p = p.add(1); // skip invalid utf8
                }
            } else {
                p = p.add(1);
            }
        }

        p
    }

    pub fn seek_token(&mut self, sp: &JSParsePos) -> Result<(), i32> {
        self.token.line_num = sp.last_line_num;
        self.line_num = sp.line_num;
        self.buf_ptr = sp.ptr;
        self.got_lf = sp.got_lf;
        self.next_token()
    }

    /// free current token data values
    pub fn free_token(&mut self) {
        match &mut self.token.data {
            TokenData::Ident { atom, .. } => {
                JS_FreeAtom(&self.ctx, *atom);
            }
            TokenData::Str { str, .. } => *str = JSValue::UNDEFINED,
            TokenData::Num(n) => *n = JSValue::UNDEFINED,
            TokenData::Regexp { body, flags } => {
                *body = JSValue::UNDEFINED;
                *flags = JSValue::UNDEFINED;
            }
        }

        // let token = &mut self.token;
        // match token.val {
        //     TOK_NUMBER => {
        //         //token.u.num.val = JSValue::UNDEFINED;
        //     }
        //     TOK_STRING | TOK_TEMPLATE => {
        //         //token.u.str.str = JSValue::UNDEFINED;
        //     }
        //     TOK_REGEXP => {
        //         //token.u.regexp.body = JSValue::UNDEFINED;
        //         //token.u.regexp.flags = JSValue::UNDEFINED;
        //     }
        //     TOK_IDENT | TOK_PRIVATE_NAME | TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD => {
        //         JS_FreeAtom(&self.ctx, token.data.ident_atom().unwrap());
        //     }
        //     _ => {}
        // }
    }
}

#[deprecated(note = "use JSParseState::next_token() instead")]
pub unsafe fn next_token(s: *mut JSParseState) -> i32 {
    if (*s).next_token().is_err() { -1 } else { 0 }
}

#[deprecated(note = "use JSParseState::free_token() instead")]
pub unsafe fn free_token(s: *mut JSParseState, token: *mut JSToken) {
    debug_assert_eq!(&mut (*s).token as *mut JSToken, token);
    (*s).free_token();
}

pub unsafe fn token_is_ident(tok: i32) -> bool {
    /* Accept keywords and reserved words as property names */
    tok == TOK_IDENT || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&tok)
}

#[inline(always)]
pub unsafe fn token_is_pseudo_keyword(s: *mut JSParseState, atom: JSAtom) -> bool {
    (*s).token.val == TOK_IDENT
        && (*s).token.data.ident_atom().unwrap() == atom
        && !(*s).token.data.ident_has_escape()
}

/* only used for ':' and '=>', 'let' or 'function' look-ahead. *pp is
only set if TOK_IMPORT is returned */
/* XXX: handle all unicode cases */
pub unsafe fn simple_next_token(pp: *mut *const u8, no_line_terminator: bool) -> i32 {
    let mut p = *pp;

    // skip spaces and comments
    loop {
        let c = *p;
        p = p.add(1);

        match c {
            b'\r' | b'\n' => {
                if no_line_terminator {
                    return b'\n' as i32;
                }
                continue;
            }

            // 0x0c: \f, 0x0b: \v
            b' ' | b'\t' | 0x0c | 0x0b => continue,
            b'/' => {
                if *p == b'/' {
                    if no_line_terminator {
                        return b'\n' as i32;
                    }
                    while *p != 0 && *p != b'\r' && *p != b'\n' {
                        p = p.add(1);
                    }
                    continue;
                }
                if *p == b'*' {
                    p = p.add(1);
                    loop {
                        if *p == 0 {
                            break;
                        }
                        if (*p == b'\r' || *p == b'\n') && no_line_terminator {
                            return b'\n' as i32;
                        }
                        if *p == b'*' && *p.add(1) == b'/' {
                            p = p.add(2);
                            break;
                        }
                        p = p.add(1);
                    }
                    continue;
                }
            }
            b'=' => {
                if *p == b'>' {
                    p = p.add(1);
                    return TOK_ARROW;
                }
                return c as i32;
            }
            _ => {
                if JSParseState::is_ident_first(c as char) {
                    if c == b'i' {
                        if *p == b'n' && !JSParseState::is_ident_next(*p.add(1) as char) {
                            p = p.add(1);
                            return TOK_IN;
                        }
                        if *p == b'm'
                            && *p.add(1) == b'p'
                            && *p.add(2) == b'o'
                            && *p.add(3) == b'r'
                            && *p.add(4) == b't'
                            && !JSParseState::is_ident_next(*p.add(5) as char)
                        {
                            *pp = p.add(5);
                            return TOK_IMPORT;
                        }
                    } else if c == b'o'
                        && *p == b'f'
                        && !JSParseState::is_ident_next(*p.add(1) as char)
                    {
                        p = p.add(1);
                        return TOK_OF;
                    } else if c == b'e'
                        && *p == b'x'
                        && *p.add(1) == b'p'
                        && *p.add(2) == b'o'
                        && *p.add(3) == b'r'
                        && *p.add(4) == b't'
                        && !JSParseState::is_ident_next(*p.add(5) as char)
                    {
                        *pp = p.add(5);
                        return TOK_EXPORT;
                    } else if c == b'f'
                        && *p == b'u'
                        && *p.add(1) == b'n'
                        && *p.add(2) == b'c'
                        && *p.add(3) == b't'
                        && *p.add(4) == b'i'
                        && *p.add(5) == b'o'
                        && *p.add(6) == b'n'
                        && !JSParseState::is_ident_next(*p.add(7) as char)
                    {
                        p = p.add(7);
                        return TOK_FUNCTION;
                    }
                    return TOK_IDENT;
                }
                return c as i32;
            }
        }
    }
}

// convert a TOK_IDENT to a keyword when needed
pub unsafe fn update_token_ident(s: &mut JSParseState) {
    // don't change match arm order
    let is_keyword: bool = match s.token.data.ident_atom().unwrap() {
        JS_ATOM_yield => {
            let fd = s.cur_func();
            matches!(
                fd.func_kind,
                JSFunctionKindEnum::JS_FUNC_GENERATOR | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
            ) || (fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                && !fd.in_function_body
                && !fd.parent.is_null()
                && matches!(
                    (*fd.parent).func_kind,
                    JSFunctionKindEnum::JS_FUNC_GENERATOR
                        | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                ))
        }
        JS_ATOM_await => {
            let is_module = s.is_module;
            let fd = s.cur_func();

            is_module
                || fd.func_kind.is_async()
                || fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT
                || (fd.func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                    && !fd.in_function_body
                    && !fd.parent.is_null()
                    && ((*fd.parent).func_kind.is_async()
                        || (*fd.parent).func_type
                            == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT))
        }

        a if (1..=JS_ATOM_LAST_KEYWORD.0).contains(&a.0) => true,
        a if (JS_ATOM_FIRST_STRICT_KEYWORD.0..=JS_ATOM_LAST_STRICT_KEYWORD.0).contains(&a.0) => {
            s.cur_func().js_mode & JS_MODE_STRICT as u8 != 0
        }
        _ => false,
    };

    if is_keyword {
        if let TokenData::Ident {
            atom,
            has_escape,
            is_reserved,
        } = &mut s.token.data
        {
            if *has_escape {
                *is_reserved = true;
                s.token.val = TOK_IDENT;
            } else {
                // The keywords atoms are pre allocated
                s.token.val = atom.0 as i32 - 1 + TOK_FIRST_KEYWORD;
            }
        }
    }
}

/// return TRUE if a regexp literal is allowed after token `tok`
#[inline(always)]
pub const fn is_regexp_allowed(tok: i32) -> bool {
    // regexp may occur after
    matches!(
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
            | CHAR_CURLY_R
    )
}
