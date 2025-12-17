use crate::{
    atom::Atom,
    bytecode::OpCode,
    object::{JS_PROP_ENUMERABLE, JS_PROP_THROW, seal_template_obj},
    parse::token::{Token, TokenData},
    string::{StringBuf, UTF8_CHAR_LEN_MAX, unicode_from_utf8_v0},
    value::JSValue,
};

use super::{state::JSParseState, token::TOK_TEMPLATE};

impl<'s> JSParseState<'s> {
    pub(super) fn parse_template_part(&mut self, mut p: *const u8) -> Result<(), i32> {
        let mut sbuf = StringBuf::Empty;

        let sep = loop {
            if p >= self.input_end {
                self.throw_syntax_error("unexpected end of string");
                return Err(-1);
            }

            let mut ch = unsafe { *p as char };
            p = unsafe { p.offset(1) };

            match ch {
                '`' => {
                    break ch; // ` template end part
                }
                '$' if unsafe { *p } == b'{' => {
                    // ${ template start or middle part
                    p = unsafe { p.offset(1) };
                    break ch;
                }
                _ => {}
            }

            if ch == '\\' {
                sbuf.push_char(ch).map_err(|_| {
                    self.ctx.throw_out_of_memory();
                    -1
                })?;
                if p >= self.input_end {
                    self.throw_syntax_error("unexpected end of string");
                    return Err(-1);
                } else {
                    unsafe {
                        ch = *p as char;
                        p = p.offset(1);
                    }
                }
            }

            if ch == '\r' {
                // newline sequences are normalized as single '\n'
                if unsafe { *p } == b'\n' {
                    p = unsafe { p.offset(1) };
                }
                ch = '\n';
            }

            if ch == '\n' {
                self.line_num += 1;
            } else if !ch.is_ascii() {
                let mut p_next = std::ptr::null::<u8>();
                // TODO: use str::from_utf8() instead
                ch = unicode_from_utf8_v0(
                    unsafe { p.offset(-1) },
                    UTF8_CHAR_LEN_MAX as _,
                    &mut p_next,
                )
                .map_err(|_| {
                    self.throw_syntax_error("invalid UTF-8 sequence");
                    -1
                })?;
                p = p_next;
            }

            // append char to buf
            sbuf.push_char(ch).map_err(|_| {
                self.ctx.throw_out_of_memory();
                -1
            })?;
        };

        self.token.val = TOK_TEMPLATE;
        self.token.data = TokenData::Str {
            sep: sep as i32, // could be '`' or '$'
            str: sbuf,
        };
        self.buf_ptr = p;

        Ok(())
    }

    pub(super) fn parse_template(&mut self, call: bool, argc: &mut i32) -> Result<i32, i32> {
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
        while self.token.val == TOK_TEMPLATE {
            let p = unsafe { self.token.ptr.add(1) };

            let mut cooked = Token {
                val: self.token.val,
                line_num: self.token.line_num,
                ptr: self.token.ptr,
                data: std::mem::replace(&mut self.token.data, TokenData::None),
            };
            let string_sep = cooked.data.string_sep().unwrap();

            if call {
                let txt = self
                    .ctx
                    .create_string_from_str(&cooked.data.string().unwrap().to_utf8())?;
                raw_array.define_property_value_u32(
                    &self.ctx,
                    depth as _,
                    txt.into(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;

                // re-parse the string with escape sequences but do not throw a
                // syntax error if it contains invalid sequences
                match self.parse_string(
                    '`',
                    false,
                    unsafe {
                        std::slice::from_raw_parts(p, self.input_end.offset_from(p) as usize)
                    },
                    Some(&mut cooked),
                ) {
                    Ok(_) => {
                        let txt = self
                            .ctx
                            .create_string_from_str(&cooked.data.string().unwrap().to_utf8())?;
                        template_object.define_property_value_u32(
                            &self.ctx,
                            depth as _,
                            txt.into(),
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
                // re-parse the string with escape sequences and throw a
                // syntax error if it contains invalid sequences
                self.parse_string(
                    '`',
                    true,
                    unsafe {
                        std::slice::from_raw_parts(p, self.input_end.offset_from(p) as usize)
                    },
                    Some(&mut cooked),
                )?;

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
            if self.token.val != b'}' as i32 {
                self.throw_syntax_error("expected '}' after template expression");
                return Err(-1);
            }

            // XXX: should convert to string at this stage?
            self.clear_token_data();
            // Resume TOK_TEMPLATE parsing (s->token.line_num and  s->token.ptr are OK)
            self.got_lf = false;
            self.last_line_num = self.token.line_num;

            self.parse_template_part(self.buf_ptr)?
        }

        self.parse_expect(TOK_TEMPLATE)
    }
}
