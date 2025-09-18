use crate::{context::JSContext, number::f64_stringify};

use super::JSValue;

impl JSValue {
    /// Get representation string of `self` value.
    /// repr string if used for interactive prompting only.
    pub fn repr(&self, ctx: &JSContext) -> Result<String, i32> {
        match self {
            // simple value types
            Self::Int(i) => Ok(format!("{}", *i).into()),
            Self::Bool(b) => Ok(if *b { "true" } else { "false" }.into()),
            Self::Float64(n) => {
                Ok(if *n == 0.0 {
                    // distinct '0' and '-0'
                    if n.is_sign_negative() { "-0" } else { "0" }.into()
                } else {
                    f64_stringify(*n, 10, None, 0).into()
                })
            }
            Self::NULL => Ok("null".into()),
            Self::UNDEFINED => Ok("undefined".into()),
            Self::Module(_) => Ok("[module]".into()),

            // complex value types
            Self::String(s) => Ok(s.content().to_utf8().into_owned()),
            Self::Symbol(p) => {
                let s = p.atom_info().unwrap().atom().to_utf8(ctx.rt());
                Ok(format!("Symbol({s})").into())
            }
            Self::Object(o) => {
                if let Some(v) = { o.0.payload.borrow().value() } {
                    // primitive object
                    Ok(format!("[object {}]", v.repr(ctx)?))
                } else {
                    Ok(format!(
                        "[object {}]",
                        o.class_name().to_string(ctx)?.content().to_utf8()
                    ))
                }
            }
            Self::Bytecode(b) => {
                Ok(format!("[bytecode {}]", b.0.borrow().func_name.to_utf8(b.rt())).into())
            }

            // internal value types
            Self::EXCEPTION => Ok("<exception>".into()),
            Self::UNINITIALIZED => Ok("<uninitialized>".into()),
            Self::CatchOffset(n) => Ok(format!("<catchoffset {}>", *n).into()),
            Self::AsyncStateResume(_) => Ok("<asyncfuncresume>".into()),
        }
    }
}

// impl std::fmt::Debug for JSValue {
//     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
//         let str: Cow<str> = match self {
//             Self::Int(i) => format!("{}", *i).into(),
//             Self::Bool(b) => if *b { "true" } else { "false" }.into(),
//             Self::UNDEFINED => "undefined".into(),
//             Self::NULL => "null".into(),
//             Self::String(s) => format!("{s}").into(),
//             Self::Symbol(p) => {
//                 format!("Symbol({})", p.atom_info().unwrap().atom().to_utf8(p.rt())).into()
//             }
//             Self::Object(p) => format!("{p}").into(),
//             Self::Float64(n) => {
//                 if *n == 0.0 {
//                     if n.is_sign_negative() { "-0" } else { "0" }.into()
//                 } else {
//                     f64_stringify(*n, 10, None, 0)
//                 }
//             }
//             Self::Bytecode(b) => {
//                 format!("[bytecode {}]", b.0.borrow().func_name.to_utf8(b.rt())).into()
//             }
//             Self::Module(_) => "[module]".into(),

//             Self::EXCEPTION => "[exception]".into(),

//             // internal value types
//             Self::UNINITIALIZED => "<uninitialized>".into(),
//             Self::CatchOffset(n) => format!("<catchoffset {}>", *n).into(),
//             Self::AsyncStateResume(_) => "<asyncfuncresume>".into(),
//         };

//         write!(f, "{str}")
//     }
// }

// pub fn JS_DumpString(p: &JSString) {
//     let ref_cnt = Rc::strong_count(&p.0);
//     print!("{ref_cnt}");

//     let sep = if ref_cnt == 1 { b'\"' } else { b'\'' } as char;
//     print!("{sep}");

//     for (_, c) in p.content().iter() {
//         match c {
//             StringChar::Char(ch) => match ch {
//                 '\'' | '"' if ch == sep => print!("\\{ch}"),
//                 '\n' => print!("\\n"),
//                 '\r' => print!("\\r"),
//                 '\t' => print!("\\t"),
//                 '\\' => print!("\\\\"),
//                 ' '..='~' => print!("{ch}"),
//                 _ => {
//                     print!("{ch}");
//                     // print!("\\u{:04x}", c as u32)
//                 }
//             },
//             StringChar::Code(c) => {
//                 print!("\\u{c:04X}")
//             }
//         }
//     }

//     print!("{sep}");
// }
