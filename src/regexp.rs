use std::{ops::Deref, rc::Rc};

use regex::Regex;
use smallvec::SmallVec;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_DEF, JS_CGETSET_NATIVE_DEF, JS_CLASS_REGEXP,
    atom::consts::*,
    context::{JS_GetActiveFunction, JSContext},
    error::{ErrorKind, JSResult},
    function::{CallContext, NativeField},
    js_core::js_get_this,
    object::{JS_PROP_C_W_E, JS_PROP_THROW, JS_PROP_WRITABLE, JSObject, ObjectDetail},
    runtime::JSRuntime,
    string::{JSString, StringBuf, StringChar},
    value::JSValue,
};

pub const RE_HEADER_FLAGS: usize = 0;
pub const RE_HEADER_CAPTURE_COUNT: usize = 1;
pub const RE_HEADER_STACK_SIZE: usize = 2;
pub const RE_HEADER_LEN: usize = 7;

pub const LRE_FLAG_GLOBAL: i32 = 1 << 0;
pub const LRE_FLAG_IGNORECASE: i32 = 1 << 1;
pub const LRE_FLAG_MULTILINE: i32 = 1 << 2;
pub const LRE_FLAG_DOTALL: i32 = 1 << 3;
pub const LRE_FLAG_UTF16: i32 = 1 << 4;
pub const LRE_FLAG_STICKY: i32 = 1 << 5;
pub const LRE_FLAG_INDICES: i32 = 1 << 6; // Unused by libregexp, just recorded. 
pub const LRE_FLAG_NAMED_GROUPS: i32 = 1 << 7; // named groups are present in the regexp

#[derive(Debug)]
pub struct JSRegExpStringIteratorData {
    pub iterating_regexp: JSValue,
    pub iterated_string: JSValue,
    pub global: bool,
    pub unicode: bool,
    pub done: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(u8)]
pub enum RegExpExecMode {
    Capture,
    Match,
    Test,
}

#[derive(Debug, Clone)]
pub struct JSRegExp {
    pattern: JSString,
    flags: u8,
    regex: Rc<Regex>,
}

impl JSRegExp {
    pub fn new(ctx: &JSContext, pattern: JSString, flags_str: &str) -> Result<Self, i32> {
        let mut flags = 0u8;
        for c in flags_str.chars() {
            match c {
                'g' => {
                    flags |= LRE_FLAG_GLOBAL as u8;
                }
                'i' => {
                    flags |= LRE_FLAG_IGNORECASE as u8;
                }
                'm' => {
                    flags |= LRE_FLAG_MULTILINE as u8;
                }
                'u' => {
                    flags |= LRE_FLAG_UTF16 as u8;
                }
                's' => {
                    flags |= LRE_FLAG_DOTALL as u8;
                }
                'y' => {
                    flags |= LRE_FLAG_STICKY as u8;
                }
                _ => {}
            }
        }

        let regex = Rc::new(Regex::new(&pattern.content().to_utf8()).map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?);

        Ok(Self {
            pattern,
            regex,
            flags,
        })
    }

