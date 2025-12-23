use crate::{
    atom::{ATOM_CONST_KEYWORD_RANGE, ATOM_CONST_STRICT_KEYWORD_RANGE, Atom},
    bytecode::OpCode,
    function::JSFunctionKindEnum,
    number::{
        ATOD_ACCEPT_BIN_OCT, ATOD_ACCEPT_LEGACY_OCTAL, ATOD_ACCEPT_SUFFIX, ATOD_ACCEPT_UNDERSCORES,
        IntOrFloat, js_atof,
    },
    runtime::js_check_stack_overflow,
    stack_frame::ExecMode,
    string::{CharCode, StringBuf, Utf8Stream, char_is_space},
};

use super::{
    function::JSParseFunctionEnum,
    state::{JSParsePos, JSParseState},
};

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

impl TokenKind {
    /// in range Self::MUL_ASSIGN..=Self::POW_ASSIGN
    #[inline(always)]
    pub fn is_op_assign(&self) -> bool {
        (Self::MUL_ASSIGN..=Self::POW_ASSIGN).contains(self)
    }

    /// get opcode for op-assign: *=, /=, +=, -= ...
    pub const fn op_assign_opcode(&self) -> Option<OpCode> {
        // Memo: following unsafe code can calc sequence of an enum item.
        // TokenKind has repr(u8) attr.
        // use seq to get OpCode from opcode array is faster.
        // unsafe {
        //     let n = *(self as *const Self as *const u8);
        //     let b = *(&Self::MUL_ASSIGN as *const Self as *const u8);
        //     let seq = b - n;
        // }
        // const assign_opcodes: [OpCode; 12] = [
        //     OpCode::MUL,
        //     OpCode::DIV,
        //     OpCode::MOD,
        //     OpCode::ADD,
        //     OpCode::SUB,
        //     OpCode::SHL,
        //     OpCode::SAR,
        //     OpCode::SHR,
        //     OpCode::AND,
        //     OpCode::XOR,
        //     OpCode::OR,
        //     OpCode::POW,
        // ];
        // ...

        match self {
            Self::MUL_ASSIGN => Some(OpCode::MUL),
            Self::DIV_ASSIGN => Some(OpCode::DIV),
            Self::MOD_ASSIGN => Some(OpCode::MOD),
            Self::PLUS_ASSIGN => Some(OpCode::ADD),
            Self::MINUS_ASSIGN => Some(OpCode::SUB),
            Self::SHL_ASSIGN => Some(OpCode::SHL),
            Self::SAR_ASSIGN => Some(OpCode::SAR),
            Self::SHR_ASSIGN => Some(OpCode::SHR),
            Self::AND_ASSIGN => Some(OpCode::AND),
            Self::XOR_ASSIGN => Some(OpCode::XOR),
            Self::OR_ASSIGN => Some(OpCode::OR),
            Self::POW_ASSIGN => Some(OpCode::POW),
            _ => None,
        }
    }
}

#[derive(Default, Debug)]
pub(super) struct Token {
    pub kind: TokenKind,
    /// line number of token start
    pub line_num: i32,
    /// raw point (within input buf) to start of token
    pub ptr: *const u8,
    pub data: TokenData,
}

