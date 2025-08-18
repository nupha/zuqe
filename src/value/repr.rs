use std::{borrow::Cow, rc::Rc};

use smallvec::SmallVec;

use crate::{
    runtime::JSRuntime,
    string::{JSString, StringChar},
};

use super::JSValue;

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

                for (_, c) in p.content().iter() {
                    match c {
                        StringChar::Char(c) => match c {
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
                                buf.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                            }
                        },

                        StringChar::Code(c) => {
                            buf.extend_from_slice(format!("\\u{c:04X}").as_bytes());
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
