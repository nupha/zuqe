use std::ops::Deref;

use crate::{
    JS_CLASS_REGEXP,
    atom::consts::JS_ATOM_Symbol_match,
    context::JSContext,
    object::ObjectDetail,
    parse::JSParseState,
    runtime::JSRuntime,
    string::JSString,
    value::{JSValue, JSValueConst},
};

#[derive(Debug)]
pub struct JSRegExpStringIteratorData {
    pub iterating_regexp: JSValue,
    pub iterated_string: JSValue,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

#[derive(Debug, Clone)]
pub struct JSRegExp {
    pattern: JSString,
    bytecode: JSString, /* also contains the flags */
}

// code point ranges for Zs,Zl or Zp property
pub static char_range_s: [u16; 21] = [
    10,
    0x0009,
    0x000D + 1,
    0x0020,
    0x0020 + 1,
    0x00A0,
    0x00A0 + 1,
    0x1680,
    0x1680 + 1,
    0x2000,
    0x200A + 1,
    // 2028;LINE SEPARATOR;Zl;0;WS;;;;;N;;;;;
    // 2029;PARAGRAPH SEPARATOR;Zp;0;B;;;;;N;;;;;
    0x2028,
    0x2029 + 1,
    0x202F,
    0x202F + 1,
    0x205F,
    0x205F + 1,
    0x3000,
    0x3000 + 1,
    // FEFF;ZERO WIDTH NO-BREAK SPACE;Cf;0;BN;;;;;N;BYTE ORDER MARK;;;;
    0xFEFF,
    0xFEFF + 1,
];

pub const lre_id_start_table_ascii: [u32; 4] = [
    /* $ A-Z _ a-z */
    0x00000000, 0x00000010, 0x87FFFFFE, 0x07FFFFFE,
];

pub const lre_id_continue_table_ascii: [u32; 4] = [
    /* $ 0-9 A-Z _ a-z */
    0x00000000, 0x03FF0010, 0x87FFFFFE, 0x07FFFFFE,
];

pub unsafe fn lre_is_space(c: i32) -> bool {
    let n = (char_range_s.len() - 1) / 2;
    for i in 0..n {
        let low = char_range_s[2 * i + 1];
        if c < low as i32 {
            return false;
        }
        let high = char_range_s[2 * i + 2];
        if c < high as i32 {
            return true;
        }
    }
    false
}

#[deprecated(note = "use JSParseState::is_ident_next() instead")]
pub unsafe fn lre_js_is_ident_next(c: char) -> bool {
    JSParseState::is_ident_next(c)
}

#[deprecated(note = "use JSParseState::is_ident_first() instead")]
pub unsafe fn lre_js_is_ident_first(c: char) -> bool {
    JSParseState::is_ident_first(c)
}

pub const fn from_hex(c: u8) -> Result<u8, i32> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(-1),
    }
}

#[inline(always)]
pub const fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

/* create a RegExp object from a string containing the RegExp bytecode
and the source pattern */
pub unsafe fn js_regexp_constructor_internal(
    ctx: &JSContext,
    ctor: JSValueConst,
    pattern: JSValue,
    bc: JSValue,
) -> JSValue {
    unimplemented!()
}

pub fn js_get_regexp(ctx: &JSContext, obj: &JSValueConst, throw_error: bool) -> Option<JSRegExp> {
    if let JSValue::Object(ob) = obj {
        if let ObjectDetail::REGEXP(re) = ob.0.payload.borrow().deref() {
            return Some(*re.clone());
        }
    }
    if throw_error {
        ctx.JS_ThrowTypeErrorInvalidClass(JS_CLASS_REGEXP);
    }
    None
}

pub fn js_is_regexp(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    if !obj.is_object() {
        return Ok(false);
    }

    let m = obj.get_property(ctx, JS_ATOM_Symbol_match);
    if m.is_exception() {
        return Err(-1);
    }
    if !m.is_undefined() {
        return m.coerce_to_bool(ctx);
    }
    if js_get_regexp(ctx, obj, false).is_some() {
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn js_regexp_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {
    unimplemented!();
}

pub fn js_regexp_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {
    unimplemented!();
}

pub fn js_regexp_iterator_mark(val: JSValue) {
    unimplemented!();
}
