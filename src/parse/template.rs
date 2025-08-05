use std::rc::Rc;

use crate::{
    atom::consts::{JS_ATOM_concat, JS_ATOM_raw},
    bytecode::OpCode,
    object::{JS_PROP_ENUMERABLE, JS_PROP_THROW, seal_template_obj},
    parse::token::TokenData,
    string::StringBuf,
    unicode::{UTF8_CHAR_LEN_MAX, unicode_from_utf8},
    value::JSValue,
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

        let mut res_string = StringBuf::Empty;

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
                        self.ctx.throw_out_of_memory();
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
                self.ctx.throw_out_of_memory();
                return Err(-1);
            }
        }

        self.token.val = TOK_TEMPLATE;
        self.token.data = TokenData::Str {
            sep: c as _,
            str: Rc::new(res_string),
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

            self.emit_push_const(template_object.clone(), false)?;

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
            let mut p = unsafe { self.token.ptr.add(1) };
            let mut cooked = self.token.clone();

            if call {
                let txt = self
                    .ctx
                    .create_string_from_content_rc(self.token.data.string().unwrap().clone())?;

                raw_array.define_property_value_u32(
                    &self.ctx,
                    depth as _,
                    txt.into(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;

                // re-parse the string with escape sequences but do not throw a
                // syntax error if it contains invalid sequences
                match self.parse_string('`', false, p, Some(&mut cooked)) {
                    Ok(p2) => {
                        p = p2;
                    }
                    Err(_) => {
                        // TODO: this is not nessecary, because ::drop() will handle string value free
                        // if let TokenData::Str { str, .. } = &mut cooked.data {
                        //     *str = JSValue::UNDEFINED;
                        // }
                    }
                }

                let txt = self
                    .ctx
                    .create_string_from_content_rc(cooked.data.string().unwrap().clone())?;

                template_object.define_property_value_u32(
                    &self.ctx,
                    depth as _,
                    txt.into(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;
            } else {
                // re-parse the string with escape sequences and throw a
                // syntax error if it contains invalid sequences

                // TODO: this is not nessecary, because ::drop() will handle string value free
                // if let TokenData::Str { str, .. } = &mut cooked.data {
                //     *str = JSValue::UNDEFINED;
                // }

                p = self.parse_string('`', true, p, Some(&mut cooked))?;
                let str = cooked.data.string().unwrap();
                if depth == 0 || !str.is_empty() {
                    self.emit_push_const(
                        self.ctx.create_string_from_content_rc(str.clone())?.into(),
                        true,
                    )?;

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
            self.parse_expr()?;

            depth += 1;
            if self.token.val != b'}' as i32 {
                self.throw_syntax_error("expected '}' after template expression");
                return Err(-1);
            }

            // XXX: should convert to string at this stage?
            self.clear_atom_in_token();
            // Resume TOK_TEMPLATE parsing (s->token.line_num and  s->token.ptr are OK)
            self.got_lf = false;
            self.last_line_num = self.token.line_num;

            unsafe { self.parse_template_part(self.buf_ptr)? }
        }

        self.parse_expect(TOK_TEMPLATE)
    }
}
