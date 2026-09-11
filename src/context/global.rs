// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::{JS_ThrowReferenceErrorUninitialized, JS_ThrowSyntaxErrorVarRedeclaration, JSResult},
    function::{DEFINE_GLOBAL_FUNC_VAR, DEFINE_GLOBAL_LEX_VAR},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_TMASK, JS_PROP_WRITABLE,
        JSProperty, ThrowPolicy,
    },
    value::JSValue,
};

/// How [`ScopedContext::set_global_var`] writes a global binding.
///
/// Replaces the former raw `flag: u8` argument (`0`/`1`/`2`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GlobalVarSetMode {
    /// Ordinary variable write: TDZ and read-only checks apply.
    Normal,
    /// Initialize a lexical binding (its `let`/`const` declarator). The binding
    /// is being defined, so the TDZ / read-only checks are skipped.
    Init,
    /// Ordinary variable write whose strict-mode checks were already performed
    /// by the caller.
    #[allow(dead_code)] // no caller emits this mode yet; kept for API parity
    Checked,
}

impl<'c> ScopedContext<'c> {
    /// construct a reference to specified global variable
    pub(crate) fn build_global_varref(
        &mut self,
        prop: Atom,
    ) -> Result<Option<(JSValue, JSValue)>, i32> {
        // no exotic behavior is possible in global_var_obj
        let gvobj = self.global_var_obj();

        let v0: JSValue = if let Some((idx, prs_flags)) = gvobj.find_shape_property(prop) {
            // check writable flag
            if prs_flags & JS_PROP_WRITABLE == 0 {
                return Err(self.throw_type_error_read_only(prop));
            }

            // XXX: should handle JS_PROP_LAZYINIT properties?
            // XXX: conformance: do these tests in OP_put_var_ref/OP_get_var_ref ?
            if let JSProperty::Normal(v) = gvobj.prop_slot(idx)
                && JSValue::UNINITIALIZED.eq(v)
            {
                return Err(JS_ThrowReferenceErrorUninitialized(self, prop));
            }

            JSValue::from(gvobj)
        } else {
            // prop does not exist
            let gobj = self.global_obj();
            if gobj.has_property(self, prop)? {
                JSValue::from(gobj)
            } else {
                JSValue::UNDEFINED
            }
        };

        let v1 = prop.to_value(self)?;
        Ok(Some((v0, v1)))
    }

