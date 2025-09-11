use crate::{
    JS_CFUNC_NATIVE_DEF, JS_PROP_DOUBLE_DEF, JS_PROP_UNDEFINED_DEF,
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_INDIRECT,
    function::{CallContext, FieldDef},
    number::{js_parseFloat, js_parseInt},
    string::StringBuf,
    value::JSValue,
};

pub fn js_global_eval(ctx: &JSContext, cc: CallContext) -> JSResult {
    // to avoid ctx borrow conflict, fetch global object in advance
    let global = JSValue::Object(ctx.global_obj());
    cc.arg(0)
        .eval_object(ctx, global, JS_EVAL_TYPE_INDIRECT, -1)
        .err()
}

pub const MAGIC_NUM_IS_NAN: i32 = 1;
pub const MAGIC_NUM_IS_FINITE: i32 = 2;

pub fn js_global_isNaN_isFinite(ctx: &JSContext, cc: CallContext) -> JSResult {
    let d = cc.arg(0).to_f64(ctx)?;
    let b = match cc.magic() {
        MAGIC_NUM_IS_NAN => f64::is_nan(d),
        MAGIC_NUM_IS_FINITE => f64::is_finite(d),
        _ => unreachable!(),
    };
    Ok(JSValue::Bool(b))
}

#[inline(always)]
fn is_uri_reserved(c: u32) -> bool {
    matches!(
        c as u8,
        b';' | b'/' | b'?' | b':' | b'@' | b'&' | b'=' | b'+' | b'$' | b','
    )
}

fn js_global_decodeURI(ctx: &JSContext, cc: CallContext) -> JSResult {
    let is_component = cc.magic() == 1;

    let str = cc.arg(0).to_string(ctx)?;
    let len = str.len();

    let mut buf = StringBuf::with_capacity(len, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    let mut code_it = str.content().iter_codepoint();

    while let Some(cp16) = code_it.next() {
        if cp16 == '%' as u16 {
            let cp = cp16 as u32;

            // %XX
            let mut d = 0u32;
            let mut xx = [0u8; 2];
            for i in 0..2 {
                let (a, n) = match code_it.next().and_then(|x| char::from_u32(x as u32)) {
                    Some(a @ '0'..='9') => (a, a as u32 - '0' as u32),
                    Some(a @ 'A'..='F') => (a, a as u32 - 'A' as u32 + 10),
                    Some(a @ 'a'..='f') => (a, a as u32 - 'a' as u32 + 10),
                    _ => {
                        ctx.throw_type_error("expecting hex digit");
                        return Err(-1);
                    }
                };
                d = d << 4 | n;
                xx[i] = a as u8;
            }

            if !is_component && is_uri_reserved(cp as _) {
                // for non-component, reserved chars are kept in %XX format.
                buf.push_u8_slice([b'%', xx[0], xx[1]].as_slice())
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
            } else {
                //
                // decode utf8 sequence
                //
                let (reminder, c_min, mut uc) = if cp >= 0xc0 && cp <= 0xdf {
                    (1, 0x80, cp & 0x1f)
                } else if cp >= 0xe0 && cp <= 0xef {
                    (2, 0x800, cp & 0xf)
                } else if cp >= 0xf0 && cp <= 0xf7 {
                    (3, 0x10000, cp & 0x7)
                } else {
                    ctx.throw_type_error("malformed UTF-8");
                    return Err(-1);
                };

                for _ in 0..reminder {
                    // expect '%'
                    if !code_it.next().map_or(false, |x| x == '%' as u16) {
                        ctx.throw_type_error("malformed UTF-8");
                        return Err(-1);
                    }

                    // hex XX
                    let mut d = 0u8;
                    for _ in 0..2 {
                        let n = match code_it.next().and_then(|x| char::from_u32(x as u32)) {
                            Some(a @ '0'..='9') => a as u8 - '0' as u8,
                            Some(a @ 'A'..='F') => a as u8 - 'A' as u8 + 10,
                            Some(a @ 'a'..='f') => a as u8 - 'a' as u8 + 10,
                            _ => {
                                ctx.throw_type_error("expecting hex digit");
                                return Err(-1);
                            }
                        };
                        d = d << 4 | n;
                    }

                    if d & 0xc0 != 0x80 {
                        ctx.throw_type_error("malformed UTF-8");
                        return Err(-1);
                    }
                    uc = (uc << 6) | (d as u32 & 0x3f);
                }

                if uc < c_min || uc > 0x10FFFF || (uc >= 0xd800 && uc < 0xe000) {
                    ctx.throw_type_error("malformed UTF-8");
                    return Err(-1);
                }
                buf.push_char(unsafe { char::from_u32_unchecked(uc) })
                    .map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
            }
        } else {
            buf.push_u16(cp16).map_err(|_| {
                ctx.throw_out_of_memory();
                -1
            })?;
        }
    }

    ctx.create_string_with_buf(buf).map(|s| s.into())
}

// global objects
pub static js_global_props: [FieldDef; 9] = [
    JS_CFUNC_NATIVE_DEF!("parseInt", 2, js_parseInt, 0),
    JS_CFUNC_NATIVE_DEF!("parseFloat", 1, js_parseFloat, 0),
    JS_CFUNC_NATIVE_DEF!("isNaN", 1, js_global_isNaN_isFinite, MAGIC_NUM_IS_NAN as _),
    JS_CFUNC_NATIVE_DEF!(
        "isFinite",
        1,
        js_global_isNaN_isFinite,
        MAGIC_NUM_IS_FINITE as _
    ),
    JS_CFUNC_NATIVE_DEF!("decodeURI", 1, js_global_decodeURI, 0),
    JS_CFUNC_NATIVE_DEF!("decodeURIComponent", 1, js_global_decodeURI, 1),
    // JS_CFUNC_MAGIC_DEF("encodeURI", 1, js_global_encodeURI, 0),
    // JS_CFUNC_MAGIC_DEF("encodeURIComponent", 1, js_global_encodeURI, 1),
    // JS_CFUNC_DEF("escape", 1, js_global_escape),
    // JS_CFUNC_DEF("unescape", 1, js_global_unescape),
    JS_PROP_DOUBLE_DEF!("Infinity", f64::INFINITY, 0),
    JS_PROP_DOUBLE_DEF!("NaN", f64::NAN, 0),
    JS_PROP_UNDEFINED_DEF!("undefined", 0),
];
