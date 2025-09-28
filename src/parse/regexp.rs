use crate::{
    parse::token::{TOK_REGEXP, TokenData},
    regexp::RegExpData,
    string::StringBuf,
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub(super) fn parse_regexp(&mut self) -> Result<(), i32> {
        let mut chars = std::str::from_utf8(unsafe {
            let len = self.buf_end.offset_from(self.buf_ptr) as usize;
            std::slice::from_raw_parts(self.buf_ptr, len)
        })
        .map(|x| x.char_indices())
        .map_err(|_| {
            self.throw_syntax_error("invalid UTF-8 sequence");
            -1
        })?;

        let _ = chars.next(); // skip starting '/'

        let mut in_class = false;
        let mut cursor = 0_usize;
        let mut pattern_str = StringBuf::Empty;
        let mut flags_str = StringBuf::Empty;

        while let Some((i, mut c)) = chars.next() {
            match c {
                '\n' | '\r' => {
                    self.throw_syntax_error("unexpected end of regexp");
                    return Err(-1);
                }
                '/' => {
                    if !in_class {
                        cursor = i + 1;
                        break;
                    }
                }
                ']' => {
                    // XXX: incorrect as the first character in a class
                    in_class = false;
                }
                '\\' => {
                    pattern_str.push_char(c).map_err(|_| {
                        self.ctx.throw_out_of_memory();
                        -1
                    })?;

                    if let Some((i, c1)) = chars.next() {
                        cursor = i;

                        if matches!(c1, '\n' | '\r') {
                            self.throw_syntax_error("unexpected end of regexp");
                            return Err(-1);
                        } else {
                            c = c1;
                        }
                    } else {
                        self.throw_syntax_error("unexpected end of regexp");
                        return Err(-1);
                    }
                }
                _ => {}
            }

            pattern_str.push_char(c).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
        }

        // flags
        for (i, c) in chars {
            if !JSParseState::is_ident_next(c) {
                cursor = i + c.encode_utf8(&mut [0u8; 4]).len();
                break;
            }
            flags_str.push_char(c).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
        }

        // if RegExp is not supported, raise error here
        // self.throw_syntax_error("RegExp are not supported");
        // return Err(-1);

        let pattern = self.ctx.create_string_with_buf(pattern_str)?;
        let flags = self.ctx.create_string_with_buf(flags_str)?;

        // pre compile regexpdata
        let regexp = RegExpData::create(
            &self.ctx,
            pattern.content().clone(),
            flags.content().clone(),
        )?;
        self.ctx.rt_mut().regexp_precompiled.push(regexp);
        let id = self.ctx.rt().regexp_precompiled.len() - 1;

        self.token.val = TOK_REGEXP;
        self.token.data = TokenData::Regexp(id);

        self.buf_ptr = unsafe { self.buf_ptr.add(cursor) };
        Ok(())
    }
}
