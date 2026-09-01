// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use smallvec::SmallVec;

use crate::{
    atom::{Atom, AtomKind},
    error::JSResult,
    number::{ATOD_ACCEPT_BIN_OCT, js_aton},
    object::JS_PROP_C_W_E,
    runtime::js_check_stack_overflow,
    string::{CharCodeDecoder, StringBuf, Utf8Stream},
    utils::unlikely,
    value::JSValue,
};

use super::{
    JSParseState,
    tokenize::{TokenData, TokenKind},
};

/// allow extended JSON
pub const JS_PARSE_JSON_EXT: i32 = 1;

// ---------------------------------------------------------------------------
// JSONParseRecord — parallels the JSON value tree to support
// json-parse-with-source reviver context.
// ---------------------------------------------------------------------------

/// A node in the parse-record tree that mirrors the JSON AST.  During
/// parsing we capture, for every value:
///   • `value` — the original `JSValue` (used for forward-modification
///     detection via [`JSValue::is_same`]).
///   • For primitives (string, number, boolean, null): `source_pos` /
///     `source_len` give the exact byte range in the source text.
///   • For objects: `children` holds the per-key records.
///   • For arrays: `elements` holds the per-index records.
#[derive(Debug, Clone, Default)]
pub(crate) struct JSONParseRecord {
    pub value: JSValue,
    pub source_pos: u32,
    pub source_len: u32,
    pub children: Vec<(Atom, JSONParseRecord)>,
    pub elements: Vec<JSONParseRecord>,
}

impl JSONParseRecord {
    #[inline(always)]
    pub const fn is_primitive(&self) -> bool {
        !self.value.is_object()
    }

    pub fn find_child(&self, name: Atom) -> Option<&JSONParseRecord> {
        if !self.elements.is_empty() {
            // Array — look up by integer index
            let idx = name.to_small_int()?;
            self.elements.get(idx as usize)
        } else {
            // Object — linear scan by Atom
            self.children
                .iter()
                .find(|(a, _)| *a == name)
                .map(|(_, r)| r)
        }
    }
}