    pub fn exec(
        &self,
        ctx: &JSContext,
        str: &JSString,
        start: usize,
        mode: RegExpExecMode,
    ) -> Result<Option<(JSValue, usize)>, i32> {
        let is_global = self.flags & LRE_FLAG_GLOBAL as u8 != 0;
        let is_sticky = self.flags & LRE_FLAG_STICKY as u8 != 0;

        let sbuf = str.content();
        let sbuf_slice = sbuf.slice(start..sbuf.buf_len());
        let utf8_text = sbuf_slice.to_utf8();

        if !utf8_text.is_empty() {
            let prop_flags = JS_PROP_C_W_E | JS_PROP_THROW;

            // iterator to map utf8 index to str buf index
            let mut chars_iter = if sbuf.is_wide() {
                Some(sbuf.iter_since(start).zip(utf8_text.chars()))
            } else {
                None
            };

            let ret = match (mode, is_global) {
                (RegExpExecMode::Capture, _) | (RegExpExecMode::Match, false) => {
                    if let Some(caps) = self.regex.captures(&utf8_text)
                        && (!is_sticky || caps.get(0).unwrap().start() == start)
                    {
                        let mut names = SmallVec::<[String; 10]>::new();
                        for n in self.regex.capture_names().filter_map(|n| n) {
                            names.push(n.to_string());
                        }

                        let mut index = 0;
                        let mut full_text: JSString = JS_ATOM_empty_string.to_string(ctx).unwrap();
                        let arr = ctx.create_array()?;

                        for (i, cap) in caps.iter().enumerate() {
                            arr.define_property_value_u32(
                                ctx,
                                i as _,
                                if let Some(m) = cap {
                                    let s = ctx.create_string_from_str(m.as_str())?;
                                    if i == 0 {
                                        full_text = s.clone();
                                        index = m.start();
                                    }
                                    s.into()
                                } else {
                                    JSValue::UNDEFINED
                                },
                                prop_flags,
                            )?;
                        }

                        // groups
                        let groups = if !names.is_empty() {
                            let gg = ctx.create_base_object()?;
                            for name in names {
                                if let Some(m) = caps.name(&name) {
                                    gg.define_property_value_str(
                                        ctx,
                                        &name,
                                        ctx.create_string_from_str(m.as_str())?,
                                        prop_flags,
                                    )?;
                                }
                            }
                            gg.into()
                        } else {
                            JSValue::UNDEFINED
                        };
                        arr.define_property_value_str(ctx, "groups", groups, prop_flags)?;

                        // Note: convert utf8 index to string buf index
                        let start_index = if index == 0 {
                            start
                        } else if let Some(mut it) = chars_iter.clone() {
                            let char_count = utf8_text[..index].chars().count();
                            let ((i, _), _) = it.nth(char_count).unwrap();
                            i + start
                        } else {
                            index
                        };
                        let last_index = start_index + full_text.buf_len();

                        arr.define_property_value_str(
                            ctx,
                            "index",
                            JSValue::Int(start_index as _),
                            prop_flags,
                        )?;

                        arr.define_property_value_str(
                            ctx,
                            "input",
                            str.clone().into(),
                            prop_flags,
                        )?;

                        Some((arr, last_index))
                    } else {
                        None
                    }
                }
                (RegExpExecMode::Match, true) => {
                    let mut arr: Option<JSValue> = None;
                    let mut cursor = 0;
                    let mut n = 0;
                    while let Some(m) = self.regex.find_at(&utf8_text, cursor)
                        && (!is_sticky || m.start() == cursor)
                    {
                        if arr.is_none() {
                            let a = ctx.create_array()?;
                            arr.replace(a);
                        }

                        arr.as_ref().unwrap().define_property_value_u32(
                            ctx,
                            n,
                            ctx.create_string_from_str(m.as_str())?.into(),
                            prop_flags,
                        )?;

                        n += 1;
                        cursor = m.end();
                    }

                    if let Some(arr) = arr {
                        let last_index = if cursor == utf8_text.len() {
                            sbuf.buf_len()
                        } else if let Some(it) = &mut chars_iter {
                            let char_count = utf8_text[..cursor].chars().count();
                            let ((i, _), _) = it.nth(char_count).unwrap();
                            i + start
                        } else {
                            cursor + start
                        };
                        Some((arr, last_index))
                    } else {
                        None
                    }
                }
                (RegExpExecMode::Test, _) => {
                    todo!()
                }
            };

            Ok(ret)
        } else {
            Ok(None)
        }
    }
}

pub const fn from_hex(c: u8) -> Result<u8, i32> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        _ => Err(-1),
    }
}

