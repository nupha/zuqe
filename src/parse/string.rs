use crate::{
    context::JS_MODE_STRICT,
    regexp::{from_hex, is_digit},
    unicode::{unicode_from_utf8, UTF8_CHAR_LEN_MAX},
    value::StringContent,
};

use super::{
    error::js_parse_error,
    state::JSParseState,
    token::{JSToken, TOK_STRING},
};

impl JSParseState {
    pub unsafe fn parse_string(
        &mut self,
        sep: char,
        do_throw: bool,
        mut p: *const u8,
        token: Option<&mut JSToken>,
        pp: *mut *const u8,
    ) -> Result<(), i32> {
        let mut content = StringContent::Empty;

        loop {
            if p >= self.buf_end {
                if do_throw {
                    js_parse_error(self, c"unexpected end of string");
                }
                return Err(-1);
            }

            if content.capacity() == content.len() {
                if content.try_reserve_exact(32).is_err() {
                    (*self.ctx).JS_ThrowOutOfMemory();
                    return Err(-1);
                }
            }

            let mut char_code: u32 = 0;

            match (*p) as char {
                c @ ('\r' | '\n') => {
                    if sep == '`' {
                        p = p.offset(1);
                        if c == '\r' && *p == b'\n' {
                            p = p.offset(1);
                        }
                        char_code = '\n' as u32;
                        // do not update s->line_num
                    } else {
                        if do_throw {
                            js_parse_error(self, c"unexpected end of string");
                        }
                        return Err(-1);
                    }
                }
                c if (c as u32) < 0x20 => {
                    // handle ASCII control characters
                    if self.cur_func.is_null() {
                        if do_throw {
                            js_parse_error(self, c"invalid character in a JSON string");
                        }
                        return Err(-1);
                    }

                    p = p.offset(1);
                }

                c if c == sep => {
                    // found string ending delimiter
                    p = p.offset(1);
                    break;
                }

                '$' if *p.add(1) == b'{' && sep == '`' => {
                    // found template expression start
                    p = p.offset(2); // move past ${
                    break;
                }
                '\\' => {
                    // handle escape sequences
                    p = p.offset(1);
                    let mut do_parse_escape: Option<char> = None;

                    match *p as char {
                        '\0' => {
                            if p >= self.buf_end {
                                if do_throw {
                                    js_parse_error(self, c"unexpected end of string");
                                }
                                return Err(-1);
                            }
                            p = p.offset(1);
                            char_code = 0;
                        }
                        c1 @ ('\'' | '\"' | '\\') => {
                            char_code = c1 as u32;
                            p = p.offset(1);
                        }
                        c1 @ ('\r' | '\n') => {
                            p = p.offset(1);
                            if c1 == '\r' && *p == b'\n' {
                                p = p.offset(1);
                            }
                            // skip escaped line breaks
                            if sep != '`' {
                                self.line_num += 1;
                            }
                            continue;
                        }
                        c1 @ ('0'..='9') => {
                            if self.cur_func.is_null() {
                                if do_throw {
                                    js_parse_error(
                                        self,
                                        c"malformed escape sequence in string literal",
                                    );
                                }
                                return Err(-1);
                            }

                            if (*self.cur_func).js_mode & JS_MODE_STRICT as u8 == 0 && sep != '`' {
                                // parse escape sequence in non-strict mode
                                do_parse_escape = Some(c1);
                            } else if c1 == '0' && !(b'0'..=b'9').contains(&*p.add(1)) {
                                // handle null character escape
                                p = p.add(1);
                                char_code = 0;
                            } else {
                                // ES2021: octal escapes 8/9 not allowed in strict mode/templates
                                if do_throw {
                                    js_parse_error(
                                        self,
                                        if c1 >= '8' || sep == '`' {
                                            c"malformed escape sequence in string literal"
                                        } else {
                                            c"octal escape sequences are not allowed in strict mode"
                                        },
                                    );
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
                                            js_parse_error(self, c"invalid UTF-8 sequence");
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
                        match self.parse_string_escape(&mut p, 1) {
                            Ok(cp) => {
                                char_code = cp;
                            }
                            Err(-1) => {
                                if do_throw {
                                    js_parse_error(
                                        self,
                                        c"malformed escape sequence in string literal",
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
                                    js_parse_error(self, c"invalid UTF-8 sequence");
                                }
                                return Err(-1);
                            }
                        }
                    } else {
                        // ascii char
                        p = p.offset(1);
                        char_code = c as u32;
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

        let str_val = (*self.ctx)
            .create_string_from_content(content)
            .check_exception()?;

        // update token
        let tok = token.unwrap_or(&mut self.token);
        tok.val = TOK_STRING;
        tok.u.str.sep = sep as i32;
        tok.u.str.str = str_val;

        *pp = p;
        Ok(())
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
                    js_parse_error(self, c"octal escape sequence is not supported");
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

#[deprecated(note = "use JSParseState::parse_string() instead")]
pub unsafe fn js_parse_string(
    s: *mut JSParseState,
    sep: char,
    do_throw: bool,
    p: *const u8,
    token: *mut JSToken,
    pp: *mut *const u8,
) -> i32 {
    if (*s)
        .parse_string(sep, do_throw, p, Some(token.as_mut().unwrap()), pp)
        .is_err()
    {
        -1
    } else {
        0
    }
}

// pub unsafe fn js_parse_string(
//     s: *mut JSParseState,
//     sep: i32,
//     do_throw: bool,
//     mut p: *const u8,
//     token: *mut JSToken,
//     pp: *mut *const u8,
// ) -> i32 {
//     // string
//     let mut b_s: StringBuffer = std::mem::zeroed();
//     let b = std::ptr::addr_of_mut!(b_s);

//     if string_buffer_init((*s).ctx, b, 32) != 0 {
//         // C2RS: goto fail
//         string_buffer_free(b);
//         return -1;
//     }

//     let mut ret: i32;
//     let mut c: i32;

//     loop {
//         if p >= (*s).buf_end {
//             // C2RS: goto invalid_char
//             if do_throw {
//                 js_parse_error(s, c"unexpected end of string");
//             }
//             string_buffer_free(b);
//             return -1;
//         }

//         c = *p as i32;

//         if c < 0x20 {
//             if (*s).cur_func.is_null() {
//                 if do_throw {
//                     js_parse_error(s, c"invalid character in a JSON string");
//                 }
//                 string_buffer_free(b);
//                 return -1;
//             }
//             if sep == b'`' as i32 {
//                 if c == b'\r' as i32 {
//                     if *p.offset(1) == b'\n' {
//                         p = p.offset(1);
//                     }
//                     c = b'\n' as i32;
//                 }
//                 // do not update s->line_num
//             } else if c == b'\n' as i32 || c == b'\r' as i32 {
//                 // C2RS: goto invalid_char
//                 if do_throw {
//                     js_parse_error(s, c"unexpected end of string");
//                 }
//                 string_buffer_free(b);
//                 return -1;
//             }
//         }
//         p = p.offset(1);
//         if c == sep as i32 {
//             break;
//         }
//         if c == b'$' as i32 && *p == b'{' as u8 && sep == b'`' as i32 {
//             // template start or middle part
//             p = p.offset(1);
//             break;
//         }
//         if c == b'\\' as i32 {
//             c = *p as i32;
//             // XXX: need a specific JSON case to avoid accepting invalid escapes
//             match c as u8 {
//                 b'\0' => {
//                     if p >= (*s).buf_end {
//                         // C2RS: goto invalid_char
//                         if do_throw {
//                             js_parse_error(s, c"unexpected end of string");
//                         }
//                         string_buffer_free(b);
//                         return -1;
//                     }
//                     p = p.offset(1);
//                 }
//                 b'\'' | b'\"' | b'\\' => {
//                     p = p.offset(1);
//                 }
//                 b'\r' => {
//                     // accept DOS and MAC newline sequences
//                     if *p.offset(1) == b'\n' {
//                         p = p.offset(1);
//                     }
//                     // fall thru
//                     p = p.offset(1);
//                     if sep != b'`' as i32 {
//                         (*s).line_num += 1;
//                     }
//                     continue;
//                 }
//                 b'\n' => {
//                     // ignore escaped newline sequence
//                     p = p.offset(1);
//                     if sep != b'`' as i32 {
//                         (*s).line_num += 1;
//                     }
//                     continue;
//                 }
//                 _ => {
//                     if (c as u8) >= b'0' && (c as u8) <= b'9' {
//                         if (*s).cur_func.is_null() {
//                             // C2RS: goto invalid_escape
//                             if do_throw {
//                                 js_parse_error(s, c"malformed escape sequence in string literal");
//                             }
//                             string_buffer_free(b);
//                             return -1;
//                         }
//                         if ((*(*s).cur_func).js_mode & JS_MODE_STRICT as u8) == 0
//                             && sep != b'`' as i32
//                         {
//                             // C2RS: goto parse_escape
//                             ret = lre_parse_escape(&mut p as *mut *const u8, true as i32);
//                         } else {
//                             if (c as u8) >= b'8' || sep == b'`' as i32 {
//                                 // Note: according to ES2021, \8 and \9 are not accepted in strict mode or in templates.
//                                 // C2RS: goto invalid_escape
//                                 if do_throw {
//                                     js_parse_error(
//                                         s,
//                                         c"malformed escape sequence in string literal",
//                                     );
//                                 }
//                                 string_buffer_free(b);
//                                 return -1;
//                             } else {
//                                 if do_throw {
//                                     js_parse_error(
//                                         s,
//                                         c"octal escape sequences are not allowed in strict mode",
//                                     );
//                                 }
//                                 string_buffer_free(b);
//                                 return -1;
//                             }
//                         }
//                         if ret == -1 {
//                             // C2RS: goto invalid_escape
//                             if do_throw {
//                                 js_parse_error(s, c"malformed escape sequence in string literal");
//                             }
//                             string_buffer_free(b);
//                             return -1;
//                         } else if ret < 0 {
//                             // ignore the '\' (could output a warning)
//                             p = p.offset(1);
//                         } else {
//                             c = ret as i32;
//                         }
//                     } else if c >= 0x80 {
//                         let mut p_next = std::ptr::null::<u8>();
//                         c = unicode_from_utf8(
//                             p,
//                             UTF8_CHAR_LEN_MAX as i32,
//                             std::ptr::addr_of_mut!(p_next),
//                         );
//                         assert!(c >= 0);

//                         if c > 0x10FFFF {
//                             // C2RS: goto invalid_utf8
//                             if do_throw {
//                                 js_parse_error(s, c"invalid UTF-8 sequence");
//                             }
//                             string_buffer_free(b);
//                             return -1;
//                         }
//                         p = p_next;
//                         // LS or PS are skipped
//                         if c == CP_LS as i32 || c == CP_PS as i32 {
//                             continue;
//                         }
//                     } else {
//                         // parse_escape
//                         ret = lre_parse_escape(&mut p as *mut *const u8, true as i32);
//                         if ret == -1 {
//                             // C2RS: goto invalid_escape
//                             if do_throw {
//                                 js_parse_error(s, c"malformed escape sequence in string literal");
//                             }
//                             string_buffer_free(b);
//                             return -1;
//                         } else if ret < 0 {
//                             // ignore the '\' (could output a warning)
//                             p = p.offset(1);
//                         } else {
//                             c = ret;
//                         }
//                     }
//                 }
//             }
//         } else if c >= 0x80 {
//             let mut p_next = std::ptr::null::<u8>();
//             c = unicode_from_utf8(
//                 p.offset(-1),
//                 UTF8_CHAR_LEN_MAX as _,
//                 std::ptr::addr_of_mut!(p_next),
//             );
//             assert!(c >= 0);

//             if c > 0x10FFFF {
//                 // C2RS: goto invalid_utf8
//                 if do_throw {
//                     js_parse_error(s, c"invalid UTF-8 sequence");
//                 }
//                 string_buffer_free(b);
//                 return -1;
//             }
//             p = p_next;
//         }
//         if string_buffer_putc(b, c as _) != 0 {
//             string_buffer_free(b);
//             return -1;
//         }
//     }
//     (*token).val = TOK_STRING;
//     (*token).u.str.sep = c;
//     (*token).u.str.str = string_buffer_end(b);
//     println!(
//         "string {:p} #{}, {:?}",
//         JS_VALUE_GET_STRING((*token).u.str.str),
//         (*JS_VALUE_GET_STRING((*token).u.str.str)).header.ref_count,
//         CStr::from_ptr((*JS_VALUE_GET_STRING((*token).u.str.str)).as_str8().cast())
//     );
//     *pp = p;

//     0
// }
