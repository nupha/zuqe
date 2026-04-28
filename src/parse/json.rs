// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use smallvec::SmallVec;

use crate::{
    atom::{Atom, AtomKind},
    context::ScopedContext,
    error::JSResult,
    number::{ATOD_ACCEPT_BIN_OCT, js_aton},
    object::JS_PROP_C_W_E,
    runtime::js_check_stack_overflow,
    string::{CharCodeDecoder, Utf8Stream, escape_str},
    utils::unlikely,
    value::JSValue,
};

use super::{
    JSParseState,
    tokenize::{TokenData, TokenKind},
};

/// allow extended JSON
pub const JS_PARSE_JSON_EXT: i32 = 1;

impl<'s> JSParseState<'s> {
    fn json_next_token(&mut self) -> Result<(), i32> {
        if js_check_stack_overflow(self.ctx().rt_mut(), 0) {
            self.throw_syntax_error("stack overflow");
            return Err(-1);
        }

        self.clear_token_data();

        enum Todo {
            Token(u8),
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
        let mut input = Utf8Stream::new(buf);
        let mut todo: Option<Todo> = None;

        while let Some(b) = input.peek() {
            self.token.line_num = self.line_num;
            self.token.ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());

            match b {
                quote @ (b'\'' | b'"') => {
                    if quote == b'\'' && !self.ext_json {
                        // standard JSON does not accept single quoted strings
                        todo = Some(Todo::Token(b'\''));
                        break;
                    } else {
                        input.seek_relative(1); // skip starting quote
                        if self
                            .parse_string(&mut input, quote as _, None, true)
                            .is_ok()
                        {
                            debug_assert_eq!(input.peek_relative(-1).unwrap(), quote);
                            self.buf_ptr =
                                input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                            return Ok(());
                        } else {
                            todo = Some(Todo::Fail);
                            break;
                        }
                    }
                }
                c @ (b'\r' | b'\n') => {
                    if c == b'\r'
                        && let Some(b'\n') = input.peek_relative(1)
                    {
                        input.seek_relative(1);
                    }
                    input.seek_relative(1);
                    self.line_num += 1;
                    continue;
                }
                //b'\f' | b'\v'
                c @ (b'\x0c' | b'\x0b') => {
                    if !self.ext_json {
                        // JSONWhitespace does not match <VT>, nor <FF>
                        todo = Some(Todo::Token(c));
                        break;
                    } else {
                        input.seek_relative(1);
                        continue;
                    }
                }
                b' ' | b'\t' => {
                    input.seek_relative(1);
                    continue;
                }
                b'/' => {
                    if !self.ext_json {
                        // JSON does not accept comments
                        todo = Some(Todo::Token(b'/'));
                        break;
                    }

                    match input.peek_relative(1) {
                        Some(b'*') => {
                            // block comment
                            input.seek_relative(2); // skip '/*'
                            if self.skip_block_comment(&mut input) {
                                continue;
                            } else {
                                debug_assert!(input.is_end());
                                self.throw_syntax_error("unexpected end of comment");
                                todo = Some(Todo::Fail);
                                break;
                            }
                        }
                        Some(b'/') => {
                            // line comment
                            input.seek_relative(2); // skip '//'
                            self.skip_line_comment(&mut input);
                        }
                        _ => {
                            todo = Some(Todo::Token(b'/'));
                            break;
                        }
                    }
                }
                c @ (b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$') => {
                    // identifier : only pure ascii characters are accepted
                    input.seek_relative(1);
                    if let Ok(atom) = self.json_parse_ident(&mut input, c) {
                        self.token.data = TokenData::Ident {
                            atom,
                            has_escape: false,
                            is_reserved: false,
                        };
                        self.token.kind = TokenKind::IDENT;
                        self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'+' => {
                    if !self.ext_json || input.peek_relative(1).is_none_or(|b| !b.is_ascii_digit())
                    {
                        todo = Some(Todo::Token(b'+'));
                        break;
                    }

                    if let Ok(n) = self.json_parse_number(input.remainder()) {
                        input.seek_relative(n as _);
                        self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'-' => {
                    if input.peek_relative(1).is_none_or(|b| !b.is_ascii_digit()) {
                        todo = Some(Todo::Token(b'-'));
                        break;
                    }

                    if let Ok(n) = self.json_parse_number(input.remainder()) {
                        input.seek_relative(n as _);
                        self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'0' => {
                    if input.peek_relative(1).is_some_and(|b| b.is_ascii_digit()) {
                        todo = Some(Todo::Token(b'0'));
                        break;
                    }

                    if let Ok(n) = self.json_parse_number(input.remainder()) {
                        input.seek_relative(n as _);
                        self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                b'1'..=b'9' => {
                    // let p0 = input.current_ptr().unwrap().as_ptr();
                    if let Ok(n) = self.json_parse_number(input.remainder()) {
                        input.seek_relative(n as i32);
                        self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
                    } else {
                        todo = Some(Todo::Fail);
                        break;
                    }
                }
                c if c.is_ascii() => {
                    todo = Some(Todo::Token(c));
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
                    self.token.kind = TokenKind::Char(c as _);
                    input.seek_relative(1);
                    self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                    Ok(())
                }
                Todo::Fail => {
                    self.token.kind = TokenKind::ERROR;
                    Err(-1)
                }
            }
        } else {
            self.token.kind = TokenKind::EOF;
            self.token.data = TokenData::None;
            self.buf_ptr = self.input_end;
            Ok(())
        }
    }

    // Returns consumed bytes
    fn json_parse_number(&mut self, bytes: &[u8]) -> Result<usize, i32> {
        let flags: i32;
        let radix: i32;

        if !self.ext_json {
            flags = 0;
            radix = 10;
        } else {
            flags = ATOD_ACCEPT_BIN_OCT;
            radix = 0;
        }

        if let Ok((num, consumed)) = js_aton(self.ctx(), bytes, radix, flags) {
            self.token.kind = TokenKind::NUMBER;
            self.token.data = TokenData::Num(num);
            Ok(consumed)
        } else {
            self.token.kind = TokenKind::ERROR;
            Err(-1)
        }
    }

    fn json_parse_ident(&mut self, inp: &mut Utf8Stream, first_char: u8) -> Result<Atom, i32> {
        let mut buf = SmallVec::<[u8; 64]>::new();
        buf.push(first_char);

        while let Some(c) = inp.peek()
            && c.is_ascii()
            && Self::is_ident_next(c as _)
        {
            if buf.capacity() - buf.len() >= 6 {
                buf.try_reserve(64).map_err(|_| {
                    self.ctx().throw_out_of_memory();
                    -1
                })?;
            }
            buf.push(c);
            inp.seek_relative(1);
        }

        Atom::from_str(
            self.ctx().rt_ptr(),
            std::str::from_utf8(&buf).map_err(|_| -1)?,
        )
    }

    fn json_parse_value(&mut self) -> JSResult {
        let mut val: JSValue;

        match self.token.kind {
            TokenKind::Char('{') => {
                self.json_next_token()?;

                val = self.ctx().create_base_object()?.into();
                if self.token.kind != TokenKind::Char('}') {
                    loop {
                        let prop_name = if self.token.kind == TokenKind::STRING {
                            Atom::from_str(
                                self.ctx().rt_ptr(),
                                &escape_str(&self.token.data.string().unwrap().to_utf8()),
                            )?
                        } else if self.ext_json && self.token.kind == TokenKind::IDENT {
                            self.token.data.ident_atom().unwrap()
                        } else {
                            self.throw_syntax_error("expecting property name");
                            return Err(-1);
                        };

                        self.json_next_token()?;
                        json_parse_expect(self, b':')?;

                        let prop_val = self.json_parse_value()?;
                        val.define_property_value(self.ctx(), prop_name, prop_val, JS_PROP_C_W_E)?;

                        if self.token.kind != TokenKind::Char(',') {
                            break;
                        }
                        self.json_next_token()?;
                        if self.ext_json && self.token.kind == TokenKind::Char('}') {
                            break;
                        }
                    }
                }
                json_parse_expect(self, b'}')?;
            }

            TokenKind::Char('[') => {
                self.json_next_token()?;

                let mut lst = Vec::<JSValue>::new();
                if self.token.kind != TokenKind::Char(']') {
                    'arr: loop {
                        let el = self.json_parse_value()?;

                        if lst.len() == lst.capacity() {
                            lst.try_reserve(32).map_err(|_| {
                                self.ctx().throw_out_of_memory();
                                -1
                            })?;
                        }
                        lst.push(el);

                        if self.token.kind != TokenKind::Char(',') {
                            break 'arr;
                        }

                        self.json_next_token()?;

                        if self.ext_json && self.token.kind == TokenKind::Char(']') {
                            break 'arr;
                        }
                    }
                }

                // make Array
                lst.shrink_to_fit();
                val = crate::array::Array::from_values(self.ctx(), lst)?.into();

                json_parse_expect(self, b']')?;
            }
            TokenKind::STRING => {
                let sbuf = self.token.data.take_string().unwrap();
                val = self.ctx().create_string_with_buf(sbuf)?.into();
                self.json_next_token()?;
            }
            TokenKind::NUMBER => {
                val = JSValue::from(self.token.data.number().unwrap());
                self.json_next_token()?;
            }
            TokenKind::IDENT => {
                val = match self.token.data.ident_atom().unwrap() {
                    Atom::CONST_true => JSValue::Bool(true),
                    Atom::CONST_false => JSValue::Bool(false),
                    Atom::CONST_null => JSValue::NULL,
                    _ => {
                        if self.token.kind == TokenKind::EOF {
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
                if self.token.kind == TokenKind::EOF {
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

fn json_parse_expect(s: &mut JSParseState, ch: u8) -> Result<(), i32> {
    if s.token.kind != TokenKind::Char(ch as _) {
        s.throw_syntax_error(&format!("expecting '{ch}'"));
        Err(-1)
    } else {
        s.json_next_token()
    }
}

impl<'c> ScopedContext<'c> {
    pub fn json_parse(self, source: &[u8], filename: &str, flags: i32) -> JSResult {
        // Note: parser will be dropped within this function.
        // so, 'source' and 'filename' are safe w/o lifetime annotation

        let mut parser = JSParseState::new_for_json(self, source, filename);
        parser.ext_json = flags & JS_PARSE_JSON_EXT != 0;

        parser.json_next_token()?;
        let val = parser.json_parse_value()?;

        if unlikely(parser.token.kind != TokenKind::EOF) {
            parser.throw_syntax_error("unexpected data at the end");
            Err(-1)
        } else {
            parser.ctx().set_return_value(val);
            Ok(val)
        }
    }
}
