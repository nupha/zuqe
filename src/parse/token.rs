use crate::{
    atom::{JSAtom, consts::*},
    context::JS_MODE_STRICT,
    function::JSFunctionKindEnum,
    number::{
        ATOD_ACCEPT_BIN_OCT, ATOD_ACCEPT_LEGACY_OCTAL, ATOD_ACCEPT_SUFFIX, ATOD_ACCEPT_UNDERSCORES,
        js_atof,
    },
    runtime::js_check_stack_overflow,
    string::{JSString, StringContent},
    unicode::{UTF8_CHAR_LEN_MAX, unicode_from_utf8},
    value::JSValue,
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

#[derive(Default, Debug, Clone)]
pub struct Token {
    pub val: i32,
    /// line number of token start
    pub line_num: i32,
    pub ptr: *const u8,
    pub data: TokenData,
}

impl Token {
    /// if data is ident type or is reserved keyword TOK_, returns atom.
    ///
    /// Note: returned atom is not duplicated, caller should clone to reference it.
    pub fn atom(&self) -> Option<JSAtom> {
        match self.val {
            TOK_IDENT | TOK_PRIVATE_NAME => self.data.ident_atom(),
            t @ (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD) => {
                // convert keyword token to atom
                let a = t - TOK_FIRST_KEYWORD + 1;
                Some(JSAtom(a as u32))
            }
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum TokenData {
    None,
    Char(char),
    Str {
        str: StringContent,
        sep: i32,
    },
    Num(JSValue),
    Ident {
        atom: JSAtom,
        has_escape: bool,
        is_reserved: bool,
    },
    Regexp {
        pattern: JSString,
        flags: JSString,
    },
}

impl Default for TokenData {
    fn default() -> Self {
        Self::None
    }
}

impl TokenData {
    /// if data is ident, returns then atom.
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
    pub fn string(&self) -> Option<&StringContent> {
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

    /// regexp pattern and flags
    pub fn regexp(&self) -> Option<(JSString, JSString)> {
        if let Self::Regexp { pattern, flags, .. } = self {
            Some((pattern.clone(), flags.clone()))
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

    // HINT: DON'T INLINE THIS.
    // inline this method will be very slow due to check stack overflow,
    // and will consume large stack space for each function that call this method,
    // which will soon use up stack space, and cause stack overflow.
    pub fn next_token(&mut self) -> Result<(), i32> {
        if js_check_stack_overflow(self.ctx.rt(), 0) {
            self.throw_syntax_error("stack overflow");
            return Err(-1);
        }

        self.clear_atom_in_token();

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        enum Todo {
            DefaultToken(char),
            SkipLineComment,
            HasIdent(char, bool),
            LineTerminator,
            ParseNumber,
            Fail,
        }
        let mut todo: Option<Todo> = None;

        self.last_ptr = self.buf_ptr;
        self.got_lf = false;
        self.last_line_num = self.token.line_num;

        // current pointer to input
        let mut p = self.buf_ptr;

        'redo: loop {
            if p >= self.buf_end {
                self.token.val = TOK_EOF;
                self.token.data = TokenData::None;
                break;
            }

            self.token.line_num = self.line_num;
            self.token.ptr = p;

            match unsafe { (*p) as char } {
                '\0' => {
                    todo = Some(Todo::DefaultToken('\0'));
                }
                '`' => {
                    if self.parse_template_part(unsafe { p.offset(1) }).is_err() {
                        todo = Some(Todo::Fail);
                    } else {
                        p = self.buf_ptr;
                    }
                }
                c @ ('\'' | '\"') => match self.parse_string(c, true, unsafe { p.add(1) }, None) {
                    Ok(p2) => {
                        p = p2;
                    }
                    Err(_) => {
                        todo = Some(Todo::Fail);
                    }
                },
                c @ ('\r' | '\n') => unsafe {
                    if c == '\r' && *p.offset(1) == b'\n' {
                        // \r\n sequence
                        p = p.offset(1);
                    }
                    p = p.offset(1);
                    todo = Some(Todo::LineTerminator);
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
                            'in_comment: loop {
                                if *p == 0 && p >= self.buf_end {
                                    self.throw_syntax_error("unexpected end of comment");
                                    self.token.val = TOK_ERROR;
                                    self.token.data = TokenData::None;
                                    return Err(-1);
                                }
                                if *p == b'*' && *p.offset(1) == b'/' {
                                    p = p.offset(2);
                                    break 'in_comment;
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
                            todo = Some(Todo::SkipLineComment);
                        }
                        b'=' => {
                            // /=
                            p = p.add(2);
                            self.token.val = TOK_DIV_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            p = p.add(1);
                            self.token.val = b'/' as i32;
                            self.token.data = TokenData::Char('/')
                        }
                    } // starts with '/'
                },
                '\\' => unsafe {
                    todo = Some(Todo::DefaultToken('\\'));

                    let bad = if *p.add(1) == b'u' {
                        let mut p1 = p.add(1);
                        if let Ok(cp) = self.parse_string_escape(&mut p1, 1) {
                            if let Some(c1) = char::from_u32(cp) {
                                if Self::is_ident_first(c1) {
                                    p = p1;
                                    todo = Some(Todo::HasIdent(c1, true));
                                    false
                                } else {
                                    true
                                }
                            } else {
                                true
                            }
                        } else {
                            true
                        }
                    } else {
                        false
                    };

                    if bad {
                        self.throw_syntax_error("Invalid Unicode escape sequence");
                        return Err(-1);
                    }
                },
                ch @ ('a'..='z' | 'A'..='Z' | '_' | '$') => unsafe {
                    // identifier
                    p = p.add(1);
                    todo = Some(Todo::HasIdent(ch, false));
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
                        todo = Some(Todo::Fail);
                    } else {
                        p = p1;
                        if let Ok((a, has_escape)) = self.parse_ident(&mut p, c1, true) {
                            self.token.val = TOK_PRIVATE_NAME;
                            self.token.data = TokenData::Ident {
                                atom: a,
                                has_escape,
                                is_reserved: false,
                            };
                        } else {
                            todo = Some(Todo::Fail);
                        }
                    }
                },
                '.' => unsafe {
                    if *p.add(1) == b'.' && *p.add(2) == b'.' {
                        p = p.add(3);
                        self.token.val = TOK_ELLIPSIS;
                        self.token.data = TokenData::None;
                    } else if (*p.add(1)).is_ascii_digit() {
                        todo = Some(Todo::ParseNumber);
                    } else {
                        todo = Some(Todo::DefaultToken('.'))
                    }
                },
                '0' => {
                    todo = if unsafe { (*p.add(1)).is_ascii_digit() }
                        && self.cur_func().js_mode & JS_MODE_STRICT as u8 != 0
                    {
                        // in strict mode, octal literals are not accepted
                        self.throw_syntax_error("octal literals are deprecated in strict mode");
                        Some(Todo::Fail)
                    } else {
                        Some(Todo::ParseNumber)
                    };
                }
                '1'..='9' => {
                    todo = Some(Todo::ParseNumber);
                }
                '*' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_MUL_ASSIGN;
                        self.token.data = TokenData::None;
                    } else if *p.offset(1) == b'*' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_POW_ASSIGN;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_POW;
                            self.token.data = TokenData::None;
                        }
                    } else {
                        todo = Some(Todo::DefaultToken('*'));
                    }
                },
                '%' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_MOD_ASSIGN;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::DefaultToken('%'));
                    }
                },
                '+' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_PLUS_ASSIGN;
                        self.token.data = TokenData::None;
                    } else if *p.offset(1) == b'+' {
                        p = p.offset(2);
                        self.token.val = TOK_INC;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::DefaultToken('+'));
                    }
                },
                '-' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_MINUS_ASSIGN;
                        self.token.data = TokenData::None;
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
                            self.token.data = TokenData::None;
                        }
                    } else {
                        todo = Some(Todo::DefaultToken('-'));
                    }
                },
                '<' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_LTE;
                        self.token.data = TokenData::None;
                    } else if *p.offset(1) == b'<' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_SHL_ASSIGN;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_SHL;
                            self.token.data = TokenData::None;
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
                        todo = Some(Todo::DefaultToken('<'));
                    }
                },
                '>' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_GTE;
                        self.token.data = TokenData::None;
                    } else if *p.offset(1) == b'>' {
                        if *p.offset(2) == b'>' {
                            if *p.offset(3) == b'=' {
                                p = p.offset(4);
                                self.token.val = TOK_SHR_ASSIGN;
                                self.token.data = TokenData::None;
                            } else {
                                p = p.offset(3);
                                self.token.val = TOK_SHR;
                                self.token.data = TokenData::None;
                            }
                        } else if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_SHR_ASSIGN;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_SAR;
                            self.token.data = TokenData::None;
                        }
                    } else {
                        todo = Some(Todo::DefaultToken('>'));
                    }
                },
                '=' => unsafe {
                    if *p.offset(1) == b'=' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_STRICT_EQ;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_EQ;
                            self.token.data = TokenData::None;
                        }
                    } else if *p.offset(1) == b'>' {
                        p = p.offset(2);
                        self.token.val = TOK_ARROW;
                        self.token.data = TokenData::None;
                    } else {
                        self.token.val = '=' as i32;
                        self.token.data = TokenData::Char('=');
                        p = p.offset(1);
                    }
                },
                '!' => unsafe {
                    if *p.offset(1) == b'=' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_STRICT_NEQ;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_NEQ;
                            self.token.data = TokenData::None;
                        }
                    } else {
                        todo = Some(Todo::DefaultToken('!'));
                    }
                },
                '&' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_AND_ASSIGN;
                        self.token.data = TokenData::None;
                    } else if *p.offset(1) == b'&' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_LAND_ASSIGN;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_LAND;
                            self.token.data = TokenData::None;
                        }
                    } else {
                        todo = Some(Todo::DefaultToken('&'));
                    }
                },
                '^' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_XOR_ASSIGN;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::DefaultToken('^'))
                    }
                },
                '|' => unsafe {
                    if *p.offset(1) == b'=' {
                        p = p.offset(2);
                        self.token.val = TOK_OR_ASSIGN;
                        self.token.data = TokenData::None;
                    } else if *p.offset(1) == b'|' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_LOR_ASSIGN;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_LOR;
                            self.token.data = TokenData::None;
                        }
                    } else {
                        todo = Some(Todo::DefaultToken('|'));
                    }
                },
                '?' => unsafe {
                    if *p.offset(1) == b'?' {
                        if *p.offset(2) == b'=' {
                            p = p.offset(3);
                            self.token.val = TOK_DOUBLE_QUESTION_MARK_ASSIGN;
                            self.token.data = TokenData::None;
                        } else {
                            p = p.offset(2);
                            self.token.val = TOK_DOUBLE_QUESTION_MARK;
                            self.token.data = TokenData::None;
                        }
                    } else if *p.offset(1) == b'.'
                        && !(*p.offset(2) >= b'0' && *p.offset(2) <= b'9')
                    {
                        p = p.offset(2);
                        self.token.val = TOK_QUESTION_MARK_DOT;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::DefaultToken('?'));
                    }
                },

                c if !c.is_ascii() => {
                    match unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p)? {
                        '\u{2028}' | '\u{2029}' => {
                            // XXX: should avoid incrementing line_number, but needed to handle HTML comments
                            self.got_lf = true;
                            self.line_num += 1;
                            continue 'redo;
                        }
                        ch => {
                            if ch.is_whitespace() {
                                continue 'redo;
                            }

                            todo = if JSParseState::is_ident_first(ch) {
                                Some(Todo::HasIdent(ch, false))
                            } else {
                                self.throw_syntax_error("unexpected character");
                                Some(Todo::Fail)
                            };
                        }
                    }
                }
                c => {
                    todo = Some(Todo::DefaultToken(c));
                }
            } // match ch

            // handle token type
            if let Some(what) = todo.take() {
                match what {
                    Todo::DefaultToken(ch) => {
                        self.token.val = ch as i32;
                        self.token.data = TokenData::Char(ch);
                        p = unsafe { p.offset(1) };
                    }
                    Todo::HasIdent(first_char, escaped) => {
                        if let Ok((atom, has_escape)) = self.parse_ident(&mut p, first_char, false)
                        {
                            self.token.val = TOK_IDENT;
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape: has_escape || escaped,
                                is_reserved: false,
                            };

                            // check if atom is a reserved keyword.
                            // if yes, convert token value to TOK_XXX
                            self.update_token_ident();
                        } else {
                            self.token.val = TOK_ERROR;
                            self.token.data = TokenData::None;
                            return Err(-1);
                        }
                    }
                    Todo::LineTerminator => {
                        self.got_lf = true;
                        self.line_num += 1;
                        continue 'redo;
                    }
                    Todo::SkipLineComment => unsafe {
                        p = self.next_token_skip_line_comment(p);
                        continue 'redo;
                    },
                    Todo::ParseNumber => {
                        p = self.next_token_parse_number(p).inspect_err(|_| {
                            self.token.val = TOK_ERROR;
                            self.token.data = TokenData::None;
                        })?;
                    }
                    Todo::Fail => {
                        self.token.val = TOK_ERROR;
                        self.token.data = TokenData::None;
                        return Err(-1);
                    }
                }
            }

            break;
        } // loop

        self.buf_ptr = p;
        Ok(())
    }

    /// returns updated str pointer
    #[inline(always)]
    fn next_token_parse_number(&mut self, ptr: *const u8) -> Result<*const u8, i32> {
        let mut p = ptr;

        let ret = unsafe {
            js_atof(
                &self.ctx,
                ptr,
                &mut p,
                0,
                ATOD_ACCEPT_BIN_OCT
                    | ATOD_ACCEPT_LEGACY_OCTAL
                    | ATOD_ACCEPT_UNDERSCORES
                    | ATOD_ACCEPT_SUFFIX,
            )
        }
        .inspect_err(|_| {
            // re-throw error as parser syntax error
            let exc_obj = self.ctx.take_exception();
            self.throw(exc_obj);
        })?;

        debug_assert!(self.buf_end >= p);

        // reject `10instanceof Number`
        let len = unsafe { self.buf_end.offset_from(p) as usize };
        if let Some(c) = std::str::from_utf8(unsafe { std::slice::from_raw_parts(p, len) })
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

        debug_assert!(p >= ptr);
        Ok(p)
    }

    /// returns new pointer
    unsafe fn next_token_skip_line_comment(&self, ptr: *const u8) -> *const u8 {
        let mut p = ptr;

        loop {
            if matches!(*p, b'\r' | b'\n') || (*p == 0 && p >= self.buf_end) {
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

    pub fn seek_token(&mut self, pos: &JSParsePos) -> Result<(), i32> {
        self.token.line_num = pos.last_line_num;
        self.line_num = pos.line_num;
        self.buf_ptr = pos.ptr;
        self.got_lf = pos.got_lf;
        self.next_token()
    }

    #[inline(always)]
    pub fn token_maybe_ident(tok: i32) -> bool {
        // Accept keywords and reserved words as property names
        tok == TOK_IDENT || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&tok)
    }

    #[inline(always)]
    pub fn token_is_pseudo_keyword(&self, atom: JSAtom) -> bool {
        self.token.val == TOK_IDENT
            && self.token.data.ident_atom().unwrap() == atom
            && !self.token.data.ident_has_escape()
    }

    /// check if token is reserved keyword or not.
    ///  if it is a keyword, convert keyword token.
    pub fn update_token_ident(&mut self) {
        if matches!(self.token.val, TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD) {
            return;
        }

        debug_assert_eq!(self.token.val, TOK_IDENT);

        let (func_type, func_kind, in_function_body, parent, js_mode) = {
            let fd = self.cur_func();
            (
                fd.func_type,
                fd.func_kind,
                fd.in_function_body,
                fd.parent,
                fd.js_mode,
            )
        };

        if let TokenData::Ident {
            is_reserved,
            atom,
            has_escape,
        } = &mut self.token.data
        {
            *is_reserved = match *atom {
                JS_ATOM_yield => {
                    matches!(
                        func_kind,
                        JSFunctionKindEnum::JS_FUNC_GENERATOR
                            | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                    ) || (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                        && !in_function_body
                        && !parent.is_null()
                        && matches!(
                            unsafe { (*parent).func_kind },
                            JSFunctionKindEnum::JS_FUNC_GENERATOR
                                | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                        ))
                }
                JS_ATOM_await => {
                    self.is_module
                        || func_kind.is_async()
                        || func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT
                        || (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                            && !in_function_body
                            && !parent.is_null()
                            && unsafe {
                                let parent = &*parent;
                                parent.func_kind.is_async()
                                    || parent.func_type
                                        == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT
                            })
                }

                a if (1..=JS_ATOM_LAST_KEYWORD.0).contains(&a.0) => true,
                a if (JS_ATOM_FIRST_STRICT_KEYWORD.0..=JS_ATOM_LAST_STRICT_KEYWORD.0)
                    .contains(&a.0) =>
                {
                    js_mode & JS_MODE_STRICT as u8 != 0
                }
                _ => false,
            };

            if *is_reserved && !*has_escape {
                // convert ident token to keyword token.
                self.token.val = atom.0 as i32 - 1 + TOK_FIRST_KEYWORD;
                self.token.data = TokenData::None;
            }
        }
    }
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
