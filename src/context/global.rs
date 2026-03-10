// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    atom::Atom,
    context::ScopedContext,
    error::{JS_ThrowReferenceErrorUninitialized, JS_ThrowSyntaxErrorVarRedeclaration, JSResult},
    function::{DEFINE_GLOBAL_FUNC_VAR, DEFINE_GLOBAL_LEX_VAR},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD,
        JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_TMASK, JS_PROP_WRITABLE, JSProperty,
    },
    value::JSValue,
};

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
                self.throw_type_error_read_only(prop);
                return Err(-1);
            }

            // XXX: should handle JS_PROP_LAZYINIT properties?
            // XXX: conformance: do these tests in OP_put_var_ref/OP_get_var_ref ?
            if let JSProperty::Normal(JSValue::UNINITIALIZED) = gvobj.prop_slot(idx) {
                JS_ThrowReferenceErrorUninitialized(self, prop);
                return Err(-1);
            }

            JSValue::Object(gvobj)
        } else {
            // prop does not exist
            let gobj = self.global_obj();
            if gobj.has_property(self, prop)? {
                JSValue::Object(gobj)
            } else {
                JSValue::UNDEFINED
            }
        };

        let v1 = prop.to_value(self)?;
        Ok(Some((v0, v1)))
    }

    /// 'def_flags' is 0 or JS_PROP_CONFIGURABLE.
    /// XXX: could support exotic global object.
    pub(crate) fn define_global_function(
        &mut self,
        prop: Atom,
        func: JSValue,
        def_flags: u32,
    ) -> Result<bool, i32> {
        let mut gobj = self.global_obj();

        let mut flags = JS_PROP_HAS_VALUE | JS_PROP_THROW;
        let configurable = gobj
            .find_shape_property(prop)
            .map(|(_, flags)| flags & JS_PROP_CONFIGURABLE != 0)
            .unwrap_or(true);
        if configurable {
            flags |= JS_PROP_ENUMERABLE
                | JS_PROP_WRITABLE
                | def_flags
                | JS_PROP_HAS_CONFIGURABLE
                | JS_PROP_HAS_WRITABLE
                | JS_PROP_HAS_ENUMERABLE;
        }

        gobj.define_property(
            self,
            prop,
            func,
            JSValue::UNDEFINED,
            JSValue::UNDEFINED,
            flags,
        )
    }

    /// def_flags is (0, DEFINE_GLOBAL_LEX_VAR) |
    ///   JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE
    // XXX: could support exotic global object.
    pub(crate) fn define_global_var(&mut self, prop: Atom, def_flags: u32) -> Result<(), i32> {
        let (mut ob, flags, val) = if def_flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
            (
                self.global_var_obj(),
                JS_PROP_ENUMERABLE | (def_flags & JS_PROP_WRITABLE) | JS_PROP_CONFIGURABLE,
                JSValue::UNINITIALIZED,
            )
        } else {
            (
                self.global_obj(),
                JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | (def_flags & JS_PROP_CONFIGURABLE),
                JSValue::UNDEFINED,
            )
        };

        if !ob.flags().extensible() || ob.has_shape_property(prop) {
            Ok(())
        } else {
            ob.add_own_property(self, prop, flags, JSProperty::Normal(val))
        }
    }

    /// flags is 0, DEFINE_GLOBAL_LEX_VAR or DEFINE_GLOBAL_FUNC_VAR
    /// XXX: could support exotic global object.
    pub(crate) fn define_global_var_checked(&mut self, prop: Atom, flags: u32) -> Result<(), i32> {
        let gobj = self.global_obj();
        let prs = gobj.find_shape_property(prop).map(|(_, flags)| flags);

        // XXX: should handle JS_PROP_LAZYINIT
        if flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
            if prs.is_some_and(|f| f & JS_PROP_CONFIGURABLE == 0) {
                JS_ThrowSyntaxErrorVarRedeclaration(self, prop);
                return Err(-1);
            }
        } else {
            if prs.is_none() && !gobj.flags().extensible() {
                self.throw_type_error(&format!(
                    "cannot define variable '{}' ",
                    prop.to_utf8(self.rt_ref())
                ));
                return Err(-1);
            }

            if flags & DEFINE_GLOBAL_FUNC_VAR as u32 != 0
                && let Some(prs_flags) = prs
                && prs_flags & JS_PROP_CONFIGURABLE == 0
                && (prs_flags & JS_PROP_TMASK == JS_PROP_GETSET
                    || prs_flags & (JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
                        != JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
            {
                self.throw_type_error(&format!(
                    "cannot define variable '{}'",
                    prop.to_utf8(self.rt_ref())
                ));
                return Err(-1);
            }
        }

        // check if there already is a lexical declaration
        if self.global_var_obj().has_shape_property(prop) {
            JS_ThrowSyntaxErrorVarRedeclaration(self, prop);
            return Err(-1);
        }

        Ok(())
    }

    pub(crate) fn get_global_var(&mut self, prop: Atom, throw_ref_error: bool) -> JSResult {
        // no exotic behavior is possible in global_var_obj
        let gvobj = self.global_var_obj();

        if let Some((idx, _)) = gvobj.find_shape_property(prop) {
            // found in global var object
            // XXX: should handle JS_PROP_TMASK properties
            match gvobj.prop_slot(idx) {
                JSProperty::Normal(JSValue::UNINITIALIZED) => {
                    JS_ThrowReferenceErrorUninitialized(self, prop);
                    Err(-1)
                }
                JSProperty::Normal(v) => Ok(*v),
                _ => {
                    unreachable!()
                }
            }
        } else {
            // search global object
            let gobj = self.global_obj();
            gobj.get_property_internal(self, prop, JSValue::Object(gobj), throw_ref_error)
        }
    }

    /// used for strict variable access: test if the variable exists
    pub(crate) fn check_global_var(&mut self, prop: Atom) -> Result<bool, i32> {
        // no exotic behavior is possible in global_var_obj
        let p = self.global_var_obj();
        if p.has_shape_property(prop) {
            Ok(true)
        } else {
            self.global_obj().has_property(self, prop)
        }
    }

    /// flag:
    /// * 0: normal variable write
    /// * 1: initialize lexical variable
    /// * 2: normal variable write, strict check was done before
    pub(crate) fn set_global_var(
        &mut self,
        prop: Atom,
        val: JSValue,
        flag: u8,
    ) -> Result<bool, i32> {
        // no exotic behavior is possible in global_var_obj
        let mut gv = self.global_var_obj();

        if let Some((idx, prs_flags)) = gv.find_shape_property(prop) {
            // XXX: should handle JS_PROP_LAZYINIT properties?
            if flag != 1 {
                match gv.prop_slot(idx) {
                    JSProperty::Normal(JSValue::UNINITIALIZED) => {
                        JS_ThrowReferenceErrorUninitialized(self, prop);
                        return Err(-1);
                    }
                    _ => {
                        if prs_flags & JS_PROP_WRITABLE == 0 {
                            self.throw_type_error_read_only(prop);
                            return Err(-1);
                        }
                    }
                }
            }

            gv.set_prop_slot(idx, JSProperty::Normal(val));
            Ok(true)
        } else {
            // not exist
            let mut flags1 = JS_PROP_THROW_STRICT;
            if self.rt_ref().is_strict_mode() {
                flags1 |= JS_PROP_NO_ADD;
            }

            let gobj = self.global_obj();
            gobj.set_property_internal(self, prop, val, Some(gobj), flags1)
        }
    }
}
