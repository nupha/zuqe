use smallvec::SmallVec;

use crate::{
    atom::{
        Atom, JSAtomRc,
        consts::{JS_ATOM_false, JS_ATOM_null, JS_ATOM_true},
    },
    context::JSContext,
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
    string::{CP_LS, CP_PS, UTF8_CHAR_LEN_MAX, unicode_from_utf8_v0},
    value::JSValue,
};

/// allow extended JSON
pub const JS_PARSE_JSON_EXT: i32 = 1;

fn json_parse_expect(s: &mut JSParseState, tok: i32) -> Result<(), i32> {
    if s.token.val != tok {
        s.throw_syntax_error(&format!("expecting '{tok}'"));
        Err(-1)
    } else {
        unsafe { json_next_token(s) }
    }
}

/// 'c' is the first character.
unsafe fn json_parse_ident(
    s: &mut JSParseState,
    pp: &mut *const u8,
    mut c: u8,
) -> Result<Atom, i32> {
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

        c = *p;
        if c >= 128 || !JSParseState::is_ident_next(c as _) {
            break;
        }
        p = p.add(1);
    }

    let atom = Atom::new(s.ctx.rt_mut(), std::str::from_utf8(&buf).map_err(|_| -1)?)?;
    *pp = p;
    Ok(atom)
}

