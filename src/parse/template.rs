use std::ops::DerefMut;

use crate::{
    array::JS_NewArray,
    atom::consts::{JS_ATOM_concat, JS_ATOM_raw},
    bytecode::OpCode,
    parse::emit::{emit_atom_dup, emit_op, emit_push_const, emit_u16},
    unicode::{unicode_from_utf8, UTF8_CHAR_LEN_MAX},
    value::{
        object::{seal_template_obj, JS_PROP_ENUMERABLE, JS_PROP_THROW},
        JSValue, JS_FreeValue, StringContent,
    },
};

use super::{error::js_parse_error, state::JSParseState, token::TOK_TEMPLATE};

impl<'s> JSParseState<'s> {
    pub unsafe fn parse_template_part(&mut self, mut p: *const u8) -> Result<(), i32> {
        macro_rules! template_error {
            ($msg:expr) => {
                js_parse_error(self, $msg);
                return Err(-1);
            };
        }

        let mut res_string = StringContent::Empty;

        let mut c: char;
        loop {
            if p >= self.buf_end {
                template_error!(c"unexpected end of string");
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
                        template_error!(c"unexpected end of string");
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
                        js_parse_error(self, c"invalid UTF-8 sequence");
                        -1
                    },
                )?;
                p = p_next;
            }

            if res_string.push_char(c).is_err() {
                (*self.ctx).JS_ThrowOutOfMemory();
                return Err(-1);
            }
        }

        self.token.val = TOK_TEMPLATE;
        self.token.u.str.sep = c as _;
        self.token.u.str.str = (*self.ctx).create_string_from_content(res_string);
        self.buf_ptr = p;

        Ok(())
    }

    pub unsafe fn parse_template(&mut self, call: bool, argc: *mut i32) -> Result<(), i32> {
        let mut ctx = self.ctx;
        let mut raw_array = JSValue::UNDEFINED;
        let mut template_object = JSValue::UNDEFINED;

        if call {
            // Create a template object: an array of cooked strings
            // Create an array of raw strings and store it to the raw property
            template_object = JS_NewArray(ctx.as_ptr()).check_exception()?;

            let ret = emit_push_const(self, template_object, false);
            JS_FreeValue(ctx.as_ptr(), template_object);
            if ret != 0 {
                return Err(-1);
            }

            raw_array = JS_NewArray(ctx.as_ptr()).check_exception()?;

            template_object.define_property_value(
                ctx.deref_mut(),
                JS_ATOM_raw,
                raw_array,
                JS_PROP_THROW,
            )?;
        }

        let mut depth = 0;
        while self.token.val == TOK_TEMPLATE {
            let mut p = self.token.ptr.offset(1);
            let mut cooked = self.token;

            if call {
                raw_array.define_property_value_u32(
                    ctx.deref_mut(),
                    depth as _,
                    self.token.u.str.str.dup(),
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;

                // re-parse the string with escape sequences but do not throw a
                // syntax error if it contains invalid sequences
                if self
                    .parse_string('`', false, p, Some(&mut cooked), &mut p)
                    .is_err()
                {
                    cooked.u.str.str = JSValue::UNDEFINED;
                }

                template_object.define_property_value_u32(
                    ctx.deref_mut(),
                    depth as _,
                    cooked.u.str.str,
                    JS_PROP_ENUMERABLE | JS_PROP_THROW,
                )?;
            } else {
                // re-parse the string with escape sequences and throw a
                // syntax error if it contains invalid sequences
                JS_FreeValue(ctx.as_ptr(), self.token.u.str.str);
                self.token.u.str.str = JSValue::UNDEFINED;

                self.parse_string('`', true, p, Some(&mut cooked), &mut p)?;
                let str = cooked.u.str.str.string();
                if depth == 0 || !str.is_empty() {
                    let ret = emit_push_const(self, cooked.u.str.str, true);
                    JS_FreeValue(self.ctx.as_ptr(), cooked.u.str.str);
                    if ret != 0 {
                        return Err(-1);
                    }

                    if depth == 0 {
                        if self.token.u.str.sep == b'`' as i32 {
                            return self.next_token();
                        }
                        emit_op(self, OpCode::GET_FIELD2);
                        emit_atom_dup(self, JS_ATOM_concat);
                    }
                    depth += 1;
                } else {
                    JS_FreeValue(self.ctx.as_ptr(), cooked.u.str.str);
                }
            }

            if self.token.u.str.sep == b'`' as i32 {
                // template is done
                if call {
                    seal_template_obj(ctx.as_ptr(), raw_array);
                    seal_template_obj(ctx.as_ptr(), template_object);
                    *argc = depth + 1;
                } else {
                    emit_op(self, OpCode::CALL_METHOD);
                    emit_u16(self, depth as u16 - 1);
                }
                return self.next_token();
            }

            // 处理模板表达式
            self.next_token()?;
            self.parse_expr()?;

            depth += 1;
            if self.token.val != b'}' as i32 {
                js_parse_error(self, c"expected '}' after template expression");
                return Err(-1);
            }

            // XXX: should convert to string at this stage?
            self.free_token();
            // Resume TOK_TEMPLATE parsing (s->token.line_num and  s->token.ptr are OK)
            self.got_lf = false;
            self.last_line_num = self.token.line_num;

            self.parse_template_part(self.buf_ptr)?
        }

        self.parse_expect(TOK_TEMPLATE)
    }
}
