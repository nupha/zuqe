use crate::{
    builtins::regexp::RegExpData,
    string::{CharCode, StringBuf, Utf8Stream},
};

use super::{
    JSParseState,
    tokenize::{TokenData, TokenKind},
};

impl<'s> JSParseState<'s> {
    /// input stream starts with '/' char
    fn parse_regexp_internal(&mut self, inp: &mut Utf8Stream) -> Result<(), i32> {
        debug_assert_eq!(inp.current_byte(), Some(b'/'));
        inp.move_cursor_by(1); // skip  leading '/'

        let mut in_class = false;
        let mut pattern_ok = false;
        let mut pattern_str = StringBuf::Empty;

        // pattern
        while let Some((i, sc)) = inp.next() {
            if pattern_str.len() == pattern_str.capacity() {
                pattern_str.try_reserve(1).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?
            }

            match sc {
                CharCode::Char('\n' | '\r') => {
                    self.throw_syntax_error("unexpected line terminator in regexp");
                    return Err(-1);
                }
                CharCode::Char('/') => {
                    if !in_class {
                        pattern_ok = true;
                        break;
                    } else {
                        let _ = pattern_str.push_u8(b'/');
                    }
                }
                CharCode::Char('[') => {
                    in_class = true;
                    let _ = pattern_str.push_u8(b'[');
                }
                CharCode::Char(']') => {
                    // XXX: incorrect as the first character in a class
                    in_class = false;
                    let _ = pattern_str.push_u8(b']');
                }
                CharCode::Char('\\') => {
                    let _ = pattern_str.push_u8(b'\\');

                    if let Some((_, sc2)) = inp.next() {
                        match sc2 {
                            CharCode::Char('\n' | '\r' | '\u{2028}' | '\u{2029}') => {
                                self.throw_syntax_error("unexpected line terminator in regexp");
                                return Err(-1);
                            }
                            CharCode::Char(_) => {
                                pattern_str.push_char(sc2).map_err(|_| {
                                    self.ctx.throw_out_of_memory();
                                    -1
                                })?;
                                continue;
                            }
                            CharCode::Code(_) => {
                                self.throw_syntax_error("invalid utf-8 sequence");
                                return Err(-1);
                            }
                        }
                    } else {
                        break; // EOF
                    }
                }
                CharCode::Char(c) => {
                    let _ = pattern_str.push_char(c);
                }
                CharCode::Code(c) => {
                    // inp.move_cursor_by(-1);
                    // println!(
                    //     "[O.o] bad({c}): {i}, {:?} {:?}",
                    //     inp.current_byte(),
                    //     inp.remainder_bytes()
                    // );
                    // let c1 = inp.decode_char(i);
                    // println!("[O.o] {c1:?}");
                    // let _ = pattern_str.push_u32(c);

                    self.throw_syntax_error("invalid utf-8 sequence");
                    return Err(-1);
                }
            }
        }

        if !pattern_ok {
            self.throw_syntax_error("expected end of regexp");
            return Err(-1);
        }

        // flags
        let mut flags_str = StringBuf::Empty;
        while let Some((_, sc)) = inp.next() {
            match sc {
                CharCode::Char(c) => {
                    if JSParseState::is_ident_next(c) {
                        flags_str.push_char(sc).map_err(|_| {
                            self.ctx.throw_out_of_memory();
                            -1
                        })?;
                    } else {
                        inp.move_cursor_by(-(c.len_utf8() as i32)); // rewind
                        break;
                    }
                }
                CharCode::Code(_) => {
                    self.throw_syntax_error("invalid utf-8 sequence");
                    return Err(-1);
                }
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

        self.token.kind = TokenKind::REGEXP;
        self.token.data = TokenData::Regexp(id);

        self.buf_ptr = inp.current_ptr().map_or(self.input_end, |x| x.as_ptr());
        Ok(())
    }

    pub(super) fn parse_regexp(&mut self) -> Result<(), i32> {
        if self.buf_ptr < self.input_end {
            let buf = unsafe {
                std::slice::from_raw_parts(
                    self.buf_ptr,
                    self.input_end.offset_from(self.buf_ptr) as usize,
                )
            };
            self.parse_regexp_internal(&mut Utf8Stream::new(buf))
        } else {
            Ok(())
        }
    }
}