    /// Define a global variable property.
    ///
    /// `def_flags` semantics:
    /// - `DEFINE_GLOBAL_LEX_VAR` (bit 7): lexical declaration (let/const)
    /// - `JS_PROP_WRITABLE` (bit 1): writable (for lexical: let=writable, const=readonly)
    /// - `JS_PROP_CONFIGURABLE` (bit 0): configurable (for var: eval=configurable, script=non-configurable)
    pub(crate) fn define_global_var(&mut self, prop: Atom, def_flags: u32) -> Result<(), i32> {
        if def_flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
            // Lexical declaration (let/const): property goes on global_var_obj.
            let mut gvobj = self.global_var_obj();
            let gv_flags =
                JS_PROP_ENUMERABLE | JS_PROP_CONFIGURABLE | (def_flags & JS_PROP_WRITABLE);

            // Check if already declared on global_var_obj (e.g. double `let x; let x;`).
            if gvobj.has_shape_property(prop) {
                return Err(JS_ThrowSyntaxErrorVarRedeclaration(self, prop));
            }

            // Check for non-configurable property on global_obj.
            // e.g. `let NaN` or `let undefined` → SyntaxError.
            let gobj = self.global_obj();
            if let Some((_, prs_flags)) = gobj.find_shape_property(prop)
                && prs_flags & JS_PROP_CONFIGURABLE == 0
            {
                return Err(JS_ThrowSyntaxErrorVarRedeclaration(self, prop));
            }

            gvobj.add_own_property(
                self,
                prop,
                gv_flags,
                JSProperty::Normal(JSValue::UNINITIALIZED),
            )
        } else {
            // Var / function declaration: property goes on global_obj.
            let mut gobj = self.global_obj();
            let gv_flags =
                JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | (def_flags & JS_PROP_CONFIGURABLE);

            if gobj.has_shape_property(prop) {
                // Property already exists on global_obj → no-op (QuickJS creates a VarRef
                // but doesn't modify property flags for an existing property).
                Ok(())
            } else if !gobj.flags().extensible() {
                // Non-extensible global object: throw TypeError (CanDeclareGlobalVar).
                Err(self.throw_type_error(&format!(
                    "cannot define variable '{}'",
                    prop.to_utf8(self.rt())
                )))
            } else {
                // Check for existing lexical declaration on global_var_obj.
                // e.g. `let x; var x;` → SyntaxError.
                if self.global_var_obj().has_shape_property(prop) {
                    return Err(JS_ThrowSyntaxErrorVarRedeclaration(self, prop));
                }
                gobj.add_own_property(self, prop, gv_flags, JSProperty::Normal(JSValue::UNDEFINED))
            }
        }
    }

    /// Pre-validation for global variable declarations.
    ///
    /// All checks are done BEFORE any bindings are created,
    /// so that partial bindings don't leak on failure.
    ///
    /// `flags` is 0 (var), `DEFINE_GLOBAL_LEX_VAR` (let/const), or
    /// `DEFINE_GLOBAL_LEX_VAR | DEFINE_GLOBAL_FUNC_VAR` (function decl).
    ///
    /// Returns `Ok(())` if the declaration is valid, `Err(-1)` with an exception
    /// set otherwise.
    pub(crate) fn define_global_var_checked(&mut self, prop: Atom, flags: u32) -> Result<(), i32> {
        let gobj = self.global_obj();
        let prs = gobj
            .find_shape_property(prop)
            .map(|(_, prs_flags)| prs_flags);

        if flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
            // HasRestrictedGlobalProperty check: non-configurable global → SyntaxError
            if prs.is_some_and(|f| f & JS_PROP_CONFIGURABLE == 0) {
                return Err(JS_ThrowSyntaxErrorVarRedeclaration(self, prop));
            }
        } else {
            // CanDeclareGlobalVar check: non-extensible + no existing prop → TypeError
            if prs.is_none() && !gobj.flags().extensible() {
                return Err(self.throw_type_error(&format!(
                    "cannot define variable '{}'",
                    prop.to_utf8(self.rt())
                )));
            }

            // CanDeclareGlobalFunction check (DEFINE_GLOBAL_FUNC_VAR)
            if flags & DEFINE_GLOBAL_FUNC_VAR as u32 != 0
                && let Some(prs_flags) = prs
            {
                // Property exists and is non-configurable:
                //   → only writable + enumerable data properties are OK.
                if prs_flags & JS_PROP_CONFIGURABLE == 0
                    && (prs_flags & JS_PROP_TMASK == JS_PROP_GETSET
                        || prs_flags & (JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
                            != (JS_PROP_WRITABLE | JS_PROP_ENUMERABLE))
                {
                    return Err(self.throw_type_error(&format!(
                        "cannot define variable '{}'",
                        prop.to_utf8(self.rt())
                    )));
                }
            }
        }

        // HasLexicalDeclaration check: already declared on global_var_obj → SyntaxError
        if self.global_var_obj().has_shape_property(prop) {
            return Err(JS_ThrowSyntaxErrorVarRedeclaration(self, prop));
        }

        Ok(())
    }

    pub(crate) fn get_global_var(&mut self, prop: Atom, throw: bool) -> JSResult {
        // no exotic behavior is possible in global_var_obj
        let gvobj = self.global_var_obj();

        if let Some((idx, _)) = gvobj.find_shape_property(prop) {
            // found in global var object
            // XXX: should handle JS_PROP_TMASK properties
            match gvobj.prop_slot(idx) {
                JSProperty::Normal(v) => {
                    if JSValue::UNINITIALIZED.eq(v) {
                        Err(JS_ThrowReferenceErrorUninitialized(self, prop))
                    } else {
                        Ok(*v)
                    }
                }
                _ => {
                    unreachable!()
                }
            }
        } else {
            // search global object
            let gobj = self.global_obj();
            gobj.get_property_internal(self, prop, JSValue::from(gobj), throw)
        }
    }

    /// Write a global variable according to `mode`.
    pub(crate) fn set_global_var(
        &mut self,
        prop: Atom,
        val: JSValue,
        mode: GlobalVarSetMode,
    ) -> Result<bool, i32> {
        // no exotic behavior is possible in global_var_obj
        let mut gv = self.global_var_obj();

        if let Some((idx, prs_flags)) = gv.find_shape_property(prop) {
            // XXX: should handle JS_PROP_LAZYINIT properties?
            if mode != GlobalVarSetMode::Init {
                match gv.prop_slot(idx) {
                    JSProperty::Normal(v) if JSValue::UNINITIALIZED.eq(v) => {
                        return Err(JS_ThrowReferenceErrorUninitialized(self, prop));
                    }
                    _ => {
                        if prs_flags & JS_PROP_WRITABLE == 0 {
                            return Err(self.throw_type_error_read_only(prop));
                        }
                    }
                }
            }

            gv.set_prop_slot(idx, JSProperty::Normal(val));
            Ok(true)
        } else {
            // not exist
            let no_add = self.rt().is_strict_mode();
            let gobj = self.global_obj();
            gobj.set_property_internal(self, prop, val, Some(gobj), ThrowPolicy::StrictOnly, no_add)
        }
    }
}
