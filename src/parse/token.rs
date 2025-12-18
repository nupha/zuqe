use crate::{
    atom::{ATOM_CONST_KEYWORD_RANGE, ATOM_CONST_STRICT_KEYWORD_RANGE, Atom},
    function::JSFunctionKindEnum,
    number::{
        ATOD_ACCEPT_BIN_OCT, ATOD_ACCEPT_LEGACY_OCTAL, ATOD_ACCEPT_SUFFIX, ATOD_ACCEPT_UNDERSCORES,
        IntOrFloat, js_atof,
    },
    runtime::js_check_stack_overflow,
    stack_frame::ExecMode,
    string::{
        StringBuf, StringChar, UTF8_CHAR_LEN_MAX, Utf8CharIter, char_is_space, unicode_from_utf8_v0,
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

#[derive(Default, Debug)]
pub struct Token {
    pub val: i32,
    /// line number of token start
    pub line_num: i32,
    /// raw point (within input buf) to start of token
    pub ptr: *const u8,
    pub data: TokenData,
}

impl Token {
    /// if data is ident type or is reserved keyword TOK_, returns atom.
    pub(crate) fn atom(&self) -> Option<Atom> {
        match self.val {
            TOK_IDENT | TOK_PRIVATE_NAME => self.data.ident_atom(),
            t @ (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD) => {
                // convert keyword token to atom
                debug_assert!(t - TOK_FIRST_KEYWORD + 1 > 0);
                let a = (t - TOK_FIRST_KEYWORD + 1) as u32;
                Some(Atom::new_const(a))
            }
            _ => None,
        }
    }

    // pub(crate) fn take_string(&mut self) -> Option<StringBuf> {
    //     let d = std::mem::replace(&mut self.data, TokenData::None);
    //     if let TokenData::Str { str, .. } = d {
    //         Some(str)
    //     } else {
    //         self.data = d;
    //         None
    //     }
    // }

    // pub(crate) fn string(&self) -> Option<&StringBuf> {
    //     if let TokenData::Str { str, .. } = &self.data {
    //         Some(str)
    //     } else {
    //         None
    //     }
    // }
}

#[derive(Debug, Default)]
pub enum TokenData {
    #[default]
    None,
    Char(char),
    Str {
        str: StringBuf,
        sep: i32,
    },
    Num(IntOrFloat),
    Ident {
        atom: Atom,
        has_escape: bool,
        is_reserved: bool,
    },
    Regexp(usize),
}

impl TokenData {
    /// if data is ident, returns the atom.
    pub const fn ident_atom(&self) -> Option<Atom> {
        if let Self::Ident { atom, .. } = self {
            Some(*atom)
        } else {
            None
        }
    }

    /// token data is ident type, and is reserved key word.
    pub const fn is_reserved(&self) -> bool {
        if let Self::Ident { is_reserved, .. } = self {
            *is_reserved
        } else {
            false
        }
    }

    /// token data is ident type, and has escape.
    pub const fn ident_has_escape(&self) -> bool {
        if let Self::Ident { has_escape, .. } = self {
            *has_escape
        } else {
            false
        }
    }

    pub(crate) fn take_string(&mut self) -> Option<StringBuf> {
        let d = std::mem::replace(self, TokenData::None);
        if let TokenData::Str { str, .. } = d {
            Some(str)
        } else {
            *self = d;
            None
        }
    }

    pub(crate) const fn string(&self) -> Option<&StringBuf> {
        if let TokenData::Str { str, .. } = self {
            Some(str)
        } else {
            None
        }
    }

    pub const fn string_sep(&self) -> Option<i32> {
        if let Self::Str { sep, .. } = self {
            Some(*sep)
        } else {
            None
        }
    }

    /// if data is number type, returns number value.
    pub const fn number(&self) -> Option<IntOrFloat> {
        if let Self::Num(n) = self {
            Some(*n)
        } else {
            None
        }
    }

    /// pre-compiled regexp data id
    pub const fn regexp(&self) -> Option<usize> {
        if let Self::Regexp(id) = self {
            Some(*id)
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

    /// Calling this will clear current token data.
    // HINT: DON'T INLINE THIS.
    // inline this method will be very slow due to check stack overflow,
    // and will consume large stack space for each function that call this method,
    // which will soon use up stack space, and cause stack overflow.
    #[inline(never)]
    pub(super) fn next_token(&mut self) -> Result<i32, i32> {
        if js_check_stack_overflow(&self.ctx.rt_ptr(), 0) {
            self.throw_syntax_error("stack overflow");
            return Err(-1);
        }

        self.clear_token_data();

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
            if p >= self.input_end {
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
                    if self
                        .parse_template_part(unsafe {
                            let ptr = p.offset(1);
                            std::slice::from_raw_parts(
                                ptr,
                                self.input_end.offset_from(ptr) as usize,
                            )
                        })
                        .is_err()
                    {
                        todo = Some(Todo::Fail);
                    } else {
                        p = self.buf_ptr;
                    }
                }
                c @ ('\'' | '"') => {
                    p = unsafe { p.add(1) }; // skip delimitor ' or "
                    match self.parse_string(
                        c,
                        true,
                        unsafe {
                            std::slice::from_raw_parts(p, self.input_end.offset_from(p) as usize)
                        },
                        None,
                    ) {
                        Ok(consumed) => {
                            p = unsafe { p.add(consumed) }; // next char of ending delimitor
                        }
                        Err(_) => {
                            todo = Some(Todo::Fail);
                        }
                    }
                }
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
                            p = p.offset(2); // skip '/*'
                            if let Some(n) = self.skip_block_comment(std::slice::from_raw_parts(
                                p,
                                self.input_end.offset_from(p) as usize,
                            )) {
                                p = p.add(n);
                                continue 'redo;
                            } else {
                                self.throw_syntax_error("unexpected end of comment");
                                self.token.val = TOK_ERROR;
                                self.token.data = TokenData::None;
                                return Err(-1);
                            }
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
                        let p1 = p.add(1);
                        let bytes =
                            std::slice::from_raw_parts(p1, self.input_end.offset_from(p1) as usize);

                        if let Ok((code, consumed)) = self.parse_string_escape(bytes, 1) {
                            if let Some(c1) = char::from_u32(code) {
                                if Self::is_ident_first(c1) {
                                    p = p1.add(consumed);
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
                        let bytes =
                            std::slice::from_raw_parts(p1, self.input_end.offset_from(p1) as usize);
                        let (cp, consumed) = self.parse_string_escape(bytes, 1)?;
                        p1 = p1.add(consumed);
                        // let cp = self.parse_string_escape(&mut p1, 1)?;
                        char::from_u32(cp).unwrap()
                    } else if !c.is_ascii() {
                        unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as _, &mut p1)?
                    } else {
                        c
                    };

                    if !Self::is_ident_first(c1) {
                        self.throw_syntax_error("invalid first charactor of private name");
                        todo = Some(Todo::Fail);
                    } else {
                        let bytes =
                            std::slice::from_raw_parts(p1, self.input_end.offset_from(p1) as usize);

                        if let Ok((atom, has_escape, consumed)) = self.parse_ident(bytes, c1, true)
                        {
                            self.token.val = TOK_PRIVATE_NAME;
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape,
                                is_reserved: false,
                            };
                            p = p1.add(consumed);
                        } else {
                            todo = Some(Todo::Fail);
                        }
                    }
                },
                '.' => unsafe {
                    if *p.add(1) == b'.' && *p.add(2) == b'.' {
                        // ...
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
                        && self.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
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
                            // Annex B: `-->` at beginning of line is an html comment end.
                            // It extends to the end of the line.
                            // p = self.next_token_skip_line_comment(p);
                            let n = self.skip_line_comment(std::slice::from_raw_parts(
                                p,
                                self.input_end.offset_from(p) as usize,
                            ));
                            p = p.add(n);
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
                        // Annex B: handle `<!--` single line html comments
                        // p = self.next_token_skip_line_comment(p);
                        let n = self.skip_line_comment(std::slice::from_raw_parts(
                            p,
                            self.input_end.offset_from(p) as usize,
                        ));
                        p = p.add(n);
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
                    match unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as i32, &mut p)? {
                        '\u{2028}' | '\u{2029}' => {
                            // XXX: should avoid incrementing line_number, but needed to handle HTML comments
                            self.got_lf = true;
                            self.line_num += 1;
                            continue 'redo;
                        }
                        ch if char_is_space(ch) => {
                            continue 'redo;
                        }
                        ch => {
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
                        let bytes = unsafe {
                            std::slice::from_raw_parts(p, self.input_end.offset_from(p) as usize)
                        };

                        if let Ok((atom, has_escape, advance)) =
                            self.parse_ident(bytes, first_char, false)
                        {
                            self.token.val = TOK_IDENT;
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape: has_escape || escaped,
                                is_reserved: false,
                            };
                            p = unsafe { p.add(advance) };

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
                        // p = self.next_token_skip_line_comment(p);
                        let n = self.skip_line_comment(std::slice::from_raw_parts(
                            p,
                            self.input_end.offset_from(p) as usize,
                        ));
                        p = p.add(n);
                        continue 'redo;
                    },
                    Todo::ParseNumber => {
                        p = self.parse_number(p).inspect_err(|_| {
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

        Ok(self.token.val)
    }

    /// returns updated str pointer
    #[inline(always)]
    fn parse_number(&mut self, ptr: *const u8) -> Result<*const u8, i32> {
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
            let exc_obj = self.ctx.rt_ptr().take_exception();
            self.throw(exc_obj);
        })?;

        debug_assert!(self.input_end >= p);

        // reject `10instanceof Number`
        let len = unsafe { self.input_end.offset_from(p) as usize };
        if let Some(c) = std::str::from_utf8(unsafe { std::slice::from_raw_parts(p, len) })
            .map_err(|_| {
                self.throw_syntax_error("invalid UTF-8 sequence");
                -1
            })?
            .chars()
            .next()
            && Self::is_ident_next(c)
        {
            self.throw_syntax_error("invalid number literal");
            return Err(-1);
        }

        self.token.val = TOK_NUMBER;
        self.token.data = TokenData::Num(ret);

        debug_assert!(p >= ptr);
        Ok(p)
    }

    /// returns number of skipped bytes (before but not include char \r, \n, LS or PS).
    fn skip_line_comment(&self, buf: &[u8]) -> usize {
        for (i, sc) in Utf8CharIter::new(buf) {
            match sc {
                StringChar::Char('\r' | '\n' | '\u{2028}' | '\u{2029}') => {
                    return i;
                }
                StringChar::Char(_) => {}
                StringChar::Code(_) => {}
            }
        }
        buf.len()
    }

    fn skip_block_comment(&mut self, buf: &[u8]) -> Option<usize> {
        let mut chars = Utf8CharIter::new(buf);

        while let Some((i, sc)) = chars.next() {
            match sc {
                StringChar::Char('\n') => {
                    self.line_num += 1;
                    self.got_lf = true;
                }
                StringChar::Char('\r' | '\u{2028}' | '\u{2029}') => {
                    self.got_lf = true;
                }
                StringChar::Char('*') => {
                    if let Some(b'/') = chars.peek_byte(0) {
                        return Some(i + 2); // skip '*/'
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// start to seek token from `pos`
    pub(super) fn seek_token(&mut self, pos: &JSParsePos) -> Result<i32, i32> {
        self.token.line_num = pos.last_line_num;
        self.line_num = pos.line_num;
        self.buf_ptr = pos.ptr;
        self.got_lf = pos.got_lf;

        self.next_token()
    }

    #[inline(always)]
    pub fn token_is_ident(tok: i32) -> bool {
        // Accept keywords and reserved words as property names
        tok == TOK_IDENT || (TOK_FIRST_KEYWORD..=TOK_LAST_KEYWORD).contains(&tok)
    }

    #[inline(always)]
    pub fn token_is_pseudo_keyword(&self, atom: Atom) -> bool {
        self.token.val == TOK_IDENT
            && self.token.data.ident_atom().is_some_and(|a| a == atom)
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
                fd.attrs.func_kind,
                fd.in_function_body,
                fd.parent,
                fd.attrs.exec_mode,
            )
        };

        if let TokenData::Ident {
            is_reserved,
            atom,
            has_escape,
        } = &mut self.token.data
        {
            *is_reserved = match *atom {
                Atom::CONST_yield => {
                    matches!(
                        func_kind,
                        JSFunctionKindEnum::JS_FUNC_GENERATOR
                            | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                    ) || (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                        && !in_function_body
                        && !parent.is_null()
                        && matches!(
                            unsafe { (&*parent).attrs.func_kind },
                            JSFunctionKindEnum::JS_FUNC_GENERATOR
                                | JSFunctionKindEnum::JS_FUNC_ASYNC_GENERATOR
                        ))
                }
                Atom::CONST_await => {
                    self.is_module
                        || func_kind.is_async()
                        || func_type == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT
                        || (func_type == JSParseFunctionEnum::JS_PARSE_FUNC_ARROW
                            && !in_function_body
                            && !parent.is_null()
                            && {
                                let parent = unsafe { &*parent };
                                parent.attrs.func_kind.is_async()
                                    || parent.func_type
                                        == JSParseFunctionEnum::JS_PARSE_FUNC_CLASS_STATIC_INIT
                            })
                }

                a if ATOM_CONST_KEYWORD_RANGE.contains(&a.id()) => true,
                a if ATOM_CONST_STRICT_KEYWORD_RANGE.contains(&a.id()) => {
                    js_mode.contains(ExecMode::STRICT)
                }
                _ => false,
            };

            if *is_reserved && !*has_escape {
                // convert ident token to keyword token.
                self.token.val = atom.id() as i32 - 1 + TOK_FIRST_KEYWORD;
                self.token.data = TokenData::None;
            }
        }
    }
}

/* only used for ':' and '=>', 'let' or 'function' look-ahead. *pp is
only set if TOK_IMPORT is returned */
/* XXX: handle all unicode cases */
pub(super) unsafe fn simple_next_token(pp: *mut *const u8, no_line_terminator: bool) -> i32 {
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