impl JSContext {
    /// create RegExp object
    pub fn create_regexp(
        &self,
        ctor: &JSValue,
        pattern: JSString,
        flags: &str,
    ) -> Result<JSObject, i32> {
        let obj = self
            .create_object_apply_ctor(
                ctor,
                ObjectDetail::REGEXP(Box::new(JSRegExp::new(self, pattern, flags)?)),
            )
            .map_err(|_| -1)?;

        obj.define_property_value(self, JS_ATOM_lastIndex, 0, JS_PROP_WRITABLE)?;
        Ok(obj)
    }
}

/* create a RegExp object from a string containing the RegExp bytecode
and the source pattern */
pub fn js_regexp_constructor_internal(
    ctx: &JSContext,
    ctor: &JSValue,
    pattern: JSString,
    flags: JSValue,
) -> Result<JSObject, i32> {
    if !flags.is_undefined() {
        ctx.create_regexp(
            ctor,
            pattern,
            flags.to_string(ctx)?.content().to_utf8().as_ref(),
        )
    } else {
        ctx.create_regexp(ctor, pattern, "")
    }
}

pub fn js_regexp_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let pat = cc.arg(0);
    let pat_is_regexp = js_is_regexp(ctx, pat)?;

    let mut flags1 = cc.arg(1).clone();

    if cc.this().is_undefined() {
        // called as a function
        let new_target_tmp = unsafe { JS_GetActiveFunction(ctx) };
        if pat_is_regexp && flags1.is_undefined() {
            let ctor = pat.get_property(ctx, JS_ATOM_constructor)?;
            if ctor.is_same(new_target_tmp) {
                return Ok(pat.clone());
            }
        }
    }

    let mut re = if let Some(re) = pat.object().and_then(|o| js_get_regexp(ctx, o, false)) {
        re
    } else {
        let (pat_val, flags_val) = if pat_is_regexp {
            (
                pat.get_property(ctx, JS_ATOM_source)?,
                if flags1.is_undefined() {
                    pat.get_property(ctx, JS_ATOM_flags)?
                } else {
                    flags1.clone()
                },
            )
        } else {
            (pat.clone(), flags1.clone())
        };

        let pattern = if pat_val.is_undefined() {
            JS_ATOM_empty_string.to_string(ctx).unwrap()
        } else {
            pat_val.to_string(ctx)?
        };
        flags1 = flags_val;

        let src = pattern.content().to_utf8();
        let regex = Regex::new(src.as_ref()).map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?;

        JSRegExp {
            pattern,
            flags: 0,
            regex: Rc::new(regex),
        }
    };

    if !flags1.is_undefined() {
        let mut flags = 0u8;

        for c in flags1.to_string(ctx)?.content().to_utf8().chars() {
            match c {
                'g' => {
                    flags |= LRE_FLAG_GLOBAL as u8;
                }
                'i' => {
                    flags |= LRE_FLAG_IGNORECASE as u8;
                }
                'm' => {
                    flags |= LRE_FLAG_MULTILINE as u8;
                }
                'u' => {
                    flags |= LRE_FLAG_UTF16 as u8;
                }
                's' => {
                    flags |= LRE_FLAG_DOTALL as u8;
                }
                'y' => {
                    flags |= LRE_FLAG_STICKY as u8;
                }
                _ => {}
            }
        }

        re.flags = flags;
    }

    let isa = ObjectDetail::REGEXP(Box::new(re));
    let obj = ctx
        .create_object_apply_ctor(cc.this(), isa)
        .map_err(|_| -1)?;
    obj.define_property_value(ctx, JS_ATOM_lastIndex, 0, JS_PROP_WRITABLE)?;
    Ok(obj.into())
}

pub fn js_get_regexp(ctx: &JSContext, obj: &JSObject, throw_error: bool) -> Option<JSRegExp> {
    if let ObjectDetail::REGEXP(re) = obj.0.payload.borrow().deref() {
        Some(*re.clone())
    } else {
        if throw_error {
            ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP);
        }
        None
    }
}

pub fn js_is_regexp(ctx: &JSContext, obj: &JSValue) -> Result<bool, i32> {
    if let JSValue::Object(o) = obj {
        let m = o.get_property(ctx, JS_ATOM_Symbol_match)?;
        if !m.is_undefined() {
            m.coerce_to_bool(ctx)
        } else {
            Ok(js_get_regexp(ctx, o, false).is_some())
        }
    } else {
        Ok(false)
    }
}

