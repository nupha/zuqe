use std::{borrow::Cow, rc::Rc};

use crate::string::{JSString, StringChar};

use super::JSValue;

impl std::fmt::Display for JSValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let str: Cow<str> = match self {
            Self::Int(i) => format!("{}", *i).into(),
            Self::Bool(b) => if *b { "true" } else { "false" }.into(),
            Self::UNDEFINED => "undefined".into(),
            Self::NULL => "null".into(),
            Self::String(s) => format!("{s}").into(),
            Self::Symbol(p) => {
                format!("Symbol({})", p.atom_info().unwrap().atom().to_utf8(p.rt())).into()
            }
            Self::Object(p) => format!("{p}").into(),
            Self::Float64(n) => match *n {
                f64::INFINITY => "Infinity".into(),
                f64::NEG_INFINITY => "-Infinity".into(),
                0.0 if n.is_sign_negative() => "-0".into(),
                0.0 => "0".into(),
                n => format!("{n}").into(),
            },

            Self::Module(_) => "[module]".into(),
            Self::Bytecode(b) => {
                format!("[bytecode {}]", b.0.borrow().func_name.to_utf8(b.rt())).into()
            }

            Self::EXCEPTION => "[exception]".into(),
            Self::UNINITIALIZED => "[uninitialized]".into(),
            Self::CatchOffset(n) => format!("[catchoffset {}]", *n).into(),
            Self::AsyncStateResume(_) => "[asyncfuncresume]".into(),
        };

        write!(f, "{str}")
    }
}

pub fn JS_DumpString(p: &JSString) {
    let ref_cnt = Rc::strong_count(&p.0);
    print!("{ref_cnt}");

    let sep = if ref_cnt == 1 { b'\"' } else { b'\'' } as char;
    print!("{sep}");

    for (_, c) in p.content().iter() {
        match c {
            StringChar::Char(ch) => match ch {
                '\'' | '"' if ch == sep => print!("\\{ch}"),
                '\n' => print!("\\n"),
                '\r' => print!("\\r"),
                '\t' => print!("\\t"),
                '\\' => print!("\\\\"),
                ' '..='~' => print!("{ch}"),
                _ => {
                    print!("{ch}");
                    // print!("\\u{:04x}", c as u32)
                }
            },
            StringChar::Code(c) => {
                print!("\\u{c:04X}")
            }
        }
    }

    print!("{sep}");
}
