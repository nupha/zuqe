// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_ARRAY, JS_CLASS_BOOLEAN, JS_CLASS_ERROR,
    JS_CLASS_MAPPED_ARGUMENTS, JS_CLASS_NUMBER, JS_CLASS_STRING, atom::Atom,
    context::ScopedContext, number::f64_stringify, object::ObjectImpl, string::StringRef,
};

use super::JSValue;

impl JSValue {
    /// Get inspect representation string of `self` value.
    /// `inspect()` string is only used for interactive prompting,
    /// use `to_string()` for other cases.
    #[inline(always)]
    pub fn inspect(&self, ctx: &mut ScopedContext) -> Result<String, i32> {
        self.inspect_internal(ctx, false)
    }

    /// Get inspect representation string of `self` value.
    /// `inspect()` string is only used for interactive prompting,
    /// use `to_string()` for other cases.
    #[cfg(feature = "nan_boxing")]
    pub(crate) fn inspect_internal(
        &self,
        ctx: &mut ScopedContext,
        quoted: bool,
    ) -> Result<String, i32> {
        match self.tag() {
            crate::JSValueTag::Int => Ok(format!("{}", self.payload_i32())),
            crate::JSValueTag::Bool => Ok(if self.payload_i32() != 0 {
                "true"
            } else {
                "false"
            }
            .into()),
            crate::JSValueTag::Float64 => {
                let n = unsafe { self.as_float_unchecked() };
                Ok(if n == 0.0 {
                    // distinct '0' and '-0'
                    if n.is_sign_negative() { "-0" } else { "0" }.into()
                } else {
                    f64_stringify(n, 10, None, 0).into()
                })
            }

            crate::JSValueTag::String => {
                let s = unsafe { self.as_string_unchecked() };
                if quoted {
                    Ok(format!("\"{}\"", s.content().encode_utf8(true)))
                } else {
                    Ok(s.content().encode_utf8(false).into())
                }
            }
            #[cfg(feature = "builtin_symbol")]
            crate::JSValueTag::Symbol => {
                let s = unsafe { self.as_string_unchecked() };
                Ok(format!("Symbol({})", s.content().encode_utf8(false)))
            }

            crate::JSValueTag::Object => unsafe { self.as_object_unchecked().inspect(ctx) },
            crate::JSValueTag::FunctionBytecode => unsafe {
                Ok(format!(
                    "[bytecode {}]",
                    self.as_bytecode_unchecked().func_name.to_utf8(ctx.rt())
                ))
            },

            crate::JSValueTag::Null => Ok("null".into()),
            crate::JSValueTag::Undefined => Ok("undefined".into()),
            crate::JSValueTag::Module => Ok("[module]".into()),

            // internal value types
            crate::JSValueTag::Exception => Ok("<exception>".into()),
            crate::JSValueTag::Uninitialized => Ok("<uninitialized>".into()),
            crate::JSValueTag::CatchOffset => Ok(format!("<catchoffset {}>", self.payload_i32())),

            crate::JSValueTag::StringRope => unreachable!("not used"),
        }
    }

    #[cfg(not(feature = "nan_boxing"))]
    pub(crate) fn inspect_internal(
        &self,
        ctx: &mut ScopedContext,
        quoted: bool,
    ) -> Result<String, i32> {
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

            Self::String(s) => {
                if quoted {
                    Ok(format!("\"{}\"", s.content().encode_utf8(true)))
                } else {
                    Ok(s.content().encode_utf8(false).into())
                }
            }
            #[cfg(feature = "builtin_symbol")]
            Self::Symbol(s) => Ok(format!("Symbol({})", s.content().encode_utf8(false))),
            Self::Object(o) => o.inspect(ctx),
            Self::Bytecode(b) => Ok(format!("[bytecode {}]", b.func_name.to_utf8(ctx.rt()))),

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

impl ObjectImpl {
    /// Represent object in "[object <Tag>]" format
    pub(crate) fn repr(&self, ctx: &mut ScopedContext) -> Result<String, i32> {
        let atom = if self.is_array(ctx)? {
            Atom::CONST_Array
        } else if self.is_callable() {
            Atom::CONST_Function
        } else {
            match self.class_id() {
                cid @ (JS_CLASS_STRING
                | JS_CLASS_ARGUMENTS
                | JS_CLASS_MAPPED_ARGUMENTS
                | JS_CLASS_ERROR
                | JS_CLASS_BOOLEAN
                | JS_CLASS_NUMBER) => ctx.rt().class_registry[cid as usize].name_atom,

                #[cfg(feature = "builtin_regexp")]
                crate::JS_CLASS_REGEXP => {
                    ctx.rt().class_registry[crate::JS_CLASS_REGEXP as usize].name_atom
                }
                #[cfg(feature = "builtin_date")]
                crate::JS_CLASS_DATE => {
                    ctx.rt().class_registry[crate::JS_CLASS_DATE as usize].name_atom
                }

                _ => Atom::CONST_Object,
            }
        };

        let tag: StringRef = {
            #[cfg(feature = "builtin_symbol")]
            if let Some(s) = self
                .get_property(ctx, Atom::CONST_Symbol_toStringTag)?
                .as_string()
            {
                s
            } else {
                atom.force_string(ctx)?
            }
            #[cfg(not(feature = "builtin_symbol"))]
            atom.force_string(ctx)?
        };

        Ok(format!("[object {tag}]"))
    }

    /// Print object.
    ///
    /// Now simply output "Array(n)" or "ClassName { }" format.
    /// TODO: should show detailed info inside later.
    pub fn inspect(&self, ctx: &mut ScopedContext) -> Result<String, i32> {
        match self.class_id() {
            JS_CLASS_ARRAY => Ok(format!("Array({})", self.payload().array().unwrap().len())),
            class_id => {
                let atom = ctx.rt().class_registry[class_id as usize].name_atom;
                let name = atom.force_string(ctx)?;

                if let Some(ta) = self.payload().typed_array() {
                    Ok(format!("{}({})", name.encode_utf8(false), ta.count()))
                } else {
                    Ok(format!("{} {{ }}", name.encode_utf8(false)))
                }
            }
        }
    }
}
