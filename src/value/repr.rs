use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN, JS_CLASS_DATE, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS,
    JS_CLASS_NUMBER, JS_CLASS_REGEXP, JS_CLASS_STRING, atom::Atom, context::ContextImpl,
    number::f64_stringify, object::ObjectRef,
};

use super::JSValue;

impl JSValue {
    /// Get inspect representation string of `self` value.
    /// `inspect()` string is only used for interactive prompting,
    /// use `to_string()` for other cases.
    pub fn inspect(&self, ctx: &ContextImpl) -> Result<String, i32> {
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
            Self::Symbol(a) => Ok(format!("Symbol({})", a.string().content().to_utf8(false))),
            Self::Object(o) => o.inspect(ctx),
            Self::Bytecode(b) => Ok(format!("[bytecode {}]", b.func_name.to_utf8())),

            Self::NULL => Ok("null".into()),
            Self::UNDEFINED => Ok("undefined".into()),
            Self::Module(_) => Ok("[module]".into()),

            // internal value types
            Self::EXCEPTION => Ok("<exception>".into()),
            Self::UNINITIALIZED => Ok("<uninitialized>".into()),
            Self::CatchOffset(n) => Ok(format!("<catchoffset {}>", *n)),
        }
    }
}

impl ObjectRef {
    pub fn inspect(&self, ctx: &ContextImpl) -> Result<String, i32> {
        let tag_atom: Atom = if self.is_array(ctx)? {
            Atom::CONST_Array
        } else if self.is_function() {
            Atom::CONST_Function
        } else {
            match self.class_id() {
                cid @ (JS_CLASS_STRING
                | JS_CLASS_ARGUMENTS
                | JS_CLASS_MAPPED_ARGUMENTS
                | JS_CLASS_ERROR
                | JS_CLASS_BOOLEAN
                | JS_CLASS_NUMBER
                | JS_CLASS_DATE
                | JS_CLASS_REGEXP) => ctx.rt().class_registry[cid as usize].class_name,

                _ => Atom::CONST_Object,
            }
        };

        // get [Symbol.toStringTag] property
        let tag =
            if let JSValue::String(s) = self.get_property(ctx, Atom::CONST_Symbol_toStringTag)? {
                s
            } else {
                tag_atom.force_string(ctx)?
            };

        Ok(format!("[object {tag}]"))
    }
}
