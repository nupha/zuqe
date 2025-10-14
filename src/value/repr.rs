use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN, JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS,
    JS_CLASS_NUMBER, JS_CLASS_REGEXP, JS_CLASS_STRING,
    atom::consts::{JS_ATOM_Array, JS_ATOM_Function, JS_ATOM_Symbol_toStringTag},
    context::JSContext,
    number::f64_stringify,
    object::JSObject,
};

use super::JSValue;

impl JSValue {
    /// Get inspect representation string of `self` value.
    /// `inspect()` string is only used for interactive prompting,
    /// use `to_string()` for other cases.
    pub fn inspect(&self, ctx: &JSContext) -> Result<String, i32> {
        match self {
            Self::Int(i) => Ok(format!("{}", *i)),
            Self::Bool(b) => Ok(if *b { "true" } else { "false" }.into()),
            Self::Float64(n) => {
                Ok(if *n == 0.0 {
                    // distinct '0' and '-0'
                    if n.is_sign_negative() { "-0" } else { "0" }.into()
                } else {
                    f64_stringify(*n, 10, None, 0).into()
                })
            }

            Self::String(s) => Ok(s.content().to_utf8(true).into()),
            Self::Symbol(p) => {
                let s = p.atom_info().unwrap().atom().to_utf8(ctx.rt());
                Ok(format!("Symbol({s})"))
            }
            Self::Object(o) => o.inspect(ctx),
            Self::Bytecode(b) => Ok(format!("[bytecode {}]", b.func_name.to_utf8(ctx.rt()))),

            Self::NULL => Ok("null".into()),
            Self::UNDEFINED => Ok("undefined".into()),
            Self::Module(_) => Ok("[module]".into()),

            // internal value types
            Self::EXCEPTION => Ok("<exception>".into()),
            Self::UNINITIALIZED => Ok("<uninitialized>".into()),
            Self::CatchOffset(n) => Ok(format!("<catchoffset {}>", *n)),
            Self::AsyncStateResume(_) => Ok("<asyncfuncresume>".into()),
        }
    }
}

impl JSObject {
    pub fn inspect(&self, ctx: &JSContext) -> Result<String, i32> {
        // get [Symbol.toStringTag] property, if got no string, use tag atom above.
        let tag = if let JSValue::String(s) = self.get_property(ctx, JS_ATOM_Symbol_toStringTag)? {
            format!("{s}")
        } else {
            let tag_atom = if self.is_array(ctx)? {
                JS_ATOM_Array
            } else if self.is_function() {
                JS_ATOM_Function
            } else {
                match self.class_id() {
                    cid @ (JS_CLASS_STRING
                    | JS_CLASS_ARGUMENTS
                    | JS_CLASS_MAPPED_ARGUMENTS
                    | JS_CLASS_ERROR
                    | JS_CLASS_BOOLEAN
                    | JS_CLASS_NUMBER
                    | JS_CLASS_DATE
                    | JS_CLASS_REGEXP) => ctx.rt().class_defs[cid as usize].class_name,

                    _ => crate::atom::consts::JS_ATOM_Object,
                }
            };
            format!("{}", tag_atom.force_string(ctx)?)
        };

        Ok(format!("[object {tag}]"))

        // if let Some(v) = self.payload().value() {
        //     // primitive object
        //     Ok(format!("[object {}]", v.inspect(ctx)?))
        // } else {
        //     Ok(format!(
        //         "[object {}]",
        //         self.class_name()
        //             .force_string(ctx)?
        //             .content()
        //             .to_utf8(false)
        //     ))
        // }
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
