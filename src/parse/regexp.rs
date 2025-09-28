use crate::{
    branch_hints::unlikely,
    parse::token::{TOK_REGEXP, TokenData},
    regexp::RegExpData,
    string::{CP_LS, CP_PS, StringBuf, unicode_from_utf8},
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub(super) fn parse_regexp(&mut self) -> Result<(), i32> {
        let mut in_class = false;
        let mut cursor = 0_usize;
        let mut pattern_str = StringBuf::Empty;
        let mut flags_str = StringBuf::Empty;

        let buf = unsafe {
            let len = self.buf_end.offset_from(self.buf_ptr) as usize;
            std::slice::from_raw_parts(self.buf_ptr, len)
        };

        let mut i = 1; // skip leading '/'

        while i < buf.len() {
            let c = buf[i];

            match c {
                b'\n' | b'\r' => {
                    self.throw_syntax_error("unexpected line terminator in regexp");
                    return Err(-1);
                }
                b'/' => {
                    if !in_class {
                        cursor = i + 1;
                        break;
                    }
                }
                b']' => {
                    // XXX: incorrect as the first character in a class
                    in_class = false;
                }
                b'\\' => {
                    pattern_str.push_u8(c).map_err(|_| {
                        self.ctx.throw_out_of_memory();
                        -1
                    })?;

                    let c1 = buf
                        .get(i + 1)
                        .ok_or(-1)
                        .and_then(|&x| {
                            if matches!(x, b'\n' | b'\r') {
                                Err(-1)
                            } else {
                                Ok(x)
                            }
                        })
                        .inspect_err(|_| {
                            self.throw_syntax_error("unexpected line terminator in regexp");
                        })?;

                    if c1 >= 0x80 {
                        let (ch, cnt) = unicode_from_utf8(&buf[i..])
                            .and_then(|(x, n)| {
                                if x as u32 > 0x10ffff {
                                    Err(-1)
                                } else {
                                    Ok((x, n))
                                }
                            })
                            .inspect_err(|_| {
                                self.throw_syntax_error("invalid utf-8 sequence");
                            })?;

                        pattern_str.push_char(ch).map_err(|_| {
                            self.ctx.throw_out_of_memory();
                            -1
                        })?;
                        i += cnt;
                        continue;
                    }
                }

                _ if c >= 0x80 => {
                    let (ch, cnt) = unicode_from_utf8(&buf[i..])
                        .and_then(|(x, n)| {
                            if x as u32 > 0x10ffff {
                                Err(-1)
                            } else {
                                Ok((x, n))
                            }
                        })
                        .inspect_err(|_| {
                            self.throw_syntax_error("invalid utf-8 sequence");
                        })?;

                    if unlikely(matches!(ch as u32, CP_LS | CP_PS)) {
                        self.throw_syntax_error("unexpected line terminator in regexp");
                        return Err(-1);
                    } else {
                        pattern_str.push_char(ch).map_err(|_| {
                            self.ctx.throw_out_of_memory();
                            -1
                        })?;
                        i += cnt;
                        continue;
                    }
                }

                _ => {}
            }

            // ascii char
            debug_assert!(c.is_ascii());
            pattern_str.push_u8(c).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
            i += 1;
        }

        let mut chars = std::str::from_utf8(buf)
            .map(|x| x.char_indices())
            .map_err(|_| {
                self.throw_syntax_error("invalid UTF-8 sequence");
                -1
            })?;

        let _ = chars.next(); // skip starting '/'

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
                break;
            }
            flags_str.push_char(c).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
            cursor = i + c.encode_utf8(&mut [0u8; 4]).len();
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
