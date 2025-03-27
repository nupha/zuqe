use std::ffi::CStr;

use crate::{
    parse::{error::js_parse_error, token::TOK_REGEXP},
    value::StringContent,
};

use super::JSParseState;

pub unsafe fn js_parse_regexp(s: *mut JSParseState) -> i32 {
    macro_rules! fail {
        () => {
            return -1;
        };
    }
    macro_rules! eof_error {
        () => {
            js_parse_error(s, c"unexpected end of regexp");
            fail!();
        };
    }
    macro_rules! eol_error {
        () => {
            js_parse_error(s, c"unexpected line terminator in regexp");
            fail!();
        };
    }
    macro_rules! invalid_utf8 {
        () => {
            js_parse_error(s, c"invalid UTF-8 sequence");
            fail!();
        };
    }

    let mut in_class = false;
    let mut offset = 0_usize;
    let mut res_string = StringContent::Empty;
    let mut res_string2 = StringContent::Empty;

    if let Ok(mut chars) = CStr::from_ptr((*s).buf_ptr.cast())
        .to_str()
        .map(|x| x.char_indices())
    {
        while let Some((i, mut c)) = chars.next() {
            offset = i;

            if c == '\n' || c == '\r' {
                eol_error!();
            } else if c == '/' {
                if !in_class {
                    break;
                }
            } else if c == '[' {
                in_class = true;
            } else if c == ']' {
                // XXX: incorrect as the first character in a class
                in_class = false;
            } else if c == '\\' {
                if res_string.push_char(c).is_err() {
                    fail!();
                }

                if let Some((i, c1)) = chars.next() {
                    offset = i;

                    if c1 == '\n' || c1 == '\r' {
                        eol_error!();
                    } else {
                        c = c1;
                    }
                } else {
                    eof_error!();
                }
            }

            if res_string.push_char(c).is_err() {
                fail!();
            }
        }

        // flags
        while let Some((i, c)) = chars.next() {
            if !JSParseState::is_ident_next(c) {
                break;
            }

            if res_string2.push_char(c).is_err() {
                fail!();
            }

            offset = i;
        }

        (*s).token.val = TOK_REGEXP;
        (*s).token.u.regexp.body = (*(*s).ctx).create_string_from_content(res_string);
        (*s).token.u.regexp.flags = (*(*s).ctx).create_string_from_content(res_string2);
        (*s).buf_ptr = (*s).buf_ptr.add(offset);
        return 0;
    } else {
        invalid_utf8!();
    }

    // let mut p = (*s).buf_ptr;
    // p = p.offset(1);

    // loop {
    //     if p >= (*s).buf_end {
    //         eof_error!();
    //     }

    //     let mut c = *p as char;
    //     p = p.offset(1);

    //     if c == '\n' || c == '\r' {
    //         eol_error!();
    //     } else if c == '/' {
    //         if !in_class {
    //             break;
    //         }
    //     } else if c == '[' {
    //         in_class = true;
    //     } else if c == ']' {
    //         // XXX: incorrect as the first character in a class
    //         in_class = false;
    //     } else if c == '\\' {
    //         if res_string.push_chars(std::iter::once(c)).is_err() {
    //             fail!();
    //         }

    //         c = *p as char;
    //         p = p.offset(1);

    //         if c == '\n' || c == '\r' {
    //             eol_error!();
    //         } else if c == '\0' && p >= (*s).buf_end {
    //             eof_error!();
    //         }
    //     } else if c >= 0x80 {
    //         let mut p_next = std::ptr::null::<u8>();
    //         c = unicode_from_utf8(p.offset(-1), UTF8_CHAR_LEN_MAX as _, &mut p_next) as _;
    //         if c > 0x10FFFF {
    //             invalid_utf8!();
    //         }
    //         p = p_next;
    //         // LS or PS are considered as line terminator
    //         if c == CP_LS || c == CP_PS {
    //             eol_error!();
    //         }
    //     }

    //     if res_string.push_chars(std::iter::once(c)).is_err() {
    //         fail!();
    //     }
    // }

    // flags
    // loop {
    //     let mut p_next = p;
    //     let mut c = *p_next as i32;
    //     p_next = p_next.offset(1);

    //     if c >= 0x80 {
    //         c = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as _, &mut p_next);
    //         if c > 0x10FFFF {
    //             invalid_utf8!();
    //         }
    //     }
    //     if lre_js_is_ident_next(c) == 0 {
    //         break;
    //     }
    //     if string_buffer_putc(b2, c as u32) != 0 {
    //         fail!();
    //     }
    //     p = p_next;
    // }

    // (*s).token.val = TOK_REGEXP;
    // (*s).token.u.regexp.body = (*(*s).ctx).create_string_from_content(res_string);
    // (*s).token.u.regexp.flags = (*(*s).ctx).create_string_from_content(res_string2);
    // (*s).buf_ptr = p;
    // return 0;
}