unsafe fn json_parse_value(s: &mut JSParseState) -> JSResult {
    let mut val: JSValue = JSValue::NULL;

    match s.token.val {
        // '{'
        CHAR_CURLY_L => {
            json_next_token(s)?;

            val = s.ctx.create_base_object()?.into();
            if s.token.val != b'}' as i32 {
                loop {
                    let prop_name: JSAtomRc = if s.token.val == TOK_STRING {
                        Atom::new(
                            s.ctx.rt_mut(),
                            &s.token.data.string().unwrap().to_utf8(true),
                        )?
                    } else if s.ext_json && s.token.val == TOK_IDENT {
                        s.token.data.ident_atom_ref().unwrap().dup(s.ctx.rt())
                    } else {
                        s.throw_syntax_error("expecting property name");
                        return Err(-1);
                    }
                    .into_rc(s.ctx.rt());

                    json_next_token(s)?;
                    json_parse_expect(s, ':' as i32)?;

                    let prop_val = json_parse_value(s)?;
                    val.define_property_value(&s.ctx, prop_name.as_ref(), prop_val, JS_PROP_C_W_E)?;

                    if s.token.val != ',' as i32 {
                        break;
                    }
                    json_next_token(s)?;
                    if s.ext_json && s.token.val == b'}' as i32 {
                        break;
                    }
                }
            }
            json_parse_expect(s, b'}' as i32)?;
        }
        // '['
        CHAR_SQUARE_L => {
            json_next_token(s)?;

            let mut lst = Vec::<JSValue>::new();
            if s.token.val != ']' as i32 {
                'arr: loop {
                    let el = json_parse_value(s)?;

                    if lst.len() == lst.capacity() {
                        lst.try_reserve(32).map_err(|_| {
                            s.ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                    lst.push(el);

                    if s.token.val != ',' as i32 {
                        break 'arr;
                    }

                    json_next_token(s)?;

                    if s.ext_json && s.token.val == b']' as i32 {
                        break 'arr;
                    }
                }
            }

            // make Array
            lst.shrink_to_fit();
            val = s.ctx.create_array_with_values(lst)?.into();

            json_parse_expect(s, b']' as i32)?;
        }
        TOK_STRING => {
            val = s
                .ctx
                .create_string_with_content(s.token.data.string().unwrap().clone())?
                .into();
            json_next_token(s)?;
        }
        TOK_NUMBER => {
            val = s.token.data.number().unwrap().clone();
            json_next_token(s)?;
        }
        TOK_IDENT => {
            val = match s.token.data.ident_atom_ref().unwrap() {
                JS_ATOM_true => JSValue::Bool(true),
                JS_ATOM_false => JSValue::Bool(false),
                JS_ATOM_null => JSValue::NULL,
                _ => {
                    if s.token.val == TOK_EOF {
                        s.throw_syntax_error("unexpected end of input");
                    } else {
                        let t = std::slice::from_raw_parts(
                            s.token.ptr,
                            s.buf_ptr.offset_from(s.token.ptr) as usize,
                        );
                        s.throw_syntax_error(&format!(
                            "unexpected token: '{}'",
                            std::str::from_utf8_unchecked(t)
                        ));
                    }
                    return Err(-1);
                }
            };

            json_next_token(s)?;
        }
        _ => {
            if s.token.val == TOK_EOF {
                s.throw_syntax_error("unexpected end of input");
            } else {
                let t = std::slice::from_raw_parts(
                    s.token.ptr,
                    s.buf_ptr.offset_from(s.token.ptr) as usize,
                );
                s.throw_syntax_error(&format!(
                    "unexpected token: '{}'",
                    std::str::from_utf8_unchecked(t)
                ));
            }
            return Err(-1);
        }
    }

    Ok(val)
}

fn json_next_token(s: &mut JSParseState) -> Result<(), i32> {
    if js_check_stack_overflow(s.ctx.rt(), 0) {
        s.throw_syntax_error("stack overflow");
        return Err(-1);
    }

    s.clear_atom_in_token();

    enum Todo {
        DefaultToken(i32),
        Fail,
    }

    let mut p = s.buf_ptr;
    s.last_ptr = p;
    s.last_line_num = s.token.line_num;

    let todo = 'redo: loop {
        if p >= s.buf_end {
            s.token.val = TOK_EOF;
            s.token.data = TokenData::None;
            s.buf_ptr = s.buf_end;
            return Ok(());
        }

        s.token.line_num = s.line_num;
        s.token.ptr = p;

        match unsafe { *p } {
            b'\0' => {
                break Todo::DefaultToken(0);
            }
            c @ (b'\'' | b'"') => {
                if c == b'\'' && !s.ext_json {
                    // standard JSON does not accept single quoted strings
                    break Todo::DefaultToken('\'' as _);
                }

                if let Ok(r) = s.parse_string('"', true, unsafe { p.add(1) }, None) {
                    p = r;
                    s.buf_ptr = p;
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
                s.line_num += 1;
                continue 'redo;
            },
            b'\n' => unsafe {
                p = p.add(1);
                s.line_num += 1;
                continue 'redo;
            },
            //b'\f' | b'\v'
            c @ (0x0c | 0x0b) => {
                if !s.ext_json {
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
                if !s.ext_json {
                    // JSON does not accept comments
                    break Todo::DefaultToken('/' as _);
                }
                if *p.add(1) == b'*' {
                    // comment
                    p = p.add(2);
                    loop {
                        if p >= s.buf_end {
                            s.throw_syntax_error("unexpected end of comment");
                            break;
                        }

                        if *p == b'*' && *p.add(1) == b'/' {
                            p = p.add(2);
                            break;
                        }
                        if *p == b'\n' {
                            s.line_num += 1;
                            p = p.add(1);
                        } else if *p == b'\r' {
                            p = p.add(1);
                        } else if *p >= 0x80 {
                            if unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as _, &mut p).is_err() {
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
                        if p >= s.buf_end {
                            break;
                        }
                        if matches!(*p, b'\r' | b'\n') {
                            break;
                        }
                        if *p >= 0x80 {
                            if let Ok(c) = unicode_from_utf8_v0(p, UTF8_CHAR_LEN_MAX as _, &mut p) {
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
                if let Ok(atom) = json_parse_ident(s, &mut p, c) {
                    s.token.data = TokenData::Ident {
                        atom: atom.into_rc(s.ctx.rt()),
                        has_escape: false,
                        is_reserved: false,
                    };
                    s.token.val = TOK_IDENT;
                    s.buf_ptr = p;
                    return Ok(());
                } else {
                    break Todo::Fail;
                }
            },
            b'+' => unsafe {
                if !s.ext_json || !(*p.add(1)).is_ascii_digit() {
                    break Todo::DefaultToken('+' as _);
                }
                if let Ok(q) = json_parse_value_number(s, p) {
                    p = q;
                    s.buf_ptr = q;
                    return Ok(());
                } else {
                    break Todo::Fail;
                };
            },
            b'0' => unsafe {
                if (*p.add(1)).is_ascii_digit() {
                    break Todo::DefaultToken('0' as _);
                }
                if let Ok(q) = json_parse_value_number(s, p) {
                    p = q;
                    s.buf_ptr = q;
                    return Ok(());
                } else {
                    break Todo::Fail;
                };
            },
            b'-' => {
                if unsafe { !(*p.add(1)).is_ascii_digit() } {
                    break Todo::DefaultToken('-' as _);
                }
                if let Ok(q) = json_parse_value_number(s, p) {
                    p = q;
                    s.buf_ptr = q;
                    return Ok(());
                } else {
                    break Todo::Fail;
                };
            }
            b'1'..=b'9' => {
                if let Ok(q) = json_parse_value_number(s, p) {
                    p = q;
                    s.buf_ptr = q;
                    return Ok(());
                } else {
                    break Todo::Fail;
                };
            }
            c if c >= 128 => {
                s.throw_syntax_error("unexpected character");
                break Todo::Fail;
            }
            c => {
                break Todo::DefaultToken(c as _);
            }
        }
    };

    match todo {
        Todo::DefaultToken(c) => {
            s.token.val = c;
            p = unsafe { p.add(1) };

            s.buf_ptr = p;
            return Ok(());
        }
        Todo::Fail => {
            s.token.val = TOK_ERROR;
            return Err(-1);
        }
    }
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

pub fn JS_ParseJSON2(ctx: &JSContext, source: &[u8], filename: &str, flags: i32) -> JSResult {
    let mut s = JSParseState::new_for_json(ctx, source, filename);
    s.ext_json = flags & JS_PARSE_JSON_EXT != 0;

    unsafe {
        json_next_token(&mut s)?;
    }

    let val = unsafe { json_parse_value(&mut s)? };
    if s.token.val != TOK_EOF {
        s.throw_syntax_error("unexpected data at the end");
        Err(-1)
    } else {
        Ok(val)
    }
}

#[inline(always)]
pub fn JS_ParseJSON(ctx: &JSContext, source: &[u8], filename: &str) -> JSResult {
    JS_ParseJSON2(ctx, source, filename, 0)
}