impl<'s> JSParseState<'s> {
    fn json_next_token(&mut self) -> Result<(), i32> {
        if js_check_stack_overflow(self.ctx().rt_mut(), 0) {
            return Err(self.throw_syntax_error("stack overflow"));
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
                        // Fast path: most JSON strings are plain UTF-8 with no
                        // escapes or control chars. Bulk-copy the raw byte run
                        // instead of decoding char-by-char; fall back to the
                        // general parser only when an escape/control appears.
                        if !self.json_string_plain(&mut input, quote as _) {
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
                        self.buf_ptr = input.current_ptr().map_or(self.input_end, |p| p.as_ptr());
                        return Ok(());
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
            match num {
                crate::number::NumValue::Float(n) => self.token.data = TokenData::Num(n),
                // JSON numbers never carry an `n` suffix; a BigInt result here
                // would mean a malformed (feature-gated) input, reject it.
                #[cfg(feature = "big_int")]
                crate::number::NumValue::BigInt(_) => {
                    self.token.kind = TokenKind::ERROR;
                    return Err(-1);
                }
            }
            Ok(consumed)
        } else {
            self.token.kind = TokenKind::ERROR;
            Err(-1)
        }
    }

    /// Fast-path JSON string scanner: if the remainder is a plain string
    /// (no backslash escape, no control char, closes at `sep`), fill the
    /// token directly from the raw UTF-8 run. Returns true when consumed.
    fn json_string_plain(&mut self, inp: &mut Utf8Stream, sep: u8) -> bool {
        let start = inp.remainder();
        let end = start
            .iter()
            .position(|&c| c == sep || c == b'\\' || c < 0x20);
        let Some(len) = end else {
            return false; // unterminated; let general parser report the error
        };
        if start[len] != sep {
            return false; // escape or control char present; slow path
        }
        let data = start[..len].to_vec();
        let sbuf = match StringBuf::from_utf8(data) {
            Ok(s) => s,
            // from_utf8 only fails on allocation; report the same error path
            // by falling back to the general parser (which will rethrow).
            Err(_) => return false,
        };
        self.token.kind = TokenKind::STRING;
        self.token.data = TokenData::Str {
            str: sbuf,
            sep: sep as i32,
        };
        inp.seek_relative((len + 1) as i32); // content + closing quote
        true
    }

    fn json_parse_ident(&mut self, inp: &mut Utf8Stream, first_char: u8) -> Result<Atom, i32> {
        let mut buf = SmallVec::<[u8; 64]>::new();
        buf.push(first_char);

        while let Some(c) = inp.peek()
            && c.is_ascii()
            && Self::is_ident_next(c as _)
        {
            if buf.capacity() - buf.len() >= 6 {
                buf.try_reserve(64)
                    .map_err(|_| self.ctx().throw_out_of_memory())?;
            }
            buf.push(c);
            inp.seek_relative(1);
        }

        let ident = std::str::from_utf8(&buf).map_err(|_| -1)?;
        match ident {
            "true" => return Ok(Atom::CONST_true),
            "false" => return Ok(Atom::CONST_false),
            "null" => return Ok(Atom::CONST_null),
            _ => {}
        }
        Atom::from_str(self.ctx(), ident)
    }

    fn json_parse_value(&mut self, record: Option<&mut JSONParseRecord>) -> JSResult {
        let mut val: JSValue;

        match self.token.kind {
            TokenKind::Char('{') => {
                self.json_next_token()?;

                val = self.ctx().create_base_object()?.into();
                let should_record = record.is_some();
                let mut children: Vec<(Atom, JSONParseRecord)> = Vec::new();

                if self.token.kind != TokenKind::Char('}') {
                    loop {
                        let prop_name = if self.token.kind == TokenKind::STRING {
                            // sink the decoded token buffer directly into an atom;
                            // no re-escaping / intermediate copies.
                            let sbuf = self.token.data.take_string().unwrap();
                            let ctx = self.ctx();
                            ctx.create_string_with_buf(sbuf)?
                                .to_atom(ctx, AtomKind::String)?
                        } else if self.ext_json && self.token.kind == TokenKind::IDENT {
                            self.token.data.ident_atom().unwrap()
                        } else {
                            return Err(self.throw_syntax_error("expecting property name"));
                        };

                        self.json_next_token()?;
                        json_parse_expect(self, b':')?;

                        let mut child_record = JSONParseRecord::default();
                        let prop_val = if should_record {
                            self.json_parse_value(Some(&mut child_record))?
                        } else {
                            self.json_parse_value(None)?
                        };

                        if should_record {
                            // push the Atom back after json_next_token advanced
                            children.push((prop_name, child_record));
                        }

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

                if let Some(pr) = record {
                    pr.value = val;
                    pr.children = children;
                    pr.elements = Vec::new();
                }
            }

            TokenKind::Char('[') => {
                self.json_next_token()?;

                let mut lst = Vec::<JSValue>::new();
                let should_record = record.is_some();
                let mut elements: Vec<JSONParseRecord> = Vec::new();

                if self.token.kind != TokenKind::Char(']') {
                    'arr: loop {
                        let mut child_record = JSONParseRecord::default();
                        let el = if should_record {
                            self.json_parse_value(Some(&mut child_record))?
                        } else {
                            self.json_parse_value(None)?
                        };

                        if should_record {
                            elements.push(child_record);
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

                val = crate::array::Array::from_values(self.ctx(), lst)?.into();

                json_parse_expect(self, b']')?;

                if let Some(pr) = record {
                    pr.value = val;
                    pr.children = Vec::new();
                    pr.elements = elements;
                }
            }
            TokenKind::STRING => {
                // Record source position before consuming the next token
                let source_start = (self.token.ptr as usize) - (self.input.as_ptr() as usize);
                let source_end = (self.buf_ptr as usize) - (self.input.as_ptr() as usize);

                let sbuf = self.token.data.take_string().unwrap();
                val = self
                    .ctx()
                    .create_string_with_buf(sbuf)
                    .map(JSValue::make_string)?;

                if let Some(pr) = record {
                    pr.value = val;
                    pr.source_pos = source_start as u32;
                    pr.source_len = (source_end - source_start) as u32;
                }

                self.json_next_token()?;
            }
            TokenKind::NUMBER => {
                let source_start = (self.token.ptr as usize) - (self.input.as_ptr() as usize);
                let source_end = (self.buf_ptr as usize) - (self.input.as_ptr() as usize);

                val = JSValue::from(self.token.data.number().unwrap());

                if let Some(pr) = record {
                    pr.value = val;
                    pr.source_pos = source_start as u32;
                    pr.source_len = (source_end - source_start) as u32;
                }

                self.json_next_token()?;
            }
            TokenKind::IDENT => {
                let source_start = (self.token.ptr as usize) - (self.input.as_ptr() as usize);
                let source_end = (self.buf_ptr as usize) - (self.input.as_ptr() as usize);

                val = match self.token.data.ident_atom().unwrap().const_value() {
                    Some(v) => v,
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

                if let Some(pr) = record {
                    pr.value = val;
                    pr.source_pos = source_start as u32;
                    pr.source_len = (source_end - source_start) as u32;
                }

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

    pub fn json_parse(&mut self, record: Option<&mut JSONParseRecord>) -> JSResult {
        self.json_next_token()?;
        let val = self.json_parse_value(record)?;

        if unlikely(self.token.kind != TokenKind::EOF) {
            Err(self.throw_syntax_error("unexpected data at the end"))
        } else {
            Ok(val)
        }
    }
}

fn json_parse_expect(s: &mut JSParseState, ch: u8) -> Result<(), i32> {
    if s.token.kind != TokenKind::Char(ch as _) {
        Err(s.throw_syntax_error(&format!("expecting '{ch}'")))
    } else {
        s.json_next_token()
    }
}
