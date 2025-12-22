use crate::{
    atom::{ATOM_CONST_KEYWORD_RANGE, ATOM_CONST_STRICT_KEYWORD_RANGE, Atom},
    function::JSFunctionKindEnum,
    number::{
        ATOD_ACCEPT_BIN_OCT, ATOD_ACCEPT_LEGACY_OCTAL, ATOD_ACCEPT_SUFFIX, ATOD_ACCEPT_UNDERSCORES,
        IntOrFloat, js_atof,
    },
    runtime::js_check_stack_overflow,
    stack_frame::ExecMode,
    string::{CharCode, StringBuf, Utf8CharIter, char_is_space},
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
            Char(char),
            HasIdent(char, bool),
            ParseNumber,
            SyntaxError,
        }
        let mut todo: Option<Todo> = None;

        self.last_ptr = self.buf_ptr;
        self.got_lf = false;
        self.last_line_num = self.token.line_num;

        let mut input = if self.buf_ptr < self.input_end {
            let pos = unsafe { self.buf_ptr.offset_from(self.input.as_ptr()) as usize };
            Utf8CharIter::new(&self.input[pos..])
        } else {
            Utf8CharIter::new(&[]) // no more bytes
        };

        let base_ptr = input.start_ptr().as_ptr();

        while let Some((i, sc)) = input.next() {
            self.token.line_num = self.line_num;
            self.token.ptr = unsafe { base_ptr.add(i) };

            match sc {
                CharCode::Char('`') => {
                    if let Ok(n) = self.parse_template_part(input.remainder_bytes()) {
                        input.move_cursor_by(n as _);
                    } else {
                        todo = Some(Todo::SyntaxError);
                    }
                }
                CharCode::Char(c @ ('\'' | '\"')) => {
                    match self.parse_string(c, true, input.remainder_bytes(), None) {
                        Ok(n) => {
                            input.move_cursor_by(n as _);
                        }
                        Err(_) => {
                            todo = Some(Todo::SyntaxError);
                        }
                    }
                }
                CharCode::Char(c @ ('\r' | '\n')) => {
                    if c == '\r'
                        && let Some(b'\n') = input.peek_byte_at(0)
                    {
                        input.move_cursor_by(1);
                    }
                    self.got_lf = true;
                    self.line_num += 1;
                    continue;
                }
                // 0x0c='\f', 0x0b='\v'
                CharCode::Char('\u{000c}' | '\u{000b}' | ' ' | '\t') => {
                    continue;
                }
                CharCode::Char('/') => {
                    if let Some((_, sc2)) = input.peek() {
                        match sc2 {
                            CharCode::Char('*') => {
                                // block comment
                                input.move_cursor_by(1); // skip '*'
                                if let Some(n) = self.skip_block_comment(input.remainder_bytes()) {
                                    input.move_cursor_by(n as _);
                                    continue;
                                } else {
                                    // at end of stream, but block comment is not closed.
                                    self.throw_syntax_error("unexpected end of comment");
                                    self.token.val = TOK_ERROR;
                                    self.token.data = TokenData::None;
                                    return Err(-1);
                                }
                            }
                            CharCode::Char('/') => {
                                // line comment
                                input.move_cursor_by(1); // skip '/'
                                self.skip_line_comment(&mut input);
                                continue;
                            }
                            CharCode::Char('=') => {
                                // /=
                                input.move_cursor_by(1); // skip '='
                                self.token.val = TOK_DIV_ASSIGN;
                                self.token.data = TokenData::None;
                            }
                            _ => {
                                self.token.val = b'/' as i32;
                                self.token.data = TokenData::Char('/')
                            }
                        }
                    }
                }
                CharCode::Char('\\') => {
                    if let Some(b'u') = input.peek_byte_at(0) {
                        // \u
                        let is_bad = if let Ok((code, consumed)) =
                            self.parse_string_escape(input.remainder_bytes(), 1)
                        {
                            if let Some(c1) = char::from_u32(code) {
                                if Self::is_ident_first(c1) {
                                    input.move_cursor_by(consumed as _);
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
                        };

                        if is_bad {
                            self.throw_syntax_error("Invalid Unicode escape sequence");
                            return Err(-1);
                        }
                    } else {
                        todo = Some(Todo::Char('\\'));
                    }
                }
                CharCode::Char(ch @ ('a'..='z' | 'A'..='Z' | '_' | '$')) => {
                    // start of identifier
                    todo = Some(Todo::HasIdent(ch, false));
                }
                CharCode::Char('#') => {
                    // private name
                    if let Some((_, c)) = input.next() {
                        let c1 = match c {
                            CharCode::Char('\\') => {
                                if let Some(b'u') = input.peek_byte_at(0) {
                                    // \u
                                    let (code, consumed) =
                                        self.parse_string_escape(input.remainder_bytes(), 1)?;
                                    if let Some(x) = char::from_u32(code) {
                                        input.move_cursor_by(consumed as _);
                                        x
                                    } else {
                                        self.throw_syntax_error("Invalid Unicode escape sequence");
                                        return Err(-1);
                                    }
                                } else {
                                    '\\'
                                }
                            }
                            CharCode::Char(c) => c,
                            CharCode::Code(_) => {
                                self.throw_syntax_error("Invalid Unicode escape sequence");
                                return Err(-1);
                            }
                        };

                        if !Self::is_ident_first(c1) {
                            self.throw_syntax_error("invalid first charactor of private name");
                            todo = Some(Todo::SyntaxError);
                        } else if let Ok((atom, has_escape, consumed)) =
                            self.parse_ident(input.remainder_bytes(), c1, true)
                        {
                            self.token.val = TOK_PRIVATE_NAME;
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape,
                                is_reserved: false,
                            };
                            input.move_cursor_by(consumed as _);
                        } else {
                            todo = Some(Todo::SyntaxError);
                        }
                    }
                }
                CharCode::Char('.') => {
                    let mut bb = [0u8; 2];
                    input.peek_bytes(&mut bb);
                    match (bb[0], bb[1]) {
                        (b'.', b'.') => {
                            // ...
                            input.move_cursor_by(2);
                            self.token.val = TOK_ELLIPSIS;
                            self.token.data = TokenData::None;
                        }
                        (b'0'..=b'9', _) => {
                            input.move_cursor_by(-1); // rewind to .
                            todo = Some(Todo::ParseNumber);
                        }
                        _ => {
                            todo = Some(Todo::Char('.'));
                        }
                    }
                }
                CharCode::Char('0') => {
                    todo = if input.peek_byte_at(0).is_some_and(|b| b.is_ascii_digit())
                        && self.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
                    {
                        self.throw_syntax_error("octal literals are deprecated in strict mode");
                        Some(Todo::SyntaxError)
                    } else {
                        input.move_cursor_by(-1); // rewind to first '0'
                        Some(Todo::ParseNumber)
                    };
                }
                CharCode::Char('1'..='9') => {
                    input.move_cursor_by(-1); // rewind to first digit
                    todo = Some(Todo::ParseNumber);
                }
                CharCode::Char('*') => {
                    match input.peek_byte_at(0) {
                        Some(b'=') => {
                            input.move_cursor_by(1);
                            self.token.val = TOK_MUL_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        Some(b'*') => {
                            if let Some(b'=') = input.peek_byte_at(1) {
                                // **=
                                input.move_cursor_by(2);
                                self.token.val = TOK_POW_ASSIGN;
                            } else {
                                // **
                                input.move_cursor_by(1);
                                self.token.val = TOK_POW;
                            }
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('*'));
                        }
                    }
                }
                CharCode::Char('%') => {
                    if let Some(b'=') = input.peek_byte_at(0) {
                        input.move_cursor_by(1);
                        self.token.val = TOK_MOD_ASSIGN;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::Char('%'));
                    }
                }
                CharCode::Char('+') => match input.peek_byte_at(0) {
                    Some(b'=') => {
                        input.move_cursor_by(1);
                        self.token.val = TOK_PLUS_ASSIGN;
                        self.token.data = TokenData::None;
                    }
                    Some(b'+') => {
                        input.move_cursor_by(1);
                        self.token.val = TOK_INC;
                        self.token.data = TokenData::None;
                    }
                    _ => {
                        todo = Some(Todo::Char('+'));
                    }
                },
                CharCode::Char('-') => {
                    match input.peek_byte_at(0) {
                        Some(b'=') => {
                            input.move_cursor_by(1);
                            self.token.val = TOK_MINUS_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        Some(b'-') => {
                            if self.allow_html_comments
                                && let Some(b'>') = input.peek_byte_at(1)
                                && self.last_line_num != self.line_num
                            {
                                // Annex B: `-->` at beginning of line is an html comment end.
                                // It extends to the end of the line.
                                input.move_cursor_by(-1);
                                self.skip_line_comment(&mut input);
                                continue;
                            } else {
                                input.move_cursor_by(1);
                                self.token.val = TOK_DEC;
                                self.token.data = TokenData::None;
                            }
                        }
                        _ => {
                            todo = Some(Todo::Char('-'));
                        }
                    }
                }
                CharCode::Char('<') => {
                    match input.peek_byte_at(0) {
                        Some(b'=') => {
                            // <=
                            input.move_cursor_by(1);
                            self.token.val = TOK_LTE;
                            self.token.data = TokenData::None;
                        }
                        Some(b'<') => {
                            if let Some(b'=') = input.peek_byte_at(1) {
                                // <<=
                                input.move_cursor_by(2);
                                self.token.val = TOK_SHL_ASSIGN;
                                self.token.data = TokenData::None;
                            } else {
                                // <<
                                input.move_cursor_by(1);
                                self.token.val = TOK_SHL;
                                self.token.data = TokenData::None;
                            }
                        }
                        Some(_) if self.allow_html_comments && input.match_bytes(b"!--") => {
                            // Annex B: handle `<!--` single line html comments
                            self.skip_line_comment(&mut input);
                            continue;
                        }
                        _ => {
                            todo = Some(Todo::Char('<'));
                        }
                    }
                }
                CharCode::Char('>') => {
                    let mut bb = [0u8; 3];
                    input.peek_bytes(&mut bb);

                    match (bb[0], bb[1], bb[2]) {
                        (b'=', _, _) => {
                            // >=
                            input.move_cursor_by(1);
                            self.token.val = TOK_GTE;
                            self.token.data = TokenData::None;
                        }
                        (b'>', b'>', b'=') => {
                            // >>>=
                            input.move_cursor_by(3);
                            self.token.val = TOK_SHR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'>', b'>', _) => {
                            // >>>
                            input.move_cursor_by(2);
                            self.token.val = TOK_SHR;
                            self.token.data = TokenData::None;
                        }
                        (b'>', b'=', _) => {
                            // >>=
                            input.move_cursor_by(2);
                            self.token.val = TOK_SAR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'>', _, _) => {
                            // >>
                            input.move_cursor_by(1);
                            self.token.val = TOK_SAR;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('>'));
                        }
                    }
                }
                CharCode::Char('=') => {
                    let mut bb = [0u8; 2];
                    input.peek_bytes(&mut bb);
                    match bb[0] {
                        b'=' => {
                            if bb.starts_with(b"==") {
                                // ===
                                input.move_cursor_by(2);
                                self.token.val = TOK_STRICT_EQ;
                                self.token.data = TokenData::None;
                            } else {
                                // ==
                                input.move_cursor_by(1);
                                self.token.val = TOK_EQ;
                                self.token.data = TokenData::None;
                            }
                        }
                        b'>' => {
                            // =>
                            input.move_cursor_by(1);
                            self.token.val = TOK_ARROW;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            self.token.val = '=' as i32;
                            self.token.data = TokenData::Char('=');
                        }
                    }
                }
                CharCode::Char('!') => {
                    let mut bb = [0u8; 2];
                    input.peek_bytes(&mut bb);
                    match bb[0] {
                        b'=' => {
                            if bb.starts_with(b"==") {
                                // !==
                                input.move_cursor_by(2);
                                self.token.val = TOK_STRICT_NEQ;
                                self.token.data = TokenData::None;
                            } else {
                                // !=
                                input.move_cursor_by(1);
                                self.token.val = TOK_NEQ;
                                self.token.data = TokenData::None;
                            }
                        }
                        _ => {
                            todo = Some(Todo::Char('!'));
                        }
                    }
                }
                CharCode::Char('&') => {
                    let mut bb = [0u8; 2];
                    input.peek_bytes(&mut bb);
                    match (bb[0], bb[1]) {
                        (b'=', _) => {
                            // &=
                            input.move_cursor_by(1);
                            self.token.val = TOK_AND_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'&', b'=') => {
                            // &&=
                            input.move_cursor_by(2);
                            self.token.val = TOK_LAND_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'&', _) => {
                            // &&
                            input.move_cursor_by(1);
                            self.token.val = TOK_LAND;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('&'));
                        }
                    }
                }
                CharCode::Char('^') => {
                    if let Some(b'=') = input.peek_byte_at(0) {
                        // ^=
                        input.move_cursor_by(1);
                        self.token.val = TOK_XOR_ASSIGN;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::Char('^'))
                    }
                }
                CharCode::Char('|') => {
                    let mut bb = [0u8; 2];
                    input.peek_bytes(&mut bb);
                    match (bb[0], bb[1]) {
                        (b'=', _) => {
                            // |=
                            input.move_cursor_by(1);
                            self.token.val = TOK_OR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'|', b'=') => {
                            // ||=
                            input.move_cursor_by(2);
                            self.token.val = TOK_LOR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'|', _) => {
                            // ||
                            input.move_cursor_by(1);
                            self.token.val = TOK_LOR;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('|'));
                        }
                    }
                }
                CharCode::Char('?') => {
                    let mut bb = [0u8; 2];
                    input.peek_bytes(&mut bb);
                    match (bb[0], bb[1]) {
                        (b'?', b'=') => {
                            // ??=
                            input.move_cursor_by(2);
                            self.token.val = TOK_DOUBLE_QUESTION_MARK_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'?', _) => {
                            // ??
                            input.move_cursor_by(1);
                            self.token.val = TOK_DOUBLE_QUESTION_MARK;
                            self.token.data = TokenData::None;
                        }
                        (b'.', x) if !x.is_ascii_digit() => {
                            // ?.
                            input.move_cursor_by(1);
                            self.token.val = TOK_QUESTION_MARK_DOT;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('?'));
                        }
                    }
                }
                CharCode::Char('\u{2028}' | '\u{2029}') => {
                    // XXX: should avoid incrementing line_number, but needed to handle HTML comments
                    self.got_lf = true;
                    self.line_num += 1;
                    continue;
                }
                CharCode::Char(ch) => {
                    if char_is_space(ch) {
                        continue;
                    }

                    todo = if !ch.is_ascii() {
                        if JSParseState::is_ident_first(ch) {
                            Some(Todo::HasIdent(ch, false))
                        } else {
                            self.throw_syntax_error("unexpected character");
                            Some(Todo::SyntaxError)
                        }
                    } else {
                        Some(Todo::Char(ch))
                    };
                }

                CharCode::Code(_) => {
                    self.throw_syntax_error("Invalid Unicode escape sequence");
                    return Err(-1);
                }
            }

            if let Some(what) = todo.take() {
                match what {
                    Todo::Char(ch) => {
                        self.token.val = ch as i32;
                        self.token.data = TokenData::Char(ch);
                    }
                    Todo::HasIdent(first_char, escaped) => {
                        if let Ok((atom, has_escape, consumed)) =
                            self.parse_ident(input.remainder_bytes(), first_char, false)
                        {
                            self.token.val = TOK_IDENT;
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape: has_escape || escaped,
                                is_reserved: false,
                            };
                            input.move_cursor_by(consumed as _);

                            // check if atom is a reserved keyword.
                            // if yes, convert token value to TOK_XXX
                            self.update_token_ident();
                        } else {
                            self.token.val = TOK_ERROR;
                            self.token.data = TokenData::None;
                            return Err(-1);
                        }
                    }
                    Todo::ParseNumber => {
                        let p0 = input.current_ptr().unwrap().as_ptr();
                        let p = self.parse_number(p0).inspect_err(|_| {
                            self.token.val = TOK_ERROR;
                            self.token.data = TokenData::None;
                        })?;
                        input.move_cursor_by(unsafe { p.offset_from(p0) as _ });
                    }
                    Todo::SyntaxError => {
                        self.token.val = TOK_ERROR;
                        self.token.data = TokenData::None;
                        return Err(-1);
                    }
                }
            }

            // return token
            self.buf_ptr = input.current_ptr().map_or(self.input_end, |x| x.as_ptr());
            return Ok(self.token.val);
        }

        // reach end of stream
        self.buf_ptr = input.current_ptr().map_or(self.input_end, |x| x.as_ptr());
        self.token.val = TOK_EOF;
        self.token.data = TokenData::None;
        Ok(self.token.val)
    }

    /// returns updated str pointer
    #[inline(never)]
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

    /// when exit, cursor is exactly at the \r, \n, LS or PS char of line.
    #[inline(never)]
    pub(super) fn skip_line_comment(&self, input: &mut Utf8CharIter) {
        while let Some((_, sc)) = input.next() {
            match sc {
                CharCode::Char(c @ ('\r' | '\n' | '\u{2028}' | '\u{2029}')) => {
                    input.move_cursor_by(-(c.len_utf8() as i32)); // rewind to line terminator
                    return;
                }
                CharCode::Char(_) => {}
                CharCode::Code(_) => {}
            }
        }
    }

    #[inline(never)]
    pub(super) fn skip_block_comment(&mut self, buf: &[u8]) -> Option<usize> {
        let mut chars = Utf8CharIter::new(buf);

        while let Some((i, sc)) = chars.next() {
            match sc {
                CharCode::Char('\n') => {
                    self.line_num += 1;
                    self.got_lf = true;
                }
                CharCode::Char('\r' | '\u{2028}' | '\u{2029}') => {
                    self.got_lf = true;
                }
                CharCode::Char('*') => {
                    if let Some(b'/') = chars.peek_byte_at(0) {
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
    pub(super) fn update_token_ident(&mut self) {
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
