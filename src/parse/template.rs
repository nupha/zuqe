use crate::{
    atom::consts::{JS_ATOM_concat, JS_ATOM_raw},
    bytecode::OpCode,
    object::{JS_PROP_ENUMERABLE, JS_PROP_THROW, seal_template_obj},
    parse::token::TokenData,
    unicode::{UTF8_CHAR_LEN_MAX, unicode_from_utf8},
    value::{JSValue, StringContent},
};

use super::{state::JSParseState, token::TOK_TEMPLATE};

impl<'s> JSParseState<'s> {
    pub unsafe fn parse_template_part(&mut self, mut p: *const u8) -> Result<(), i32> {
        macro_rules! template_error {
            ($msg:expr) => {
                self.throw_syntax_error($msg);
                return Err(-1);
            };
        }

        let mut res_string = StringContent::Empty;

        let mut c: char;
        loop {
            if p >= self.buf_end {
                template_error!("unexpected end of string");
            }

            c = *p as char;
            p = p.offset(1);

            match c {
                '`' => {
                    break; // ` template end part
                }
                '$' if *p == b'{' => {
                    // ${ template start or middle part
                    p = p.offset(1);
                    break;
                }
                '\\' => {
                    res_string.push_char(c).map_err(|_| {
                        self.ctx.JS_ThrowOutOfMemory();
                        -1
                    })?;
                    if p >= self.buf_end {
                        template_error!("unexpected end of string");
                    } else {
                        c = *p as char;
                        p = p.offset(1);
                    }
                }
                '\r' => {
                    // newline sequences are normalized as single '\n'
                    if *p == b'\n' {
                        p = p.offset(1);
                    }
                    c = '\n';
                }
                _ => {}
            }

            if c == '\n' {
                self.line_num += 1;
            } else if c.is_ascii() {
                let mut p_next = std::ptr::null::<u8>();
                c = unicode_from_utf8(p.offset(-1), UTF8_CHAR_LEN_MAX as _, &mut p_next).map_err(
                    |_| {
                        self.throw_syntax_error("invalid UTF-8 sequence");
                        -1
                    },
                )?;
                p = p_next;
            }

            if res_string.push_char(c).is_err() {
                self.ctx.JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.token.val = TOK_TEMPLATE;
        self.token.data = TokenData::Str {
            sep: c as _,
            str: self.ctx.create_string_from_content(res_string),
        };
        self.buf_ptr = p;

        Ok(())
    }

    pub fn parse_template(&mut self, call: bool, argc: *mut i32) -> Result<(), i32> {
        let mut raw_array = JSValue::UNDEFINED;
        let mut template_object = JSValue::UNDEFINED;

        if call {
            // Create a template object: an array of cooked strings
            // Create an array of raw strings and store it to the raw property
            template_object = self.ctx.create_array()?;

            let ret = self.emit_push_const(template_object.clone(), false);
            if ret != 0 {
                return Err(-1);
            }

            raw_array = self.ctx.create_array()?;

            template_object.define_property_value(
                &self.ctx,
                JS_ATOM_raw,
                raw_array.clone(),
                JS_PROP_THROW,
            )?;
        }

        let mut depth = 0;
        while self.token.val == TOK_TEMPLATE {
            let mut p = unsafe { self.token.ptr.offset(1) };
            let mut cooked = self.token.clone();

            if call {
                raw_array.define_property_value_u32(
                    &self.ctx,
                    depth as _,
                    self.token.data.string().unwrap().clone(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;

                // re-parse the string with escape sequences but do not throw a
                // syntax error if it contains invalid sequences
                if self
                    .parse_string('`', false, p, Some(&mut cooked), &mut p)
                    .is_err()
                {
                    // TODO: this is not nessecary, because ::drop() will handle string value free
                    if let TokenData::Str { str, .. } = &mut cooked.data {
                        *str = JSValue::UNDEFINED;
                    }
                }

                template_object.define_property_value_u32(
                    &self.ctx,
                    depth as _,
                    cooked.data.string().unwrap().clone(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;
            } else {
                // re-parse the string with escape sequences and throw a
                // syntax error if it contains invalid sequences

                // TODO: this is not nessecary, because ::drop() will handle string value free
                if let TokenData::Str { str, .. } = &mut cooked.data {
                    *str = JSValue::UNDEFINED;
                }

                self.parse_string('`', true, p, Some(&mut cooked), &mut p)?;
                let str = unsafe { cooked.data.string().unwrap().string_unchecked() };
                if depth == 0 || !str.is_empty() {
                    let ret = self.emit_push_const(JSValue::String(str.clone()), true);
                    // emit_push_const(self, cooked.u.str.str.replace(JSValue::UNDEFINED), true);

                    if ret != 0 {
                        return Err(-1);
                    }

                    if depth == 0 {
                        if self.token.data.string_sep().unwrap() == b'`' as i32 {
                            return self.next_token();
                        }
                        self.emit_op(OpCode::GET_FIELD2);
                        self.emit_atom_dup(JS_ATOM_concat);
                    }
                    depth += 1;
                } else {
                    // cooked.u.str.str = JSValue::UNDEFINED;
                }
            }

            if self.token.data.string_sep().unwrap() == b'`' as i32 {
                // template is done
                if call {
                    seal_template_obj(&self.ctx, raw_array);
                    seal_template_obj(&self.ctx, template_object);
                    unsafe {
                        *argc = depth + 1;
                    }
                } else {
                    self.emit_op(OpCode::CALL_METHOD);
                    self.emit_u16(depth as u16 - 1);
                }
                return self.next_token();
            }

            // 处理模板表达式
            self.next_token()?;
            unsafe {
                self.parse_expr()?;
            }

            depth += 1;
            if self.token.val != b'}' as i32 {
                self.throw_syntax_error("expected '}' after template expression");
                return Err(-1);
            }

            // XXX: should convert to string at this stage?
            self.free_token();
            // Resume TOK_TEMPLATE parsing (s->token.line_num and  s->token.ptr are OK)
            self.got_lf = false;
            self.last_line_num = self.token.line_num;

            unsafe { self.parse_template_part(self.buf_ptr)? }
        }

        self.parse_expect(TOK_TEMPLATE)
    }
}
