use crate::{
    builtins::number::{js_parseFloat, js_parseInt},
    error::JSResult,
    eval::{EvalFlags, EvalType},
    function::{CallContext, FieldDef},
    string::{CharCode, StringBuf},
    utils::unlikely,
    value::JSValue,
};

pub fn js_global_eval(mut cc: CallContext) -> JSResult {
    let global = cc.ctx().global_obj();
    let val = *cc.arg(0);
    val.eval_object(
        cc.ctx_mut(),
        global.into(),
        EvalType::Indirect,
        EvalFlags::empty(),
        -1,
    )
}

pub const MAGIC_NUM_IS_NAN: i32 = 1;
pub const MAGIC_NUM_IS_FINITE: i32 = 2;

pub(crate) fn js_global_isNaN_isFinite(cc: CallContext) -> JSResult {
    let d = cc.arg(0).to_f64(cc.ctx())?;
    let b = match cc.magic() {
        MAGIC_NUM_IS_NAN => f64::is_nan(d),
        MAGIC_NUM_IS_FINITE => f64::is_finite(d),
        _ => unreachable!(),
    };
    Ok(JSValue::Bool(b))
}

#[inline(always)]
const fn is_uri_reserved(c: u32) -> bool {
    matches!(
        c as u8,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b',' | b'#'
    )
}

fn js_global_decodeURI(cc: CallContext) -> JSResult {
    let is_component = cc.magic() == 1;
    let ctx = cc.ctx();

    let str = cc.arg(0).to_string(ctx)?;
    let len = str.len();

    let mut buf = StringBuf::with_capacity(len, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    let mut char_it = str.content().chars();
    while let Some((_, cc)) = char_it.next() {
        match cc {
            CharCode::Char('%') => {
                // first %XX byte
                let mut d = 0u32;
                let mut hh = [0u8; 2];
                for x in hh.iter_mut() {
                    let (a, n) = match char_it.next().and_then(|(_, x)| x.char()) {
                        Some(a @ '0'..='9') => (a, a as u32 - '0' as u32),
                        Some(a @ 'A'..='F') => (a, a as u32 - 'A' as u32 + 10),
                        Some(a @ 'a'..='f') => (a, a as u32 - 'a' as u32 + 10),
                        _ => {
                            ctx.throw_uri_error("expecting hex digit");
                            return Err(-1);
                        }
                    };
                    d = d << 4 | n;
                    *x = a as u8;
                }
                debug_assert!(d <= 0xff);

                if d < 0x80 {
                    // ascii
                    if !is_component && is_uri_reserved(d as _) {
                        // for non-component, reserved chars are kept in %XX format.
                        buf.push_u8_slice([b'%', hh[0], hh[1]].as_slice())
                            .map_err(|_| {
                                ctx.throw_out_of_memory();
                                -1
                            })?;
                    } else {
                        buf.push_u8(d as u8).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                } else {
                    // decode utf8 sequence with following bytes
                    let (reminder, c_min, mut uc) = if (0xc0..=0xdf).contains(&d) {
                        (1, 0x80, d & 0x1f)
                    } else if (0xe0..=0xef).contains(&d) {
                        (2, 0x800, d & 0xf)
                    } else if (0xf0..=0xf7).contains(&d) {
                        (3, 0x10000, d & 0x7)
                    } else {
                        ctx.throw_uri_error("malformed UTF-8");
                        return Err(-1);
                    };

                    for _ in 0..reminder {
                        if let Some((_, CharCode::Char('%'))) = char_it.next() {
                            // hex %XX
                            let mut d = 0u8;
                            for _ in 0..2 {
                                let n = match char_it.next().and_then(|(_, x)| x.char()) {
                                    Some(a @ '0'..='9') => a as u8 - b'0',
                                    Some(a @ 'A'..='F') => a as u8 - b'A' + 10,
                                    Some(a @ 'a'..='f') => a as u8 - b'a' + 10,
                                    _ => {
                                        ctx.throw_uri_error("expecting hex digit");
                                        return Err(-1);
                                    }
                                };
                                d = d << 4 | n;
                            }

                            if d & 0xc0 != 0x80 {
                                ctx.throw_uri_error("malformed UTF-8");
                                return Err(-1);
                            }
                            uc = (uc << 6) | (d as u32 & 0x3f);
                        } else {
                            // expect '%'
                            ctx.throw_uri_error("malformed UTF-8");
                            return Err(-1);
                        }
                    }

                    if unlikely(uc < c_min || uc > 0x10FFFF || (0xd800..0xe000).contains(&uc)) {
                        ctx.throw_uri_error("malformed UTF-8");
                        return Err(-1);
                    } else {
                        buf.push_u32(uc).map_err(|_| {
                            ctx.throw_out_of_memory();
                            -1
                        })?;
                    }
                }
            }
            CharCode::Char(ch) => {
                buf.push_char(ch).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            CharCode::Code(_) => {
                buf.push_u16_slice(cc.encode_utf16(&mut [0; 2]))
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
            }
        }
    }

    ctx.create_string_with_buf(buf).map(|s| s.into())
}

pub const js_global_props: &[FieldDef] = &[
    FieldDef::CALL("parseInt", 2, js_parseInt, 0),
    FieldDef::CALL("parseFloat", 1, js_parseFloat, 0),
    FieldDef::CALL("isNaN", 1, js_global_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    FieldDef::CALL(
        "isFinite",
        1,
        js_global_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _,
    ),
    FieldDef::CALL("decodeURI", 1, js_global_decodeURI, 0),
    FieldDef::CALL("decodeURIComponent", 1, js_global_decodeURI, 1),
    // JS_CFUNC_MAGIC_DEF("encodeURI", 1, js_global_encodeURI, 0),
    // JS_CFUNC_MAGIC_DEF("encodeURIComponent", 1, js_global_encodeURI, 1),
    // JS_CFUNC_DEF("escape", 1, js_global_escape),
    // JS_CFUNC_DEF("unescape", 1, js_global_unescape),
    FieldDef::FLOAT64("Infinity", f64::INFINITY, 0),
    FieldDef::FLOAT64("NaN", f64::NAN, 0),
    FieldDef::UNDEFINED("undefined", 0),
];
