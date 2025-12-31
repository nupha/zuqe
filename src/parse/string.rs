use crate::{
    stack_frame::ExecMode,
    string::{CharCode, CharCodeDecoder, StringBuf, Utf8Stream},
};

use super::{
    state::JSParseState,
    tokenize::{Token, TokenData, TokenKind},
};

impl<'s> JSParseState<'s> {
    /// `input` cursor at start if string content, not include string starting delimitor.
    /// returns consumed number of bytes (include the trailing delimitor).
    /// when exit, cursor is exactly at next char of ending delimitor.
    /// `token`: None for self.token
    pub(super) fn parse_string(
        &mut self,
        input: &mut Utf8Stream,
        sep: char,
        token: Option<&mut Token>,
        throw_err: bool,
    ) -> Result<usize, i32> {
        let mut sbuf = StringBuf::Empty;
        let mut consume: Option<usize> = None;

        while let Some((span, sc)) = input.next_char() {
            if let CharCode::Char(mut c) = sc {
                if (c as u32) < 0x20 {
                    // ascii control chars
                    if self.cur_func_def.is_null() {
                        if throw_err {
                            self.throw_syntax_error("invalid character in a JSON string");
                        }
                        return Err(-1);
                    }

                    if sep == '`' {
                        if c == '\r' {
                            if let Some(b'\n') = input.current_byte() {
                                input.move_cursor_by(1); // skip \n after \r
                            }
                            c = '\n'
                        }
                    } else if matches!(c, '\r' | '\n') {
                        if throw_err {
                            self.throw_syntax_error("unexpected end of string");
                        }
                        return Err(-1);
                    }
                }

                if c == sep {
                    // string end, consumed includes trailing delimitor.
                    debug_assert_eq!(
                        span.start + span.len(),
                        span.start + c.len_utf8(),
                        "{span:?}, [{c}]"
                    );
                    consume = Some(span.start + span.len());
                    break;
                } else if c == '$'
                    && sep == '`'
                    && let Some(b'{') = input.current_byte()
                {
                    // template expression start
                    input.move_cursor_by(1); // skip '{'
                    consume = Some(span.start + 2);
                    break;
                }

                let mut cc = c as u32;

                if c == '\\' {
                    // handle escape sequence
                    if let Some((sc2, _)) = input.current_char() {
                        let mut should_parse_escape: Option<char> = None;

                        match sc2 {
                            CharCode::Char(c1 @ ('\'' | '\"' | '\\')) => {
                                input.move_cursor_by(1);
                                cc = c1 as u32;
                            }
                            CharCode::Char(n @ ('\r' | '\n')) => {
                                if n == '\r'
                                    && let Some(b'\n') = input.peek_byte_at(1)
                                {
                                    input.move_cursor_by(2); // skip \r\n
                                } else {
                                    input.move_cursor_by(1); // skip \r or \n
                                }

                                // skip escaped line breaks
                                if sep != '`' {
                                    self.line_num += 1;
                                }
                                continue;
                            }
                            CharCode::Char(d @ ('0'..='9')) => {
                                if self.cur_func_def.is_null() {
                                    // parsing JSON
                                    if throw_err {
                                        self.throw_syntax_error(
                                            "malformed escape sequence in string literal",
                                        );
                                    }
                                    return Err(-1);
                                }

                                if !self.cur_func().attrs.exec_mode.contains(ExecMode::STRICT)
                                    && sep != '`'
                                {
                                    // parse escape sequence in non-strict mode
                                    should_parse_escape = Some(d);
                                } else if d == '0'
                                    && input.peek_byte_at(1).is_none_or(|x| !x.is_ascii_digit())
                                {
                                    // found \0
                                    input.move_cursor_by(1); // skip '0'
                                    cc = 0;
                                } else {
                                    // ES2021: octal escapes 8/9 not allowed in strict mode/templates
                                    if throw_err {
                                        self.throw_syntax_error(if d >= '8' || sep == '`' {
                                            "malformed escape sequence in string literal"
                                        } else {
                                            "octal escape sequences are not allowed in strict mode"
                                        });
                                    }
                                    return Err(-1);
                                }
                            }
                            CharCode::Char(c1 @ ('\u{2028}' | '\u{2029}')) => {
                                // skip line/paragraph separators
                                input.move_cursor_by(c1.len_utf8() as _);
                                continue;
                            }
                            CharCode::Char(c1) => {
                                if c1.is_ascii() {
                                    should_parse_escape = Some(c1)
                                } else {
                                    input.move_cursor_by(c1.len_utf8() as i32);
                                    cc = c1 as u32;
                                }
                            }
                            CharCode::Code(_) => {
                                if throw_err {
                                    self.throw_syntax_error("invalid UTF-8 sequence");
                                }
                                return Err(-1);
                            }
                        }

                        if let Some(ch) = should_parse_escape.take() {
                            let bytes = input.remainder(); //&buf[j..];
                            match self.parse_string_escape(bytes, 1) {
                                Ok((code, consume)) => {
                                    input.move_cursor_by(consume as _);
                                    cc = code;
                                }
                                Err(-1) => {
                                    if throw_err {
                                        self.throw_syntax_error(
                                            "malformed escape sequence in string literal",
                                        );
                                    }
                                    return Err(-1);
                                }
                                Err(-2) => {
                                    // ignore the '\' (could output a warning)
                                    input.move_cursor_by(1);
                                    cc = ch as u32;
                                }
                                _ => unreachable!(),
                            }
                        }
                    } else {
                        if throw_err {
                            self.throw_syntax_error("unexpected end of string");
                        }
                        return Err(-1);
                    }
                }

                // add char_code to string buf
                if sbuf.capacity() == sbuf.len() {
                    sbuf.try_reserve_exact(32).map_err(|_| {
                        self.ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                let _ = sbuf.push_u32(cc); // can't fail
            } else {
                if throw_err {
                    self.throw_syntax_error("invalid UTF-8 sequence");
                }
                return Err(-1);
            }
        }

        if let Some(consume) = consume {
            // update token
            let tok = token.unwrap_or(&mut self.token);
            tok.kind = TokenKind::STRING;
            tok.data = TokenData::Str {
                str: sbuf,
                sep: sep as i32,
            };
            Ok(consume)
        } else {
            if throw_err {
                self.throw_syntax_error("unexpected end of string");
            }
            Err(-1)
        }
    }

    /// parse escaped char code \u{XXXX}, \uXXXX, \xXX from given bytes.
    /// bytes starts since next char after '\\'
    ///
    /// Returns:
    /// - (code_point, byte_consumed)
    /// - Err(-1): invalid escape sequence
    /// - Err(-2): unprocessed
    pub(super) fn parse_string_escape(
        &mut self,
        bytes: &[u8],
        allow_utf16: i32,
    ) -> Result<(u32, usize), i32> {
        debug_assert!(!bytes.is_empty());
        debug_assert_ne!(bytes[0], b'\\', "won't start with \\");

        let mut pos = 1; // after the first character
        let ch = bytes[0] as char;

        let (code, consumed): (u32, usize) = match ch {
            'b' => {
                (b'\x08' as u32, 1) // backspace
            }
            'f' => {
                (b'\x0C' as u32, 1) // form feed
            }
            'n' => (b'\n' as u32, 1),
            'r' => (b'\r' as u32, 1),
            't' => (b'\t' as u32, 1),
            'v' => {
                (b'\x0B' as u32, 1) // \v in C is vertical tab, 0x0B in Rust
            }

            c @ ('x' | 'u') => {
                if pos < bytes.len() && bytes[pos] == b'{' && allow_utf16 != 0 {
                    // format \u{0099}
                    pos += 1; // skip '{'

                    let hex_start = pos;
                    while pos < bytes.len() && bytes[pos] != b'}' {
                        pos += 1;
                    }

                    if pos >= bytes.len() || bytes[pos] != b'}' {
                        return Err(-1); // missing closing brace
                    }

                    // Parse hex digits
                    let mut cp = 0_u32;
                    for b in bytes.iter().take(pos).skip(hex_start) {
                        cp = (cp << 4) + (from_hex(*b)? as u32);
                        if cp > 0x10ffff {
                            return Err(-1); // bad unicode
                        }
                    }
                    // for i in hex_start..pos {
                    //     cp = (cp << 4) + (from_hex(bytes[i])? as u32);
                    //     if cp > 0x10ffff {
                    //         return Err(-1); // bad unicode
                    //     }
                    // }

                    let consumed = pos + 1; // from #0 to '}' inclusive
                    (cp, consumed)
                } else {
                    // format \x99 or \u0099
                    let cnt = if (c as u8) == b'x' { 2 } else { 4 };
                    if pos + cnt > bytes.len() {
                        return Err(-1); // not enough bytes
                    }

                    let mut cp = 0_u32;
                    for b in bytes.iter().skip(pos).take(cnt) {
                        cp = (cp << 4) | (from_hex(*b)? as u32);
                    }
                    // for i in pos..pos + cnt {
                    //     cp = (cp << 4) | (from_hex(bytes[i])? as u32);
                    // }

                    let mut consumed = cnt + 1; // +1 for the initial char

                    // Check for surrogate pair handling
                    if (0xD800..0xDC00).contains(&cp)
                        && allow_utf16 == 2
                        && pos + cnt + 2 <= bytes.len()
                        && bytes[pos + cnt] == b'\\'
                        && bytes[pos + cnt + 1] == b'u'
                    {
                        // convert an escaped surrogate pair into a unicode char
                        let mut c1 = 0_u32;
                        let mut i = 0;
                        while i < 4 && pos + cnt + 2 + i < bytes.len() {
                            if let Ok(h) = from_hex(bytes[pos + cnt + 2 + i]) {
                                c1 = (c1 << 4) | (h as u32);
                                i += 1;
                            } else {
                                break;
                            }
                        }

                        if i == 4 && (0xDC00..0xE000).contains(&c1) {
                            consumed = cnt + 6 + 1; // +1 for the initial char
                            cp = (((cp & 0x3FF) << 10) | (c1 & 0x3FF)) + 0x10000;
                        }
                    }

                    (cp, consumed)
                }
            }
            c @ ('0'..='7') => {
                let mut code = c as u32 - '0' as u32; // digit #1
                let mut consumed = 1;

                if allow_utf16 == 2 {
                    // only accept \0 not followed by digit
                    if code != 0 || (pos < bytes.len() && bytes[pos].is_ascii_digit()) {
                        return Err(-1);
                    }
                } else {
                    // parse a legacy octal sequence
                    // digit #2,3
                    for i in 0..2 {
                        if pos + i >= bytes.len() || !bytes[pos + i].is_ascii_digit() {
                            break;
                        }

                        let v = bytes[pos + i] as u32 - '0' as u32;
                        if v > 7 {
                            break;
                        } else {
                            code = (code << 3) | v;
                            consumed = i + 2; // include this digit + initial char

                            // digit #1,2 should less than 32
                            if i == 0 && code >= 32 {
                                break;
                            }
                        }
                    }
                }

                (code, consumed)
            }
            _ => {
                return Err(-2);
            }
        };

        Ok((code, consumed))
    }
}

#[inline]
const fn from_hex(c: u8) -> Result<u8, i32> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(-1),
    }
}
