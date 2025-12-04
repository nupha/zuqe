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
    string::{CP_LS, CP_PS, UTF8_CHAR_LEN_MAX, escape_str, unicode_from_utf8_v0},
    value::JSValue,
};

/// allow extended JSON
pub const JS_PARSE_JSON_EXT: i32 = 1;

impl<'s> JSParseState<'s> {
    fn json_next_token(&mut self) -> Result<(), i32> {
        if js_check_stack_overflow(&self.ctx.rt(), 0) {
            self.throw_syntax_error("stack overflow");
            return Err(-1);
        }

        self.clear_token_data();

        enum Todo {
            DefaultToken(i32),
            Fail,
        }

        let mut p = self.buf_ptr;
        self.last_ptr = p;
        self.last_line_num = self.token.line_num;

        let todo = 'redo: loop {
            if p >= self.input_end {
                self.token.val = TOK_EOF;
                self.token.data = TokenData::None;
                self.buf_ptr = self.input_end;
                return Ok(());
            }

            self.token.line_num = self.line_num;
            self.token.ptr = p;

            match unsafe { *p } {
                b'\0' => {
                    break Todo::DefaultToken(0);
                }
                c @ (b'\'' | b'"') => {
                    if c == b'\'' && !self.ext_json {
                        // standard JSON does not accept single quoted strings
                        break Todo::DefaultToken('\'' as _);
                    }

                    if let Ok(r) = self.parse_string('"', true, unsafe { p.add(1) }, None) {
                        p = r;
                        self.buf_ptr = p;
                        return Ok(());
                    } else {
                        break Todo::Fail;
                    }
                }
                b'\r' => unsafe {
                    // accept DOS and MAC newline sequences
                    if *p.add(1) == b'\n' {
                        p = p.add(1);
                    }
                    p = p.add(1);
                    self.line_num += 1;
                    continue 'redo;
                },
                b'\n' => unsafe {
                    p = p.add(1);
                    self.line_num += 1;
                    continue 'redo;
                },
                //b'\f' | b'\v'
                c @ (0x0c | 0x0b) => {
                    if !self.ext_json {
                        // JSONWhitespace does not match <VT>, nor <FF>
                        break Todo::DefaultToken(c as _);
                    }
                    p = unsafe { p.add(1) };
                    continue 'redo;
                }
                b' ' | b'\t' => {
                    p = unsafe { p.add(1) };
                    continue 'redo;
                }
                b'/' => unsafe {
                    if !self.ext_json {
                        // JSON does not accept comments
                        break Todo::DefaultToken('/' as _);
                    }
                    if *p.add(1) == b'*' {
                        // comment
                        p = p.add(2);
                        loop {
                            if p >= self.input_end {
                                self.throw_syntax_error("unexpected end of comment");
                                break;
                            }

                            if *p == b'*' && *p.add(1) == b'/' {
                                p = p.add(2);
                                break;
                            }
                            if *p == b'\n' {
                                self.line_num += 1;
                                p = p.add(1);
                            } else if *p == b'\r' {
                                p = p.add(1);
                            } else if *p >= 0x80 {
                                if unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as _, &mut p).is_err()
                                {
                                    p = p.add(1); // skip invalid UTF-8
                                }
                            } else {
                                p = p.add(1);
                            }
                        }
                        continue 'redo;
                    } else if *p.offset(1) == b'/' {
                        // line comment
                        p = p.offset(2);
                        loop {
                            if p >= self.input_end {
                                break;
                            }
                            if matches!(*p, b'\r' | b'\n') {
                                break;
                            }
                            if *p >= 0x80 {
                                if let Ok(c) =
                                    unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as _, &mut p)
                                {
                                    // LS or PS are considered as line terminator
                                    if matches!(c as u32, CP_LS | CP_PS) {
                                        break;
                                    }
                                } else {
                                    p = p.offset(1); // skip invalid UTF-8
                                }
                            } else {
                                p = p.offset(1);
                            }
                        }
                        continue 'redo;
                    } else {
                        break Todo::DefaultToken('/' as _);
                    }
                },
                c @ (b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$') => unsafe {
                    // identifier : only pure ascii characters are accepted
                    p = p.add(1);
                    if let Ok(atom) = json_parse_ident(self, &mut p, c) {
                        self.token.data = TokenData::Ident {
                            atom,
                            has_escape: false,
                            is_reserved: false,
                        };
                        self.token.val = TOK_IDENT;
                        self.buf_ptr = p;
                        return Ok(());
                    } else {
                        break Todo::Fail;
                    }
                },
                b'+' => unsafe {
                    if !self.ext_json || !(*p.add(1)).is_ascii_digit() {
                        break Todo::DefaultToken('+' as _);
                    }
                    if let Ok(q) = json_parse_value_number(self, p) {
                        p = q;
                        self.buf_ptr = q;
                        return Ok(());
                    } else {
                        break Todo::Fail;
                    };
                },
                b'0' => unsafe {
                    if (*p.add(1)).is_ascii_digit() {
                        break Todo::DefaultToken('0' as _);
                    }
                    if let Ok(q) = json_parse_value_number(self, p) {
                        p = q;
                        self.buf_ptr = q;
                        return Ok(());
                    } else {
                        break Todo::Fail;
                    };
                },
                b'-' => {
                    if unsafe { !(*p.add(1)).is_ascii_digit() } {
                        break Todo::DefaultToken('-' as _);
                    }
                    if let Ok(q) = json_parse_value_number(self, p) {
                        p = q;
                        self.buf_ptr = q;
                        return Ok(());
                    } else {
                        break Todo::Fail;
                    };
                }
                b'1'..=b'9' => {
                    if let Ok(q) = json_parse_value_number(self, p) {
                        p = q;
                        self.buf_ptr = q;
                        return Ok(());
                    } else {
                        break Todo::Fail;
                    };
                }
                c if c >= 128 => {
                    self.throw_syntax_error("unexpected character");
                    break Todo::Fail;
                }
                c => {
                    break Todo::DefaultToken(c as _);
                }
            }
        };

        match todo {
            Todo::DefaultToken(c) => {
                self.token.val = c;
                p = unsafe { p.add(1) };
                self.buf_ptr = p;
                Ok(())
            }
            Todo::Fail => {
                self.token.val = TOK_ERROR;
                Err(-1)
            }
        }
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
                                self.ctx.rt(),
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

