use std::rc::Rc;

use crate::{
    context::JS_MODE_STRICT,
    parse::token::TokenData,
    regexp::{from_hex, is_digit},
    string::StringBuf,
    unicode::{UTF8_CHAR_LEN_MAX, unicode_from_utf8},
};

use super::{
    state::JSParseState,
    token::{TOK_STRING, Token},
};

impl<'s> JSParseState<'s> {
    pub fn parse_string(
        &mut self,
        sep: char,
        do_throw: bool,
        ptr: *const u8,
        token: Option<&mut Token>,
    ) -> Result<*const u8, i32> {
        let mut content = StringBuf::Empty;
        let mut p = ptr;

        loop {
            if p >= self.buf_end {
                if do_throw {
                    self.throw_syntax_error("unexpected end of string");
                }
                return Err(-1);
            }

            if content.capacity() == content.buf_len() {
                content.try_reserve_exact(32).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
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
                    p = unsafe { p.offset(2) }; // move past ${
                    break;
                }
                '\\' => {
                    // handle escape sequences
                    p = unsafe { p.offset(1) };
                    let mut do_parse_escape: Option<char> = None;

                    match unsafe { *p as char } {
                        '\0' => {
                            if p >= self.buf_end {
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
                                if do_throw {
                                    self.throw_syntax_error(
                                        "malformed escape sequence in string literal",
                                    );
                                }
                                return Err(-1);
                            }

                            if self.cur_func().js_mode & JS_MODE_STRICT as u8 == 0 && sep != '`' {
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
                                match unicode_from_utf8(
                                    p,
                                    UTF8_CHAR_LEN_MAX as i32,
                                    std::ptr::addr_of_mut!(p_next),
                                ) {
                                    Ok(uc) => {
                                        p = p_next;
                                        if matches!(uc, '\u{2088}' | '\u{2089}') {
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
                        match unsafe { self.parse_string_escape(&mut p, 1) } {
                            Ok(cp) => {
                                char_code = cp;
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
                                // treat backslash as literal character
                                char_code = ch as u32;
                            }
                            _ => unreachable!(),
                        }
                    }
                }
                c => {
                    if !c.is_ascii() {
                        // handle UTF-8 characters
                        let mut p_next = std::ptr::null::<u8>();
                        match unicode_from_utf8(
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
            if let Some(uc) = char::from_u32(char_code) {
                content.push_char(uc).unwrap(); // can't fail
            } else {
                content.push_u16(char_code as u16).unwrap(); // can't fail
            }
        }

        // update token
        let tok = token.unwrap_or(&mut self.token);
        tok.val = TOK_STRING;
        tok.data = TokenData::Str {
            str: Rc::new(content),
            sep: sep as i32,
        };

        Ok(p)
    }

    /// parse char escape \u{XXXX}, \uXXXX, \xXX.
    /// `pp` points to next char of '\'.
    /// returns code as u32, could be invalid unicode
    pub(super) unsafe fn parse_string_escape(
        &mut self,
        pp: *mut *const u8,
        allow_utf16: i32,
    ) -> Result<u32, i32> {
        let mut p = *pp;

        let ch = *p as char;
        p = p.offset(1); // move to next byte

        let code: u32 = match ch {
            'b' => {
                b'\x08' as u32 // backspace
            }
            'f' => {
                b'\x0C' as u32 // form feed
            }
            'n' => b'\n' as u32,
            'r' => b'\r' as u32,
            't' => b'\t' as u32,
            'v' => {
                b'\x0B' as u32 // \v in C is vertical tab, 0x0B in Rust
            }

            c @ ('x' | 'u') => {
                if *p == b'{' && allow_utf16 != 0 {
                    // format \u{0099}
                    p = p.offset(1); // skip '{'

                    let mut cp = 0_u32;
                    while (*p) != b'}' {
                        cp = (cp << 4) + (from_hex(*p)? as u32);
                        if cp > 0x10ffff {
                            return Err(-1); // bad unicode
                        }
                        p = p.add(1);
                    }
                    p = p.add(1); // skip '}'
                    cp
                } else {
                    // format \x99 or \u0099
                    let cnt = if (c as u8) == b'x' { 2 } else { 4 };
                    let mut cp = 0_u32;
                    for _ in 0..cnt {
                        cp = (cp << 4) | (from_hex(*p)? as u32);
                        p = p.offset(1);
                    }

                    if (0xD800..0xDC00).contains(&cp)
                        && allow_utf16 == 2
                        && *p == b'\\'
                        && *p.add(1) == b'u'
                    {
                        // convert an escaped surrogate pair into a unicode char
                        let mut c1 = 0_u32;
                        let mut i = 0;
                        while i < 4 {
                            if let Ok(h) = from_hex(*p.add(2 + i)) {
                                c1 = (c1 << 4) | (h as u32);
                                i += 1;
                            } else {
                                break;
                            }
                        }

                        if i == 4 && (0xDC00..0xE000).contains(&c1) {
                            p = p.add(6);
                            cp = (((cp & 0x3FF) << 10) | (c1 & 0x3FF)) + 0x10000;
                        }
                    }

                    cp
                }
            }
            c @ ('0'..='7') => {
                let cp = c as u32 - '0' as u32;

                if allow_utf16 == 2 {
                    // only accept \0 not followed by digit
                    if cp != 0 || is_digit(*p) {
                        return Err(-1);
                    }
                } else {
                    // parse a legacy octal sequence
                    self.throw_syntax_error("octal escape sequence is not supported");
                    return Err(-1);

                    // TODO parse \0nnn octal sequence - legacy
                    // let p0 = p;
                    // loop {
                    //     if !((*p) as char).is_digit(8) {
                    //         break;
                    //     } else {
                    //         p = p.add(1);
                    //     }
                    // }
                    // let s = std::str::from_utf8(std::slice::from_raw_parts(
                    //     p0,
                    //     p.offset_from(p0) as usize,
                    // ))
                    // .map_err(|_| -1)?;
                    // c = char::from_u32(u32::from_str_radix(s, 8).map_err(|_| -1)?).ok_or(-1)?;
                }

                cp
            }
            _ => {
                return Err(-2);
            }
        };

        *pp = p;
        Ok(code)
    }
}
