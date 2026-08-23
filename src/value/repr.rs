// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARGUMENTS, JS_CLASS_BOOLEAN, JS_CLASS_ERROR, JS_CLASS_MAPPED_ARGUMENTS,
    JS_CLASS_NUMBER, JS_CLASS_STRING, atom::Atom, context::ScopedContext, object::ObjectImpl,
    string::StringRef,
};

impl ObjectImpl {
    /// Represent object in "[object <Tag>]" format
    pub fn repr(&self, ctx: &mut ScopedContext) -> Result<String, i32> {
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
                // NOTE: BigInt wrappers are deliberately absent: per spec they
                // have no builtin tag; "[object BigInt]" comes from the
                // prototype's own @@toStringTag, so non-string overrides fall
                // back to "Object".
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
            let val = self.get_property(ctx, Atom::CONST_Symbol_toStringTag)?;

            // Per ES spec, the `@@toStringTag` value is only honored when it
            // is a *string primitive*. A String object (e.g. `new String(t)`)
            // must be ignored and the builtin tag used instead. `as_string(false)`
            // does not unwrap String objects, so it only matches primitives.
            if let Some(s) = val.as_string(false) {
                s
            } else {
                atom.force_string(ctx)?
            }
        };

        Ok(format!("[object {tag}]"))
    }
}
