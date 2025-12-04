use crate::{
    context::EvalMode,
    parse::token::TokenData,
    string::{StringBuf, UTF8_CHAR_LEN_MAX, unicode_from_utf8_v0},
};

use super::{
    state::JSParseState,
    token::{TOK_STRING, Token},
};

impl<'s> JSParseState<'s> {
    /// `token`: None for self.token
    pub(super) fn parse_string(
        &mut self,
        sep: char,
        do_throw: bool,
        ptr: *const u8,
        token: Option<&mut Token>,
    ) -> Result<*const u8, i32> {
        let mut sbuf = StringBuf::Empty;
        let mut p = ptr;

        loop {
            if p >= self.input_end {
                if do_throw {
                    self.throw_syntax_error("unexpected end of string");
                }
                return Err(-1);
            }

            let mut char_code: u32 = 0;

            match unsafe { (*p) as char } {
                c @ ('\r' | '\n') => {
                    if sep == '`' {
                        p = unsafe { p.offset(1) };
                        if c == '\r' && unsafe { *p == b'\n' } {
                            p = unsafe { p.offset(1) };
                        }
                        char_code = '\n' as u32;
                        // do not update s->line_num
                    } else {
                        if do_throw {
                            self.throw_syntax_error("unexpected end of string");
                        }
                        return Err(-1);
                    }
                }
                c if c == sep => {
                    // found string ending delimiter
                    p = unsafe { p.offset(1) };
                    break;
                }

                '$' if unsafe { *p.add(1) } == b'{' && sep == '`' => {
                    // found template expression start
                    p = unsafe { p.offset(2) }; // skip ${
                    break;
                }
                '\\' => {
                    // handle escape sequences
                    p = unsafe { p.offset(1) };
                    let mut do_parse_escape: Option<char> = None;

                    match unsafe { *p as char } {
                        '\0' => {
                            if p >= self.input_end {
                                if do_throw {
                                    self.throw_syntax_error("unexpected end of string");
                                }
                                return Err(-1);
                            }
                            p = unsafe { p.offset(1) };
                            char_code = 0;
                        }
                        c1 @ ('\'' | '\"' | '\\') => {
                            char_code = c1 as u32;
                            p = unsafe { p.offset(1) };
                        }
                        c1 @ ('\r' | '\n') => {
                            p = unsafe { p.offset(1) };
                            if c1 == '\r' && unsafe { *p == b'\n' } {
                                p = unsafe { p.offset(1) };
                            }
                            // skip escaped line breaks
                            if sep != '`' {
                                self.line_num += 1;
                            }
                            continue;
                        }
                        c1 @ ('0'..='9') => {
                            if self.cur_func_def.is_null() {
                                // parsing JSON
                                if do_throw {
                                    self.throw_syntax_error(
                                        "malformed escape sequence in string literal",
                                    );
                                }
                                return Err(-1);
                            }

                            if !self.cur_func().attrs.js_mode.contains(EvalMode::STRICT)
                                && sep != '`'
                            {
                                // parse escape sequence in non-strict mode
                                do_parse_escape = Some(c1);
                            } else if c1 == '0' && unsafe { !(*p.add(1)).is_ascii_digit() } {
                                // handle null character escape
                                p = unsafe { p.add(1) };
                                char_code = 0;
                            } else {
                                // ES2021: octal escapes 8/9 not allowed in strict mode/templates
                                if do_throw {
                                    self.throw_syntax_error(if c1 >= '8' || sep == '`' {
                                        "malformed escape sequence in string literal"
                                    } else {
                                        "octal escape sequences are not allowed in strict mode"
                                    });
                                }
                                return Err(-1);
                            }
                        }
                        c1 => {
                            if !c1.is_ascii() {
                                let mut p_next = std::ptr::null::<u8>();
                                match unicode_from_utf8_v0(
                                    p,
                                    UTF8_CHAR_LEN_MAX as i32,
                                    std::ptr::addr_of_mut!(p_next),
                                ) {
                                    Ok(uc) => {
                                        p = p_next;
                                        if matches!(uc, '\u{2028}' | '\u{2029}') {
                                            continue; // skip line/paragraph separators
                                        } else {
                                            char_code = uc as u32;
                                        }
                                    }
                                    Err(_) => {
                                        if do_throw {
                                            self.throw_syntax_error("invalid UTF-8 sequence");
                                        }
                                        return Err(-1);
                                    }
                                }
                            } else {
                                do_parse_escape = Some(c1)
                            }
                        }
                    } // match

                    if let Some(ch) = do_parse_escape.take() {
                        let bytes = unsafe {
                            std::slice::from_raw_parts(p, self.input_end.offset_from(p) as usize)
                        };
                        match self.parse_string_escape(bytes, 1) {
                            Ok((code, consume)) => {
                                char_code = code;
                                p = unsafe { p.add(consume) };
                            }
                            Err(-1) => {
                                if do_throw {
                                    self.throw_syntax_error(
                                        "malformed escape sequence in string literal",
                                    );
                                }
                                return Err(-1);
                            }
                            Err(-2) => {
                                // ignore the '\' (could output a warning)
                                char_code = ch as u32;
                                p = unsafe { p.add(1) };
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                c => {
                    if !c.is_ascii() {
                        // handle UTF-8 characters
                        let mut p_next = std::ptr::null::<u8>();
                        match unicode_from_utf8_v0(
                            p,
                            UTF8_CHAR_LEN_MAX as i32,
                            std::ptr::addr_of_mut!(p_next),
                        ) {
                            Ok(uc) => {
                                p = p_next;
                                if matches!(uc, '\u{2088}' | '\u{2089}') {
                                    continue; // LS or PS are skipped
                                } else {
                                    char_code = uc as u32;
                                }
                            }
                            Err(_) => {
                                if do_throw {
                                    self.throw_syntax_error("invalid UTF-8 sequence");
                                }
                                return Err(-1);
                            }
                        }
                    } else {
                        // ascii char
                        char_code = c as u32;
                        if char_code < 0x20 {
                            // ascii control chars
                            if self.cur_func_def.is_null() {
                                if do_throw {
                                    self.throw_syntax_error("invalid character in a JSON string");
                                }
                                return Err(-1);
                            }
                        }
                        p = unsafe { p.offset(1) };
                    }
                }
            }

            // add char_code to string content
            if sbuf.capacity() == sbuf.len() {
                sbuf.try_reserve_exact(32).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
            }
            if let Some(uc) = char::from_u32(char_code) {
                sbuf.push_char(uc).unwrap(); // can't fail
            } else {
                sbuf.push_u16(char_code as u16).unwrap(); // can't fail
            }
        }

        // update token
        let tok = token.unwrap_or(&mut self.token);
        tok.val = TOK_STRING;
        tok.data = TokenData::Str {
            str: sbuf,
            sep: sep as i32,
        };

        Ok(p)
    }

    /// parse escaped char code \u{XXXX}, \uXXXX, \xXX from given bytes.
    /// bytes starts since next char after '\'
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
                    for i in hex_start..pos {
                        cp = (cp << 4) + (from_hex(bytes[i])? as u32);
                        if cp > 0x10ffff {
                            return Err(-1); // bad unicode
                        }
                    }

                    let consumed = pos + 1; // from #0 to '}' inclusive
                    (cp, consumed)
                } else {
                    // format \x99 or \u0099
                    let cnt = if (c as u8) == b'x' { 2 } else { 4 };
                    if pos + cnt > bytes.len() {
                        return Err(-1); // not enough bytes
                    }

                    let mut cp = 0_u32;
                    for i in pos..pos + cnt {
                        cp = (cp << 4) | (from_hex(bytes[i])? as u32);
                    }

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
