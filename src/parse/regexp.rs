use std::ffi::CStr;

use crate::{
    parse::token::{TOK_REGEXP, TokenData},
    value::StringContent,
};

use super::JSParseState;

pub unsafe fn js_parse_regexp(s: &mut JSParseState) -> i32 {
    macro_rules! fail {
        () => {
            return -1;
        };
    }
    macro_rules! eof_error {
        () => {
            s.throw_syntax_error("unexpected end of regexp");
            fail!();
        };
    }
    macro_rules! eol_error {
        () => {
            s.throw_syntax_error("unexpected line terminator in regexp");
            fail!();
        };
    }
    macro_rules! invalid_utf8 {
        () => {
            (*s).throw_syntax_error("invalid UTF-8 sequence");
            fail!();
        };
    }

    let mut in_class = false;
    let mut offset = 0_usize;
    let mut res_string = StringContent::Empty;
    let mut res_string2 = StringContent::Empty;

    if let Ok(mut chars) = CStr::from_ptr(s.buf_ptr.cast())
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
        for (i, c) in chars {
            if !JSParseState::is_ident_next(c) {
                break;
            }
            if res_string2.push_char(c).is_err() {
                fail!();
            }
            offset = i;
        }

        s.token.val = TOK_REGEXP;
        s.token.data = TokenData::Regexp {
            body: s.ctx.create_string_from_content(res_string),
            flags: s.ctx.create_string_from_content(res_string2),
        };
        s.buf_ptr = s.buf_ptr.add(offset);
        0
    } else {
        invalid_utf8!();
    }
}
