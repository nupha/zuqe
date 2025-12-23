use crate::{
    atom::Atom,
    bytecode::OpCode,
    object::{JS_PROP_ENUMERABLE, JS_PROP_THROW, seal_template_obj},
    string::{CharCode, StringBuf, Utf8Stream},
    utils::unlikely,
    value::JSValue,
};

use super::{
    state::JSParseState,
    tokenize::{Token, TokenData, TokenKind},
};

impl<'s> JSParseState<'s> {
    /// returns number of bytes consumed (include ending delimitor)
    pub(super) fn parse_template_part(&mut self, buf: &[u8]) -> Result<usize, i32> {
        let mut sbuf = StringBuf::Empty;
        let mut chars = Utf8Stream::new(buf).peekable();

        let mut sep: char = '\0';
        let mut consume = 0;

        while let Some((i, sc)) = chars.next() {
            match sc {
                CharCode::Char('`') => {
                    sep = '`';
                    consume = i + 1; // next to '`'
                    break;
                }
                CharCode::Char('$')
                    if chars
                        .peek()
                        .is_some_and(|(_, a)| matches!(a, CharCode::Char('{'))) =>
                {
                    // ${ template start or middle part
                    let _ = chars.next(); // skip '{'
                    sep = '$';
                    consume = i + 2; // next to '{'
                    break;
                }
                CharCode::Char('\\') => {
                    sbuf.push_char('\\').map_err(|_| {
                        self.ctx.throw_out_of_memory();
                        -1
                    })?;

                    match chars.peek() {
                        Some((_, CharCode::Char('\r' | '\n'))) => {
                            continue; // match \r or \n arm
                        }
                        Some(_) => {
                            let (_, c1) = chars.next().unwrap();
                            sbuf.push_char(c1).map_err(|_| {
                                self.ctx.throw_out_of_memory();
                                -1
                            })?;
                        }
                        None => {
                            self.throw_syntax_error("unexpected end of string");
                            return Err(-1);
                        }
                    }
                }
                CharCode::Char(c1 @ ('\r' | '\n')) => {
                    if c1 == '\r'
                        && let Some((_, CharCode::Char('\n'))) = chars.peek()
                    {
                        let _ = chars.next(); // skip \n after \r
                    }
                    sbuf.push_char('\n').map_err(|_| {
                        self.ctx.throw_out_of_memory();
                        -1
                    })?;
                    self.line_num += 1;
                }
                CharCode::Char(ch) => {
                    sbuf.push_char(ch).map_err(|_| {
                        self.ctx.throw_out_of_memory();
                        -1
                    })?;
                }
                CharCode::Code(_) => {
                    self.throw_syntax_error("invalid UTF-8 sequence");
                    return Err(-1);
                }
            }
        }

        debug_assert!(matches!(sep, '\0' | '`' | '$'));

        if unlikely(sep == '\0') {
            self.throw_syntax_error("unexpected end of string");
            Err(-1)
        } else {
            self.token.val = TokenKind::TEMPLATE;
            self.token.data = TokenData::Str {
                sep: sep as i32,
                str: sbuf,
            };
            self.buf_ptr = unsafe { buf.as_ptr().add(consume) };

            Ok(consume)
        }
    }

    pub(super) fn parse_template(&mut self, call: bool, argc: &mut i32) -> Result<TokenKind, i32> {
        let mut raw_array = JSValue::UNDEFINED;
        let mut template_object = JSValue::UNDEFINED;

        if call {
            // Create a template object: an array of cooked strings
            // Create an array of raw strings and store it to the raw property
            template_object = self.ctx.create_array()?.into();
            self.emit_push_const(template_object, false)?;

            raw_array = self.ctx.create_array()?.into();
            template_object.define_property_value(
                &self.ctx,
                Atom::CONST_raw,
                raw_array,
                JS_PROP_THROW,
            )?;
        }

        let mut depth = 0;

        while self.token.val == TokenKind::TEMPLATE {
            // the template content stream since token.ptr through input end, w/o starting quote.
            let mut tmpl = unsafe {
                let p = self.token.ptr.add(1); // skip '`'
                let bb = if p < self.input_end {
                    std::slice::from_raw_parts(p, self.input_end.offset_from(p) as usize)
                } else {
                    &[]
                };
                Utf8Stream::new(bb)
            };

            let mut cooked = Token {
                val: self.token.val,
                line_num: self.token.line_num,
                ptr: self.token.ptr,
                data: std::mem::replace(&mut self.token.data, TokenData::None),
            };
            let string_sep = cooked.data.string_sep().unwrap();

            if call {
                raw_array.define_property_value_u32(
                    &self.ctx,
                    depth as _,
                    self.ctx
                        .create_string_from_str(&cooked.data.string().unwrap().to_utf8())?
                        .into(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;

                // re-parse the template string with escape sequences but do not throw a
                // syntax error if it contains invalid sequences
                match self.parse_string(&mut tmpl, '`', Some(&mut cooked), false) {
                    Ok(_) => {
                        template_object.define_property_value_u32(
                            &self.ctx,
                            depth as _,
                            self.ctx
                                .create_string_from_str(&cooked.data.string().unwrap().to_utf8())?
                                .into(),
                            JS_PROP_ENUMERABLE | JS_PROP_THROW,
                        )?;
                    }
                    Err(_) => {
                        template_object.define_property_value_u32(
                            &self.ctx,
                            depth as _,
                            JSValue::UNDEFINED,
                            JS_PROP_ENUMERABLE | JS_PROP_THROW,
                        )?;
                    }
                }
            } else {
                // re-parse the template string with escape sequences and throw a
                // syntax error if it contains invalid sequences
                self.parse_string(&mut tmpl, '`', Some(&mut cooked), true)?;

                let sbuf = cooked.data.string().unwrap();
                if depth == 0 || !sbuf.is_empty() {
                    self.emit_push_const(
                        self.ctx.create_string_from_str(&sbuf.to_utf8())?.into(),
                        true,
                    )?;

                    if depth == 0 {
                        if string_sep == b'`' as i32 {
                            return self.next_token();
                        }
                        self.emit_op(OpCode::GET_FIELD2);
                        self.emit_atom(Atom::CONST_concat);
                    }
                    depth += 1;
                }
            }

            if string_sep == b'`' as i32 {
                // template is done
                if call {
                    seal_template_obj(&self.ctx, raw_array);
                    seal_template_obj(&self.ctx, template_object);
                    *argc = depth + 1;
                } else {
                    self.emit_op(OpCode::CALL_METHOD);
                    self.emit_u16(depth as u16 - 1);
                }
                return self.next_token();
            }

            // 处理模板表达式
            self.next_token()?;
            self.parse_expr()?;

            depth += 1;
            if self.token.val != TokenKind::Char('}') {
                self.throw_syntax_error("expected '}' after template expression");
                return Err(-1);
            }

            // XXX: should convert to string at this stage?
            self.clear_token_data();
            // Resume TEMPLATE parsing (s->token.line_num and  s->token.ptr are OK)
            self.got_lf = false;
            self.last_line_num = self.token.line_num;

            self.parse_template_part(unsafe {
                std::slice::from_raw_parts(
                    self.buf_ptr,
                    self.input_end.offset_from(self.buf_ptr) as usize,
                )
            })?;
        }

        self.parse_expect(TokenKind::TEMPLATE)
    }
}
