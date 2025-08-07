use std::{borrow::Cow, clone, rc::Rc};

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
                format!("[bytecode {}]", b.0.borrow().func_name.to_utf8(b.rt())).into()
            }
            Self::Object(p) => format!("{p}").into(),
            Self::Symbol(p) => {
                format!("Symbol({})", p.atom_info().unwrap().atom().to_utf8(p.rt())).into()
            }
            Self::Module(_) => "[module]".into(),
            Self::CatchOffset(n) => format!("[catchoffset({})]", *n).into(),
            Self::AsyncStateResume(_) => "[asyncfuncresume]".into(),
        }
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