pub fn js_regexp_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {}

pub fn js_regexp_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectDetail, id: usize) {
    unimplemented!();
}

pub fn js_regexp_iterator_mark(val: JSValue) {
    unimplemented!();
}

pub fn js_regexp_get_source(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this = cc.this();

    if let JSValue::Object(o) = this {
        if cc
            .this()
            .is_same(&ctx.0.borrow().class_proto[JS_CLASS_REGEXP as usize])
        {
            // empty_regex
            return ctx.create_string_from_str("(?:))").map(JSValue::String);
        }

        let re = js_get_regexp(ctx, o, true).ok_or(-1)?;
        let p = re.pattern;
        if p.is_empty() {
            return ctx.create_string_from_str("(?:))").map(JSValue::String);
        }

        let mut buf = StringBuf::with_capacity(p.buf_len() * 3 / 2, p.is_wide()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        /* Escape '/' and newline sequences as needed */
        let mut bra = false;
        let mut chars = p.content().iter();
        while let Some((_, c)) = chars.next() {
            match c {
                StringChar::Char('\\') => {
                    buf.push_char('\\');
                    if let Some((_, x)) = chars.next() {
                        buf.push_char(x);
                    }
                }
                StringChar::Char(']') => {
                    bra = false;
                    buf.push_char(']');
                }
                StringChar::Char('[') => {
                    if !bra {
                        if chars.peek() == Some(']' as u32 as u16) {
                            // [] case
                            chars.next(); // consume ']'
                            buf.push_str("[]");
                            continue;
                        }
                        bra = true;
                    }
                    buf.push_char('[');
                }
                StringChar::Char('\n') => {
                    buf.push_char('\n');
                }
                StringChar::Char('\r') => {
                    buf.push_char('\r');
                }

                StringChar::Char('/') => {
                    if bra {
                        buf.push_str("\\/");
                    } else {
                        buf.push_char('/');
                    }
                }
                c => {
                    buf.push_char(c);
                }
            }
        }

        buf.shrink_to_fit();
        ctx.create_string_from_content(buf).map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    }
}

pub fn js_regexp_get_flag(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mask = cc.magic();
    let this = cc.this();

    if let JSValue::Object(o) = this {
        if let Some(re) = js_get_regexp(ctx, o, false) {
            Ok(JSValue::Bool((re.flags as i32) & mask != 0))
        } else {
            if this.is_same(&ctx.0.borrow().class_proto[JS_CLASS_REGEXP as usize]) {
                Ok(JSValue::UNDEFINED)
            } else {
                ctx.throw_type_error_invalid_class(JS_CLASS_REGEXP);
                Err(-1)
            }
        }
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_get_flags(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(o) = cc.this() {
        const TESTS: [(&str, u8); 7] = [
            ("hasIndices", 'd' as u8),
            ("global", 'g' as u8),
            ("ignoreCase", 'i' as u8),
            ("multiline", 'm' as u8),
            ("dotAll", 's' as u8),
            ("unicode", 'u' as u8),
            ("sticky", 'y' as u8),
        ];

        let mut str: [u8; 8] = [0; 8];
        let mut pos = 0_usize;

        for (name, ch) in TESTS.iter() {
            if o.get_property_str(ctx, *name)
                .and_then(|b| b.coerce_to_bool(ctx))?
            {
                str[pos] = *ch;
                pos += 1;
            }
        }

        ctx.create_string_from_str(std::str::from_utf8(&str[0..pos]).unwrap())
            .map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

pub fn js_regexp_toString(ctx: &JSContext, cc: CallContext) -> JSResult {
    if let JSValue::Object(_) = cc.this() {
        let Ok(mut buf) = StringBuf::with_capacity(96, false) else {
            ctx.throw_out_of_memory();
            return Err(-1);
        };

        let _ = buf.push_u8('/' as u8);

        let pattern = cc
            .this()
            .get_property(ctx, JS_ATOM_source)
            .and_then(|v| v.to_string(ctx))?;

        buf.push(&pattern.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        buf.push_u8('/' as u8).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        let flags = cc
            .this()
            .get_property(ctx, JS_ATOM_flags)
            .and_then(|v| v.to_string(ctx))?;

        buf.push(&flags.content()).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;

        buf.shrink_to_fit();
        ctx.create_string_from_content(buf).map(JSValue::String)
    } else {
        ctx.throw_type_error_not_an_object();
        Err(-1)
    }
}

fn js_regexp_compile(ctx: &JSContext, cc: CallContext) -> JSResult {
    let re = js_get_regexp(ctx, cc.this().object().unwrap(), true).ok_or(-1)?;

    let pattern1 = cc.arg(0);
    let flags1 = cc.arg(1);

    let pattern: JSString;
    let regex: Rc<Regex>;

    if let Some(re1) = pattern1.object().and_then(|o| js_get_regexp(ctx, o, false)) {
        if flags1.is_undefined() {
            ctx.throw_type_error("flags must be undefined");
            return Err(-1);
        }
        pattern = re1.pattern.clone();
        regex = re1.regex.clone();
    } else {
        pattern = if pattern1.is_undefined() {
            JS_ATOM_empty_string.to_string(ctx).unwrap()
        } else {
            pattern1.to_string(ctx)?
        };

        regex = Rc::new(Regex::new(&pattern.content().to_utf8()).map_err(|e| {
            ctx.throw_error(ErrorKind::Syntax, &e.to_string());
            -1
        })?);
    }

    cc.this()
        .set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;

    // TODO: mutable.
    // re.pattern = pattern;
    // re.regex = regex;

    Ok(cc.this().clone())
}

pub fn js_regexp_exec(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    let str = cc.arg(0).to_string(ctx)?;

    let (ret, last_index) = re
        .exec(
            ctx,
            &str,
            if re.flags & LRE_FLAG_GLOBAL as u8 == 0 {
                0
            } else {
                let val = cc.this().get_property(ctx, JS_ATOM_lastIndex)?;
                val.coerce_to_length(ctx)? as usize
            },
            RegExpExecMode::Capture,
        )?
        .unwrap_or_else(|| (JSValue::NULL, 0));

    if re.flags & (LRE_FLAG_GLOBAL | LRE_FLAG_STICKY) as u8 != 0 {
        this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(last_index as _))?;
    }

    Ok(ret)
}

pub fn js_regexp_test(ctx: &JSContext, cc: CallContext) -> JSResult {
    js_regexp_exec(ctx, cc).map(|v| JSValue::Bool(v.is_null()))
}

pub fn js_regexp_Symbol_match(ctx: &JSContext, cc: CallContext) -> JSResult {
    let Some(this) = cc.this().object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let is_match_all = cc.magic() == JS_ATOM_Symbol_matchAll.0 as i32;

    let re = js_get_regexp(ctx, this, true).ok_or(-1)?;
    if is_match_all && (re.flags & LRE_FLAG_GLOBAL as u8 == 0) {
        ctx.throw_type_error("regexp must have the 'g' flag");
        return Err(-1);
    }

    let str = cc.arg(0).to_string(ctx)?;

    let (ret, last_index) = re
        .exec(
            ctx,
            &str,
            if re.flags & LRE_FLAG_GLOBAL as u8 == 0 {
                0
            } else {
                let val = cc.this().get_property(ctx, JS_ATOM_lastIndex)?;
                val.coerce_to_length(ctx)? as usize
            },
            RegExpExecMode::Capture,
        )?
        .unwrap_or_else(|| (JSValue::NULL, 0));

    this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(last_index as _))?;
    Ok(ret)

    // let flags = this.get_property(ctx, JS_ATOM_flags)?;
    // let p = flags.to_string(ctx)?;
    // let A: JSValue;

    // let global = p.content().index_of_char('g', 0).is_some();
    // if !global {
    //     this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
    //     A = js_regexp_exec(ctx, cc)?;
    // } else {
    //     let fullUnicode = this
    //         .get_property(ctx, JS_ATOM_unicode)
    //         .and_then(|v| v.coerce_to_bool(ctx))?;

    //     this.set_property(ctx, JS_ATOM_lastIndex, JSValue::Int(0))?;
    //     A = js_regexp_exec(ctx, cc)?;
    // }

    // Ok(A)
}

pub fn js_regexp_Symbol_replace(ctx: &JSContext, cc: CallContext) -> JSResult {
    todo!("RegExp[Symbol.replace]");
}

pub fn js_regexp_Symbol_search(ctx: &JSContext, cc: CallContext) -> JSResult {
    todo!("RegExp[Symbol.search]");
}

pub fn js_regexp_Symbol_split(ctx: &JSContext, cc: CallContext) -> JSResult {
    todo!("RegExp[Symbol.split]");
}

pub const js_regexp_proto_funcs: [NativeField; 18] = [
    JS_CGETSET_NATIVE_DEF!("flags", js_regexp_get_flags, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("source", js_regexp_get_source, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("global", js_regexp_get_flag, NONE, LRE_FLAG_GLOBAL as _),
    JS_CGETSET_NATIVE_DEF!(
        "ignoreCase",
        js_regexp_get_flag,
        NONE,
        LRE_FLAG_IGNORECASE as _
    ),
    JS_CGETSET_NATIVE_DEF!(
        "multiline",
        js_regexp_get_flag,
        NONE,
        LRE_FLAG_MULTILINE as _
    ),
    JS_CGETSET_NATIVE_DEF!("dotAll", js_regexp_get_flag, NONE, LRE_FLAG_DOTALL as _),
    JS_CGETSET_NATIVE_DEF!("unicode", js_regexp_get_flag, NONE, LRE_FLAG_UTF16 as _),
    JS_CGETSET_NATIVE_DEF!("sticky", js_regexp_get_flag, NONE, LRE_FLAG_STICKY as _),
    JS_CGETSET_NATIVE_DEF!(
        "hasIndices",
        js_regexp_get_flag,
        NONE,
        LRE_FLAG_INDICES as _
    ),
    JS_CFUNC_NATIVE_DEF!("exec", 1, js_regexp_exec, 0),
    JS_CFUNC_NATIVE_DEF!("compile", 2, js_regexp_compile, 0),
    JS_CFUNC_NATIVE_DEF!("test", 1, js_regexp_test, 0),
    JS_CFUNC_NATIVE_DEF!("toString", 0, js_regexp_toString, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.replace]", 2, js_regexp_Symbol_replace, 0),
    JS_CFUNC_NATIVE_DEF!(
        "[Symbol.match]",
        1,
        js_regexp_Symbol_match,
        JS_ATOM_Symbol_match.0 as _
    ),
    JS_CFUNC_NATIVE_DEF!(
        "[Symbol.matchAll]",
        1,
        js_regexp_Symbol_match,
        JS_ATOM_Symbol_matchAll.0 as _
    ),
    JS_CFUNC_NATIVE_DEF!("[Symbol.search]", 1, js_regexp_Symbol_search, 0),
    JS_CFUNC_NATIVE_DEF!("[Symbol.split]", 2, js_regexp_Symbol_split, 0),
    //JS_CGETSET_DEF("__source", js_regexp_get___source, NULL ),
    //JS_CGETSET_DEF("__flags", js_regexp_get___flags, NULL ),
];

pub const js_regexp_funcs: [NativeField; 1] = [
    JS_CGETSET_DEF!("[Symbol.species]", js_get_this, NONE),
    //JS_CFUNC_DEF("__RegExpExec", 2, js_regexp___RegExpExec ),
    //JS_CFUNC_DEF("__RegExpDelete", 2, js_regexp___RegExpDelete ),
];
