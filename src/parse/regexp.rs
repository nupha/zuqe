use crate::{
    builtins::regexp::RegExpData,
    parse::tokenize::{TOK_REGEXP, TokenData},
    string::{CP_LS, CP_PS, StringBuf, unicode_from_utf8},
    utils::unlikely,
};

use super::JSParseState;

impl<'s> JSParseState<'s> {
    pub(super) fn parse_regexp(&mut self) -> Result<(), i32> {
        let mut in_class = false;
        let mut cursor = 0_usize;
        let mut pattern_str = StringBuf::Empty;
        let mut flags_str = StringBuf::Empty;

        let buf = unsafe {
            std::slice::from_raw_parts(
                self.buf_ptr,
                self.input_end.offset_from(self.buf_ptr) as usize,
            )
        };
        debug_assert_eq!(buf[0], b'/');

        let mut i = 1; // skip leading '/'

        // pattern
        while i < buf.len() {
            let mut c = buf[i];

            match c {
                b'\n' | b'\r' => {
                    self.throw_syntax_error("unexpected line terminator in regexp");
                    return Err(-1);
                }
                b'/' => {
                    if !in_class {
                        i += 1;
                        cursor = i;
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

                    // get next byte
                    i += 1;
                    c = buf
                        .get(i)
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

                    if c >= 0x80 {
                        let (ch, cnt) = unicode_from_utf8(&buf[i..])
                            .and_then(|(x, n)| if x > 0x10ffff { Err(-1) } else { Ok((x, n)) })
                            .inspect_err(|_| {
                                self.throw_syntax_error("invalid utf-8 sequence");
                            })?;

                        if unlikely(matches!(ch, CP_LS | CP_PS)) {
                            self.throw_syntax_error("unexpected line terminator in regexp");
                            return Err(-1);
                        } else {
                            pattern_str.push_u32(ch).map_err(|_| {
                                self.ctx.throw_out_of_memory();
                                -1
                            })?;
                            i += cnt;
                            continue;
                        }
                    }
                }

                _ if c >= 0x80 => {
                    let (ch, cnt) = unicode_from_utf8(&buf[i..])
                        .and_then(|(x, n)| if x > 0x10ffff { Err(-1) } else { Ok((x, n)) })
                        .inspect_err(|_| {
                            self.throw_syntax_error("invalid utf-8 sequence");
                        })?;

                    if unlikely(matches!(ch, CP_LS | CP_PS)) {
                        self.throw_syntax_error("unexpected line terminator in regexp");
                        return Err(-1);
                    } else {
                        pattern_str.push_u32(ch).map_err(|_| {
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

        // flags
        while i < buf.len() {
            let c = buf[i];
            let c = if unlikely(c >= 0x80) {
                let (ch, cnt) = unicode_from_utf8(&buf[i..])
                    .and_then(|(x, n)| if x > 0x10ffff { Err(-1) } else { Ok((x, n)) })
                    .inspect_err(|_| {
                        self.throw_syntax_error("invalid utf-8 sequence");
                    })?;
                i += cnt;
                ch
            } else {
                i += 1;
                c as _
            };

            if char::from_u32(c).is_none_or(|c| !JSParseState::is_ident_next(c)) {
                break;
            } else {
                flags_str.push_u32(c).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
                cursor = i;
            }
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
        self.ctx.rt_ptr().regexp_precompiled.push(regexp);
        let id = self.ctx.rt_ptr().regexp_precompiled.len() - 1;

        self.token.val = TOK_REGEXP;
        self.token.data = TokenData::Regexp(id);

        self.buf_ptr = unsafe { self.buf_ptr.add(cursor) };
        Ok(())
    }
}