/// 'c' is the first character.
fn json_parse_ident(s: &mut JSParseState, pp: &mut *const u8, mut c: u8) -> Result<Atom, i32> {
    let mut p = *pp;
    let mut buf = SmallVec::<[u8; 128]>::new();

    loop {
        if buf.capacity() - buf.len() >= UTF8_CHAR_LEN_MAX {
            buf.try_reserve(128).map_err(|_| {
                s.ctx.throw_out_of_memory();
                -1
            })?;
        }
        buf.push(c);

        unsafe {
            c = *p;
            if c >= 128 || !JSParseState::is_ident_next(c as _) {
                break;
            }
            p = p.add(1);
        }
    }

    let atom = Atom::new(s.ctx.rt(), std::str::from_utf8(&buf).map_err(|_| -1)?)?;
    *pp = p;
    Ok(atom)
}

// Helper function to encapsulate number parsing logic
// Returns new pointer on success, or null pointer on failure (to signal goto fail)
fn json_parse_value_number(s: &mut JSParseState, mut p: *const u8) -> Result<*const u8, i32> {
    let flags: i32;
    let radix: i32;

    if !s.ext_json {
        flags = 0;
        radix = 10;
    } else {
        flags = ATOD_ACCEPT_BIN_OCT;
        radix = 0;
    }

    if let Ok(num) = unsafe { js_atof(&s.ctx, p, &mut p, radix, flags) } {
        s.token.val = TOK_NUMBER;
        s.token.data = TokenData::Num(num);
        Ok(p) // return updated pointer  
    } else {
        s.token.val = TOK_ERROR;
        Err(-1)
    }
}

pub fn JS_ParseJSON(ctx: &ContextImpl, source: &[u8], filename: &str) -> JSResult {
    // Note: parser will be dropped within this function.
    // so, 'source' and 'filename' are safe w/o lifetime annotation

    let mut parser = JSParseState::new_for_json(ctx, source, filename);
    parser.ext_json = JS_PARSE_JSON_EXT != 0;

    parser.json_next_token()?;

    let val = parser.json_parse_value()?;
    if parser.token.val != TOK_EOF {
        parser.throw_syntax_error("unexpected data at the end");
        Err(-1)
    } else {
        Ok(val)
    }
}
