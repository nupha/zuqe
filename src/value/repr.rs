use std::{borrow::Cow, rc::Rc};

use smallvec::SmallVec;

use crate::{
    atom::consts::{
        JS_ATOM_false, JS_ATOM_null, JS_ATOM_toLocaleString, JS_ATOM_true, JS_ATOM_undefined,
    },
    context::JSContext,
    error::JSResult,
    function::JS_Invoke,
    number::f64_stringify,
    runtime::JSRuntime,
    string::JSString,
};

use super::{HINT_STRING, JSValue};

impl JSValue {
    /// get string representation of value.
    pub fn repr(&self) -> Cow<'_, str> {
        match self {
            Self::Int(i) => format!("{}", *i).into(),
            Self::Bool(b) => if *b { "true" } else { "false" }.into(),
            Self::UNDEFINED => "undefined".into(),
            Self::NULL => "null".into(),
            Self::EXCEPTION => "[exception]".into(),
            Self::UNINITIALIZED => "[uninitialized]".into(),
            Self::Float64(n) => format!("{:14}", *n).into(),
            Self::String(p) => {
                let sep = '\"';
                let mut buf = SmallVec::<[u8; 128]>::new();
                buf.push(sep as u8);
                for c in p.content().chars() {
                    match c {
                        '\'' | '"' if c == sep => {
                            buf.extend_from_slice(format!("\\{c}").as_bytes());
                        }
                        '\n' => {
                            buf.extend_from_slice(b"\\n");
                        }
                        '\r' => {
                            buf.extend_from_slice(b"\\r");
                        }
                        '\t' => {
                            buf.extend_from_slice(b"\\t");
                        }
                        '\\' => {
                            buf.extend_from_slice(b"\\\\");
                        }
                        ' '..='~' => {
                            buf.push(c as u8);
                        }
                        _ => {
                            buf.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
                        }
                    }
                }
                buf.push(sep as u8);
                String::from_utf8(buf.to_vec()).unwrap().into()
            }
            Self::Bytecode(b) => {
                format!("[bytecode {}]", b.0.borrow().func_name.to_str(b.rt())).into()
            }
            Self::Object(p) => format!("{p}").into(),
            Self::Symbol(p) => {
                format!("Symbol({})", p.atom_info().unwrap().atom().to_str(p.rt())).into()
            }
            Self::Module(_) => "[module]".into(),
            Self::CatchOffset(n) => format!("[catchoffset({})]", *n).into(),
            Self::AsyncStateResume(_) => "[asyncfuncresume]".into(),
        }
    }

    /// Convert value `self` to string or symbol.
    /// returns String or Symbol value.
    fn coerce_to_string_or_symbol(
        &self,
        ctx: &JSContext,
        allow_symbol: bool,
    ) -> Result<JSValue, i32> {
        let ret: Result<JSValue, i32> = match self {
            Self::String(s) => Ok(s.clone().into()),
            Self::Int(i) => ctx
                .create_string_from_utf8(format!("{}", *i).into_bytes())
                .map(JSValue::String),
            Self::Bool(b) => if *b { JS_ATOM_true } else { JS_ATOM_false }
                .to_string(ctx)
                .map(JSValue::String),
            Self::NULL => JS_ATOM_null.to_string(ctx).map(JSValue::String),
            Self::UNDEFINED => JS_ATOM_undefined.to_string(ctx).map(JSValue::String),
            Self::Object(_) => self
                .coerce_to_primitive(ctx, HINT_STRING)?
                .coerce_to_string_or_symbol(ctx, allow_symbol),
            Self::Symbol(s) => {
                if allow_symbol {
                    Ok(JSValue::Symbol(s.clone()))
                } else {
                    ctx.throw_type_error("cannot convert symbol to string");
                    Err(-1)
                }
            }
            Self::Module(_) => ctx.create_string("[module]").map(JSValue::String),
            Self::Bytecode(_) => ctx
                .create_string("[function bytecode]")
                .map(JSValue::String),
            Self::Float64(n) => {
                let s = f64_stringify(*n, 10, None, 0);
                ctx.create_string(s.as_ref()).map(JSValue::String)
            }
            Self::UNINITIALIZED => ctx.create_string("[uninitialized]").map(JSValue::String),
            Self::CatchOffset(_) => ctx.create_string("[cacthoffset]").map(JSValue::String),
            Self::AsyncStateResume(_) => {
                ctx.create_string("[asyncfuncresume]").map(JSValue::String)
            }
            Self::EXCEPTION => {
                return Err(-1);
            }
        };

        ret
    }

    /// Coerce value to string.
    /// Returns a string type value or Err.
    #[inline(always)]
    pub fn coerce_to_string(&self, ctx: &JSContext) -> Result<JSString, i32> {
        if let Self::String(s) = self.coerce_to_string_or_symbol(ctx, false)? {
            Ok(s)
        } else {
            unreachable!()
        }
    }

    /// Invoking `toLocaleString()` method, returns result value.
    /// `self` is not consumed.
    pub fn to_locale_string(&self, ctx: &JSContext) -> Result<JSString, i32> {
        if self.is_undefined_or_null() {
            self.coerce_to_string(ctx)
        } else if let Self::String(s) =
            JS_Invoke(ctx, self, JS_ATOM_toLocaleString, &[]).check_exception()?
        {
            Ok(s)
        } else {
            ctx.throw_internal_error("toLocaleString() must return string");
            Err(-1)
        }
    }

    /// Coerce `self` value to property key value.
    /// returns string or symbol value
    #[inline(always)]
    pub fn coerce_to_propkey(&self, ctx: &JSContext) -> Result<JSValue, i32> {
        self.coerce_to_string_or_symbol(ctx, true)
    }
}

pub unsafe fn JS_DumpString(rt: &JSRuntime, p: &JSString) {
    // if p.is_null() {
    //     print!("<null>");
    //     return;
    // }

    let ref_cnt = Rc::strong_count(&p.0);
    print!("{ref_cnt}");

    let sep = if ref_cnt == 1 { b'\"' } else { b'\'' } as char;
    print!("{sep}");

    for c in p.content().chars() {
        match c {
            '\'' | '"' if c == sep => print!("\\{c}"),
            '\n' => print!("\\n"),
            '\r' => print!("\\r"),
            '\t' => print!("\\t"),
            '\\' => print!("\\\\"),
            ' '..='~' => print!("{c}"),
            _ => print!("\\u{:04x}", c as u32),
        }
    }

    print!("{sep}");
}
