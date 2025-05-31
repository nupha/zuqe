use crate::{
    atom::consts::JS_ATOM_Symbol_match,
    context::JSContext,
    gc::GCMarkFunc,
    parse::JSParseState,
    runtime::JSRuntime,
    value::{
        object::JSObjectIsa, JSString, JSValue, JSValueConst, JS_ToBoolFree, JS_TAG_OBJECT,
        JS_VALUE_GET_TAG,
    },
    JS_CLASS_REGEXP,
};

pub struct JSRegExpStringIteratorData {
    pub iterating_regexp: JSValue,
    pub iterated_string: JSValue,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct JSRegExp {
    pattern: *mut JSString,
    bytecode: *mut JSString, /* also contains the flags */
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
    ctx: *mut JSContext,
    ctor: JSValueConst,
    pattern: JSValue,
    bc: JSValue,
) -> JSValue {
    unimplemented!()
}

pub unsafe fn js_get_regexp(
    ctx: *mut JSContext,
    obj: JSValueConst,
    throw_error: bool,
) -> *mut JSRegExp {
    if JS_VALUE_GET_TAG(obj) == JS_TAG_OBJECT {
        if let JSObjectIsa::REGEXP(re) = obj.object_unchecked().isa() {
            return *re;
        }
    }

    if throw_error {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeErrorInvalidClass(JS_CLASS_REGEXP);
    }

    std::ptr::null_mut()
}

pub unsafe fn js_is_regexp(ctx: *mut JSContext, obj: JSValueConst) -> Result<bool, i32> {
    if !obj.is_object() {
        return Ok(false);
    }

    let m = obj.get_property(ctx.as_mut().unwrap(), JS_ATOM_Symbol_match);
    if m.is_exception() {
        return Err(-1);
    }
    if !m.is_undefined() {
        return JS_ToBoolFree(ctx, m);
    }
    if !js_get_regexp(ctx, obj, false).is_null() {
        Ok(true)
    } else {
        Ok(false)
    }
}

pub unsafe fn js_regexp_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_regexp_iterator_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_regexp_iterator_mark(rt: *mut JSRuntime, val: JSValue, mark_func: GCMarkFunc) {
    unimplemented!();
}