impl Token {
    /// if data is ident type or is reserved keyword, returns atom.
    pub(crate) const fn atom(&self) -> Option<Atom> {
        match self.kind {
            TokenKind::IDENT | TokenKind::PRIVATE_NAME => self.data.ident_atom(),
            t => t.keyword_atom(),
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
    #[deprecated]
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
    /// Calling this will clear current token data.
    // HINT: DON'T INLINE THIS.
    // inline this method will be very slow due to check stack overflow,
    // and will consume large stack space for each function that call this method,
    // which will soon use up stack space, and cause stack overflow.
    #[inline(never)]
    pub(super) fn next_token(&mut self) -> Result<TokenKind, i32> {
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
            Utf8Stream::new(&self.input[pos..])
        } else {
            Utf8Stream::new(&[]) // no more bytes
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
                    if self.parse_string(&mut input, c, None, true).is_err() {
                        todo = Some(Todo::SyntaxError);
                    }
                }
                CharCode::Char(c @ ('\r' | '\n')) => {
                    if c == '\r'
                        && let Some(b'\n') = input.current_byte()
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
                    if let Some((_, sc2)) = input.peek_char() {
                        match sc2 {
                            CharCode::Char('*') => {
                                // block comment
                                input.move_cursor_by(1); // skip '*'
                                if self.skip_block_comment(&mut input) {
                                    continue;
                                } else {
                                    // at end of stream, but block comment is not closed.
                                    self.throw_syntax_error("unexpected end of comment");
                                    self.token.kind = TokenKind::ERROR;
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
                                self.token.kind = TokenKind::DIV_ASSIGN;
                                self.token.data = TokenData::None;
                            }
                            _ => {
                                self.token.kind = TokenKind::Char('/');
                                self.token.data = TokenData::Char('/')
                            }
                        }
                    }
                }
                CharCode::Char('\\') => {
                    if let Some(b'u') = input.current_byte() {
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
                                if let Some(b'u') = input.current_byte() {
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
                            self.token.kind = TokenKind::PRIVATE_NAME;
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
                            self.token.kind = TokenKind::ELLIPSIS;
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
                    todo = if input.current_byte().is_some_and(|b| b.is_ascii_digit())
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
                    match input.current_byte() {
                        Some(b'=') => {
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::MUL_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        Some(b'*') => {
                            if let Some(b'=') = input.peek_byte_at(1) {
                                // **=
                                input.move_cursor_by(2);
                                self.token.kind = TokenKind::POW_ASSIGN;
                            } else {
                                // **
                                input.move_cursor_by(1);
                                self.token.kind = TokenKind::POW;
                            }
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('*'));
                        }
                    }
                }
                CharCode::Char('%') => {
                    if let Some(b'=') = input.current_byte() {
                        input.move_cursor_by(1);
                        self.token.kind = TokenKind::MOD_ASSIGN;
                        self.token.data = TokenData::None;
                    } else {
                        todo = Some(Todo::Char('%'));
                    }
                }
                CharCode::Char('+') => match input.current_byte() {
                    Some(b'=') => {
                        input.move_cursor_by(1);
                        self.token.kind = TokenKind::PLUS_ASSIGN;
                        self.token.data = TokenData::None;
                    }
                    Some(b'+') => {
                        input.move_cursor_by(1);
                        self.token.kind = TokenKind::INC;
                        self.token.data = TokenData::None;
                    }
                    _ => {
                        todo = Some(Todo::Char('+'));
                    }
                },
                CharCode::Char('-') => {
                    match input.current_byte() {
                        Some(b'=') => {
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::MINUS_ASSIGN;
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
                                self.token.kind = TokenKind::DEC;
                                self.token.data = TokenData::None;
                            }
                        }
                        _ => {
                            todo = Some(Todo::Char('-'));
                        }
                    }
                }
                CharCode::Char('<') => {
                    match input.current_byte() {
                        Some(b'=') => {
                            // <=
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::LTE;
                            self.token.data = TokenData::None;
                        }
                        Some(b'<') => {
                            if let Some(b'=') = input.peek_byte_at(1) {
                                // <<=
                                input.move_cursor_by(2);
                                self.token.kind = TokenKind::SHL_ASSIGN;
                                self.token.data = TokenData::None;
                            } else {
                                // <<
                                input.move_cursor_by(1);
                                self.token.kind = TokenKind::SHL;
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
                            self.token.kind = TokenKind::GTE;
                            self.token.data = TokenData::None;
                        }
                        (b'>', b'>', b'=') => {
                            // >>>=
                            input.move_cursor_by(3);
                            self.token.kind = TokenKind::SHR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'>', b'>', _) => {
                            // >>>
                            input.move_cursor_by(2);
                            self.token.kind = TokenKind::SHR;
                            self.token.data = TokenData::None;
                        }
                        (b'>', b'=', _) => {
                            // >>=
                            input.move_cursor_by(2);
                            self.token.kind = TokenKind::SAR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'>', _, _) => {
                            // >>
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::SAR;
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
                                self.token.kind = TokenKind::STRICT_EQ;
                                self.token.data = TokenData::None;
                            } else {
                                // ==
                                input.move_cursor_by(1);
                                self.token.kind = TokenKind::EQ;
                                self.token.data = TokenData::None;
                            }
                        }
                        b'>' => {
                            // =>
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::ARROW;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            self.token.kind = TokenKind::Char('=');
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
                                self.token.kind = TokenKind::STRICT_NEQ;
                                self.token.data = TokenData::None;
                            } else {
                                // !=
                                input.move_cursor_by(1);
                                self.token.kind = TokenKind::NEQ;
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
                            self.token.kind = TokenKind::AND_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'&', b'=') => {
                            // &&=
                            input.move_cursor_by(2);
                            self.token.kind = TokenKind::LAND_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'&', _) => {
                            // &&
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::LAND;
                            self.token.data = TokenData::None;
                        }
                        _ => {
                            todo = Some(Todo::Char('&'));
                        }
                    }
                }
                CharCode::Char('^') => {
                    if let Some(b'=') = input.current_byte() {
                        // ^=
                        input.move_cursor_by(1);
                        self.token.kind = TokenKind::XOR_ASSIGN;
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
                            self.token.kind = TokenKind::OR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'|', b'=') => {
                            // ||=
                            input.move_cursor_by(2);
                            self.token.kind = TokenKind::LOR_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'|', _) => {
                            // ||
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::LOR;
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
                            self.token.kind = TokenKind::DOUBLE_QUESTION_MARK_ASSIGN;
                            self.token.data = TokenData::None;
                        }
                        (b'?', _) => {
                            // ??
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::DOUBLE_QUESTION_MARK;
                            self.token.data = TokenData::None;
                        }
                        (b'.', x) if !x.is_ascii_digit() => {
                            // ?.
                            input.move_cursor_by(1);
                            self.token.kind = TokenKind::QUESTION_MARK_DOT;
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

                    todo = if ch.is_ascii() {
                        Some(Todo::Char(ch))
                    } else {
                        // non-ascii
                        if JSParseState::is_ident_first(ch) {
                            Some(Todo::HasIdent(ch, false))
                        } else {
                            self.throw_syntax_error("unexpected character");
                            Some(Todo::SyntaxError)
                        }
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
                        self.token.kind = TokenKind::Char(ch);
                        self.token.data = TokenData::None;
                    }
                    Todo::HasIdent(first_char, escaped) => {
                        if let Ok((atom, has_escape, consumed)) =
                            self.parse_ident(input.remainder_bytes(), first_char, false)
                        {
                            self.token.kind = TokenKind::IDENT;
                            self.token.data = TokenData::Ident {
                                atom,
                                has_escape: has_escape || escaped,
                                is_reserved: false,
                            };
                            input.move_cursor_by(consumed as _);

                            // check if atom is a reserved keyword.
                            // if yes, convert to TokenKind
                            self.update_token_ident();
                        } else {
                            self.token.kind = TokenKind::ERROR;
                            self.token.data = TokenData::None;
                            return Err(-1);
                        }
                    }
                    Todo::ParseNumber => {
                        let p0 = input.current_ptr().unwrap().as_ptr();
                        let p = self.parse_number(p0).inspect_err(|_| {
                            self.token.kind = TokenKind::ERROR;
                            self.token.data = TokenData::None;
                        })?;
                        input.move_cursor_by(unsafe { p.offset_from(p0) as _ });
                    }
                    Todo::SyntaxError => {
                        self.token.kind = TokenKind::ERROR;
                        self.token.data = TokenData::None;
                        return Err(-1);
                    }
                }
            }

            // return token
            self.buf_ptr = input.current_ptr().map_or(self.input_end, |x| x.as_ptr());
            return Ok(self.token.kind);
        }

        // reach end of stream
        self.buf_ptr = input.current_ptr().map_or(self.input_end, |x| x.as_ptr());
        self.token.kind = TokenKind::EOF;
        self.token.data = TokenData::None;
        Ok(self.token.kind)
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

        self.token.kind = TokenKind::NUMBER;
        self.token.data = TokenData::Num(ret);

        debug_assert!(p >= ptr);
        Ok(p)
    }

    /// when exit, cursor is exactly at the \r, \n, LS or PS char of line.
    #[inline(never)]
    pub(super) fn skip_line_comment(&self, input: &mut Utf8Stream) {
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

    /// when exit, cursor is exactly at next char of '*/' (returns true), or at end of stream (returns false).
    #[inline(never)]
    pub(super) fn skip_block_comment(&mut self, input: &mut Utf8Stream) -> bool {
        while let Some((_, sc)) = input.next() {
            match sc {
                CharCode::Char('\n') => {
                    self.line_num += 1;
                    self.got_lf = true;
                }
                CharCode::Char('\r' | '\u{2028}' | '\u{2029}') => {
                    self.got_lf = true;
                }
                CharCode::Char('*') => {
                    if let Some(b'/') = input.current_byte() {
                        input.move_cursor_by(1); // skip '/'
                        return true;
                    }
                }
                _ => {}
            }
        }

        false
    }

    /// start to seek token from `pos`
    pub(super) fn seek_token(&mut self, pos: &JSParsePos) -> Result<TokenKind, i32> {
        self.token.line_num = pos.last_line_num;
        self.line_num = pos.line_num;
        self.buf_ptr = pos.ptr;
        self.got_lf = pos.got_lf;

        self.next_token()
    }

    #[inline(always)]
    pub fn token_is_ident(tok: TokenKind) -> bool {
        // Accept keywords and reserved words as property names
        matches!(tok, TokenKind::IDENT) || tok.is_keyword()
    }

    #[inline(always)]
    pub fn token_is_pseudo_keyword(&self, atom: Atom) -> bool {
        self.token.kind == TokenKind::IDENT
            && self.token.data.ident_atom().is_some_and(|a| a == atom)
            && !self.token.data.ident_has_escape()
    }

    /// check if token is reserved keyword or not.
    ///  if it is a keyword, convert keyword token.
    pub(super) fn update_token_ident(&mut self) {
        if self.token.kind.is_keyword() {
            return;
        }

        debug_assert_eq!(self.token.kind, TokenKind::IDENT);

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
                self.token.kind = TokenKind::from_keyword_atom(*atom).unwrap();
                self.token.data = TokenData::None;
            }
        }
    }

    /// only used for ':' and '=>', 'let' or 'function' look-ahead.
    /// `inp` stream will update its cursor to end of token.
    pub(super) fn peek_token_internal(inp: &mut Utf8Stream, no_line_terminator: bool) -> TokenKind {
        while let Some((_, sc)) = inp.next() {
            match sc {
                CharCode::Char('\r' | '\n') => {
                    if no_line_terminator {
                        return TokenKind::Char('\n');
                    }
                }
                // 0x0c: \f, 0x0b: \v
                CharCode::Char(' ' | '\t' | '\x0c' | '\x0b') => {}
                CharCode::Char('/') => {
                    match inp.current_byte() {
                        Some(b'/') => {
                            // '//'
                            if no_line_terminator {
                                return TokenKind::Char('\n');
                            } else {
                                inp.move_cursor_by(1); // skip /
                                while inp
                                    .current_byte()
                                    .is_some_and(|x| !matches!(x, b'\r' | b'\n'))
                                {
                                    inp.move_cursor_by(1);
                                }
                            }
                        }
                        Some(b'*') => {
                            // '/*'
                            inp.move_cursor_by(1); // skip *
                            while let Some(b2) = inp.current_byte() {
                                match b2 {
                                    b'\r' | b'\n' if no_line_terminator => {
                                        return TokenKind::Char('\n');
                                    }
                                    b'*' if inp.peek_byte_at(1).is_some_and(|x| x == b'/') => {
                                        // '*/'
                                        inp.move_cursor_by(1); // skip /
                                        break;
                                    }
                                    _ => {
                                        inp.move_cursor_by(1);
                                    }
                                }
                            }
                        }
                        None => {
                            break;
                        }
                        _ => {}
                    }
                }
                CharCode::Char('=') => {
                    return if let Some(b'>') = inp.current_byte() {
                        // '=>'
                        inp.move_cursor_by(1);
                        TokenKind::ARROW
                    } else {
                        TokenKind::Char('=')
                    };
                }
                CharCode::Char('i') => {
                    let mut bb = [0u8; 5];
                    inp.peek_bytes(&mut bb);

                    match (bb[0], bb[1], bb[2], bb[3], bb[4]) {
                        (b'n', ..) => {
                            inp.move_cursor_by(1);
                            return TokenKind::IN;
                        }
                        (b'm', b'p', b'o', b'r', b't') => {
                            inp.move_cursor_by(5);
                            return TokenKind::IMPORT;
                        }
                        _ => {
                            return TokenKind::IDENT;
                        }
                    }
                }
                CharCode::Char('o') => {
                    return if inp.current_byte().is_some_and(|x| x == b'f') {
                        inp.move_cursor_by(1);
                        TokenKind::OF
                    } else {
                        TokenKind::IDENT
                    };
                }
                CharCode::Char('e') => {
                    return if inp.match_bytes(b"xport") {
                        inp.move_cursor_by(5);
                        TokenKind::EXPORT
                    } else {
                        TokenKind::IDENT
                    };
                }
                CharCode::Char('f') => {
                    return if inp.match_bytes(b"unction") {
                        inp.move_cursor_by(7);
                        TokenKind::FUNCTION
                    } else {
                        TokenKind::IDENT
                    };
                }
                CharCode::Char(c) => {
                    return if Self::is_ident_first(c) {
                        TokenKind::IDENT
                    } else {
                        TokenKind::Char(c)
                    };
                }
                _ => {}
            }
        }

        TokenKind::EOF
    }

    /// peek next token since current bytes cursor.
    /// this function won't update cursor position.
    pub(super) fn peek_token(&self, no_line_terminator: bool) -> TokenKind {
        if self.buf_ptr < self.input_end {
            Self::peek_token_internal(
                &mut Utf8Stream::new(unsafe {
                    std::slice::from_raw_parts(
                        self.buf_ptr,
                        self.input_end.offset_from(self.buf_ptr) as usize,
                    )
                }),
                no_line_terminator,
            )
        } else {
            TokenKind::EOF
        }
    }
}
