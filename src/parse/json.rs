use smallvec::SmallVec;

use crate::{
    atom::{Atom, AtomKind},
    context::ContextImpl,
    error::JSResult,
    number::{ATOD_ACCEPT_BIN_OCT, js_atof},
    object::JS_PROP_C_W_E,
    parse::{
        JSParseState,
        token::{
            CHAR_CURLY_L, CHAR_SQUARE_L, TOK_EOF, TOK_ERROR, TOK_IDENT, TOK_NUMBER, TOK_STRING,
            TokenData,
        },
    },
    runtime::js_check_stack_overflow,
    string::{UTF8_CHAR_LEN_MAX, Utf8CharIter, escape_str},
    utils::unlikely,
    value::JSValue,
};

/// allow extended JSON
pub const JS_PARSE_JSON_EXT: i32 = 1;

impl<'s> JSParseState<'s> {
    fn json_next_token(&mut self) -> Result<(), i32> {
        if js_check_stack_overflow(&self.ctx.rt_ptr(), 0) {
            self.throw_syntax_error("stack overflow");
            return Err(-1);
        }

        self.clear_token_data();

        enum Todo {
            Token(i32),
            Fail,
        }

        self.last_ptr = self.buf_ptr;
        self.last_line_num = self.token.line_num;

        let buf = unsafe {
            if self.buf_ptr < self.input_end {
                std::slice::from_raw_parts(
                    self.buf_ptr,
                    self.input_end.offset_from(self.buf_ptr) as usize,
                )
            } else {
                &[]
            }
        };
        let mut ins = Utf8CharIter::new(buf);
        let mut todo: Option<Todo> = None;

        while let Some(b) = ins.peek_offset(0) {
            self.token.line_num = self.line_num;
            self.token.ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());

            match b {
                quote @ (b'\'' | b'"') => {
                    if quote == b'\'' && !self.ext_json {
                        // standard JSON does not accept single quoted strings
                        todo = Some(Todo::Token('\'' as _));
                        break;
                    } else {
                        ins.seek_offset(1); // skip starting quote

                        if let Ok(n) =
                            self.parse_string(quote as _, true, ins.remainder_bytes(), None)
                        {
                            ins.seek_offset(n as _); // at next char of ending quote
                            debug_assert_eq!(ins.peek_offset(-1).unwrap(), quote);

                            self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                            return Ok(());
                        } else {
                            todo = Some(Todo::Fail);
                            break;
                        }
                    }
                }
                c @ (b'\r' | b'\n') => {
                    if c == b'\r'
                        && let Some(b'\n') = ins.peek_offset(1)
                    {
                        ins.seek_offset(1);
                    }
                    ins.seek_offset(1);
                    self.line_num += 1;
                    continue;
                }
                //b'\f' | b'\v'
                c @ (b'\x0c' | b'\x0b') => {
                    if !self.ext_json {
                        // JSONWhitespace does not match <VT>, nor <FF>
                        todo = Some(Todo::Token(c as _));
                        break;
                    } else {
                        ins.seek_offset(1);
                        continue;
                    }
                }
                b' ' | b'\t' => {
                    ins.seek_offset(1);
                    continue;
                }
                b'/' => {
                    if !self.ext_json {
                        // JSON does not accept comments
                        todo = Some(Todo::Token('/' as _));
                        break;
                    }

                    match ins.peek_offset(1) {
                        Some(b'*') => {
                            // block comment
                            ins.seek_offset(2); // skip '/*'
                            if let Some(pos) = ins.current_index() {
                                if let Some(n) = self.skip_block_comment(&buf[pos..]) {
                                    ins.seek_offset(n as _);
                                    continue;
                                } else {
                                    ins.end();
                                    self.throw_syntax_error("unexpected end of comment");
                                    todo = Some(Todo::Fail);
                                    break;
                                }
                            }
                        }
                        Some(b'/') => {
                            // line comment
                            ins.seek_offset(2); // skip '//'
                            if let Some(pos) = ins.current_index() {
                                let n = self.skip_line_comment(&buf[pos..]);
                                ins.seek_offset(n as _);
                                continue;
                            }
                        }
                        _ => {
                            todo = Some(Todo::Token('/' as _));
                            break;
                        }
                    }
                }
                c @ (b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$') => {
                    // identifier : only pure ascii characters are accepted
                    ins.seek_offset(1);
                    if let Ok(atom) = self.json_parse_ident(&mut ins, c) {
                        self.token.data = TokenData::Ident {
                            atom,
                            has_escape: false,
                            is_reserved: false,
                        };
                        self.token.val = TOK_IDENT;
                        self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'+' => {
                    if !self.ext_json || ins.peek_offset(1).is_none_or(|b| !b.is_ascii_digit()) {
                        todo = Some(Todo::Token('+' as _));
                        break;
                    }

                    if let Ok(n) = self.json_parse_value_number(ins.current_ptr().unwrap().as_ptr())
                    {
                        ins.seek_offset(n as _);
                        self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'-' => {
                    if ins.peek_offset(1).is_none_or(|b| !b.is_ascii_digit()) {
                        todo = Some(Todo::Token('-' as _));
                        break;
                    }

                    if let Ok(n) = self.json_parse_value_number(ins.current_ptr().unwrap().as_ptr())
                    {
                        ins.seek_offset(n as _);
                        self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'0' => {
                    if ins.peek_offset(1).is_some_and(|b| b.is_ascii_digit()) {
                        todo = Some(Todo::Token('0' as _));
                        break;
                    }

                    if let Ok(n) = self.json_parse_value_number(ins.current_ptr().unwrap().as_ptr())
                    {
                        ins.seek_offset(n as _);
                        self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'1'..=b'9' => {
                    let p0 = ins.current_ptr().unwrap().as_ptr();
                    if let Ok(n) = self.json_parse_value_number(p0) {
                        ins.seek_offset(n as i32);
                        self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                c if c.is_ascii() => {
                    todo = Some(Todo::Token(c as _));
                    break;
                }
                _ => {
                    self.throw_syntax_error("unexpected character");
                    todo = Some(Todo::Fail);
                    break;
                }
            }
        }

        if let Some(todo) = todo {
            match todo {
                Todo::Token(c) => {
                    self.token.val = c;
                    ins.seek_offset(1);
                    self.buf_ptr = ins.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                    Ok(())
                }
                Todo::Fail => {
                    self.token.val = TOK_ERROR;
                    Err(-1)
                }
            }
        } else {
            self.token.val = TOK_EOF;
            self.token.data = TokenData::None;
            self.buf_ptr = self.input_end;
            Ok(())
        }
    }

    // Returns consumed bytes
    fn json_parse_value_number(&mut self, mut p: *const u8) -> Result<usize, i32> {
        let flags: i32;
        let radix: i32;

        if !self.ext_json {
            flags = 0;
            radix = 10;
        } else {
            flags = ATOD_ACCEPT_BIN_OCT;
            radix = 0;
        }

        let p0 = p;

        if let Ok(num) = unsafe { js_atof(&self.ctx, p, &mut p, radix, flags) } {
            self.token.val = TOK_NUMBER;
            self.token.data = TokenData::Num(num);
            Ok(unsafe { p.offset_from(p0) as usize })
        } else {
            self.token.val = TOK_ERROR;
            Err(-1)
        }
    }

    fn json_parse_ident(&mut self, inp: &mut Utf8CharIter, first_char: u8) -> Result<Atom, i32> {
        let mut buf = SmallVec::<[u8; 64]>::new();
        buf.push(first_char);

        while let Some(c) = inp.peek_offset(0)
            && c.is_ascii()
            && Self::is_ident_next(c as _)
        {
            if buf.capacity() - buf.len() >= UTF8_CHAR_LEN_MAX {
                buf.try_reserve(64).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
            }
            buf.push(c);
            inp.seek_offset(1);
        }

        let atom = Atom::new(
            self.ctx.rt_ptr(),
            std::str::from_utf8(&buf).map_err(|_| -1)?,
        )?;
        Ok(atom)
    }

    fn json_parse_value(&mut self) -> JSResult {
        let mut val: JSValue;

        match self.token.val {
            // '{'
            CHAR_CURLY_L => {
                self.json_next_token()?;

                val = self.ctx.create_base_object()?.into();
                if self.token.val != b'}' as i32 {
                    loop {
                        let prop_name = if self.token.val == TOK_STRING {
                            Atom::from_str(
                                self.ctx.rt_ptr(),
                                &escape_str(&self.token.data.string().unwrap().to_utf8()),
                                AtomKind::String,
                            )?
                        } else if self.ext_json && self.token.val == TOK_IDENT {
                            self.token.data.ident_atom().unwrap()
                        } else {
                            self.throw_syntax_error("expecting property name");
                            return Err(-1);
                        };

                        self.json_next_token()?;
                        json_parse_expect(self, ':' as i32)?;

                        let prop_val = self.json_parse_value()?;
                        val.define_property_value(&self.ctx, prop_name, prop_val, JS_PROP_C_W_E)?;

                        if self.token.val != ',' as i32 {
                            break;
                        }
                        self.json_next_token()?;
                        if self.ext_json && self.token.val == b'}' as i32 {
                            break;
                        }
                    }
                }
                json_parse_expect(self, b'}' as i32)?;
            }
            // '['
            CHAR_SQUARE_L => {
                self.json_next_token()?;

                let mut lst = Vec::<JSValue>::new();
                if self.token.val != ']' as i32 {
                    'arr: loop {
                        let el = self.json_parse_value()?;

                        if lst.len() == lst.capacity() {
                            lst.try_reserve(32).map_err(|_| {
                                self.ctx.throw_out_of_memory();
                                -1
                            })?;
                        }
                        lst.push(el);

                        if self.token.val != ',' as i32 {
                            break 'arr;
                        }

                        self.json_next_token()?;

                        if self.ext_json && self.token.val == b']' as i32 {
                            break 'arr;
                        }
                    }
                }

                // make Array
                lst.shrink_to_fit();
                val = self.ctx.create_array_with_values(lst)?.into();

                json_parse_expect(self, b']' as i32)?;
            }
            TOK_STRING => {
                let sbuf = self.token.data.take_string().unwrap();
                val = self.ctx.create_string_with_buf(sbuf)?.into();
                self.json_next_token()?;
            }
            TOK_NUMBER => {
                val = JSValue::from(self.token.data.number().unwrap());
                self.json_next_token()?;
            }
            TOK_IDENT => {
                val = match self.token.data.ident_atom().unwrap() {
                    Atom::CONST_true => JSValue::Bool(true),
                    Atom::CONST_false => JSValue::Bool(false),
                    Atom::CONST_null => JSValue::NULL,
                    _ => {
                        if self.token.val == TOK_EOF {
                            self.throw_syntax_error("unexpected end of input");
                        } else {
                            self.throw_syntax_error(&format!("unexpected token: '{}'", unsafe {
                                let t = std::slice::from_raw_parts(
                                    self.token.ptr,
                                    self.buf_ptr.offset_from(self.token.ptr) as usize,
                                );
                                std::str::from_utf8_unchecked(t)
                            }));
                        }
                        return Err(-1);
                    }
                };

                self.json_next_token()?;
            }
            _ => {
                if self.token.val == TOK_EOF {
                    self.throw_syntax_error("unexpected end of input");
                } else {
                    self.throw_syntax_error(&format!("unexpected token: '{}'", unsafe {
                        let t = std::slice::from_raw_parts(
                            self.token.ptr,
                            self.buf_ptr.offset_from(self.token.ptr) as usize,
                        );
                        std::str::from_utf8_unchecked(t)
                    }));
                }
                return Err(-1);
            }
        }

        Ok(val)
    }
}

fn json_parse_expect(s: &mut JSParseState, tok: i32) -> Result<(), i32> {
    if s.token.val != tok {
        s.throw_syntax_error(&format!("expecting '{tok}'"));
        Err(-1)
    } else {
        s.json_next_token()
    }
}

impl ContextImpl {
    pub fn json_parse(&self, source: &[u8], filename: &str, flags: i32) -> JSResult {
        // Note: parser will be dropped within this function.
        // so, 'source' and 'filename' are safe w/o lifetime annotation

        let mut parser = JSParseState::new_for_json(self, source, filename);
        parser.ext_json = flags & JS_PARSE_JSON_EXT != 0;

        parser.json_next_token()?;
        let val = parser.json_parse_value()?;

        if unlikely(parser.token.val != TOK_EOF) {
            parser.throw_syntax_error("unexpected data at the end");
            Err(-1)
        } else {
            Ok(val)
        }
    }
}
