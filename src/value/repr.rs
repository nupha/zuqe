// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS,
    JS_CLASS_NUMBER, JS_CLASS_STRING, atom::Atom, context::ContextImpl, number::f64_stringify,
    object::Object, string::StringRef,
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

            Self::String(s) => Ok(s.content().encode_utf8(true).into()),
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => Ok(format!("Symbol({})", s.content().encode_utf8(false))),
            Self::Object(o) => o.inspect(ctx),
            Self::Bytecode(b) => Ok(format!("[bytecode {}]", b.func_name.to_utf8(ctx.rt_ref()))),

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

impl Object {
    /// return object's toStringTag
    pub(crate) fn string_tag(&self, ctx: &ContextImpl) -> Result<Atom, i32> {
        let tag = if self.is_array(ctx)? {
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
                | JS_CLASS_NUMBER) => ctx.rt_ptr().class_registry[cid as usize].class_name,

                #[cfg(feature = "builtin_regexp")]
                crate::JS_CLASS_REGEXP => {
                    ctx.rt_ptr().class_registry[crate::JS_CLASS_REGEXP as usize].class_name
                }
                #[cfg(feature = "builtin_date")]
                crate::JS_CLASS_DATE => {
                    ctx.rt_ptr().class_registry[crate::JS_CLASS_DATE as usize].class_name
                }

                _ => Atom::CONST_Object,
            }
        };

        Ok(tag)
    }

    pub fn inspect(&self, ctx: &ContextImpl) -> Result<String, i32> {
        let tag: StringRef;

        #[cfg(feature = "builtin_symbol")]
        {
            tag = if let JSValue::String(s) =
                self.get_property(ctx, Atom::CONST_Symbol_toStringTag)?
            {
                s
            } else {
                self.string_tag(ctx).and_then(|a| a.force_string(ctx))?
            };
        }

        #[cfg(not(feature = "builtin_symbol"))]
        {
            tag = self.to_string_tag(ctx).and_then(|a| a.force_string(ctx))?;
        }

        Ok(format!("[object {tag}]"))
    }
}
