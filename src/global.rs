use crate::{
    atom::JSAtom,
    context::{JSContext, is_strict_mode},
    error::{JS_ThrowReferenceErrorUninitialized, JS_ThrowSyntaxErrorVarRedeclaration},
    function::{DEFINE_GLOBAL_FUNC_VAR, DEFINE_GLOBAL_LEX_VAR},
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE,
        JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_VALUE, JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD,
        JS_PROP_THROW, JS_PROP_THROW_STRICT, JS_PROP_TMASK, JS_PROP_WRITABLE, JSObject, JSProperty,
    },
    value::{JSValue, JSValueConst},
};

impl JSContext {
    /// construct a reference to specified global variable
    pub(crate) fn build_global_varref(
        &self,
        prop: JSAtom,
    ) -> Result<Option<(JSValue, JSValue)>, i32> {
        // no exotic behavior is possible in global_var_obj
        let gvobj = self.global_var_obj();

        let v0: JSValue = if let Some((idx, prs_flags)) = gvobj.find_shape_property(prop) {
            // check writable flag
            if prs_flags & JS_PROP_WRITABLE == 0 {
                return if self.JS_ThrowTypeErrorReadOnly(JS_PROP_THROW, prop) {
                    Err(-1)
                } else {
                    Ok(None)
                };
            }

            // XXX: should handle JS_PROP_AUTOINIT properties?
            // XXX: conformance: do these tests in OP_put_var_ref/OP_get_var_ref ?
            if let JSProperty::Normal(JSValue::UNINITIALIZED) =
                gvobj.0.prop_values.borrow().get(idx).unwrap()
            {
                JS_ThrowReferenceErrorUninitialized(self, prop);
                return Err(-1);
            }

            JSValue::Object(gvobj.clone())
        } else {
            // prop does not exist
            let gobj = self.global_obj();
            if gobj.has_property(self, prop)? {
                JSValue::Object(gobj.clone())
            } else {
                JSValue::UNDEFINED
            }
        };

        let v1 = prop.to_value(self, false);
        Ok(Some((v0, v1)))
    }
}

/// flags is 0, DEFINE_GLOBAL_LEX_VAR or DEFINE_GLOBAL_FUNC_VAR
/// XXX: could support exotic global object.
pub fn JS_CheckDefineGlobalVar(ctx: &JSContext, prop: JSAtom, flags: u32) -> i32 {
    let gobj = ctx.global_obj();

    if let Some((_, prs_flags)) = gobj.find_shape_property(prop) {
        // XXX: should handle JS_PROP_AUTOINIT
        if flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
            if prs_flags & JS_PROP_CONFIGURABLE == 0 {
                JS_ThrowSyntaxErrorVarRedeclaration(ctx, prop);
                return -1;
            }
        } else {
            if !gobj.flags().extensible() {
                ctx.JS_ThrowTypeError(&format!(
                    "cannot define variable '{}' ",
                    prop.to_str(ctx.rt())
                ));
                return -1;
            }

            if flags & DEFINE_GLOBAL_FUNC_VAR as u32 != 0 {
                if prs_flags & JS_PROP_CONFIGURABLE == 0
                    && (prs_flags & JS_PROP_TMASK == JS_PROP_GETSET
                        || prs_flags & (JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
                            != JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
                {
                    ctx.JS_ThrowTypeError(&format!(
                        "cannot define variable '{}'",
                        prop.to_str(ctx.rt())
                    ));
                    return -1;
                }
            }
        }
    }

    // check if there already is a lexical declaration
    if ctx.global_var_obj().has_shape_property(prop) {
        JS_ThrowSyntaxErrorVarRedeclaration(ctx, prop);
        return -1;
    }

    0
}

// def_flags is (0, DEFINE_GLOBAL_LEX_VAR) |
//   JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE
// XXX: could support exotic global object.
pub fn JS_DefineGlobalVar(ctx: &JSContext, prop: JSAtom, def_flags: u32) -> i32 {
    let p: JSObject;
    let val: JSValue;
    let flags: u32;

    if def_flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
        p = ctx.global_var_obj();
        flags = JS_PROP_ENUMERABLE | (def_flags & JS_PROP_WRITABLE) | JS_PROP_CONFIGURABLE;
        val = JSValue::UNINITIALIZED;
    } else {
        p = ctx.global_obj();
        flags = JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | (def_flags & JS_PROP_CONFIGURABLE);
        val = JSValue::UNDEFINED;
    }

    if p.has_shape_property(prop) {
        0
    } else if !p.flags().extensible() {
        0
    } else if p
        .add_shape_property_then(ctx, prop, flags, |pr| {
            *pr = JSProperty::Normal(val);
        })
        .is_ok()
    {
        0
    } else {
        -1
    }
}

/// 'def_flags' is 0 or JS_PROP_CONFIGURABLE.
/// `func` is not consumed.
/// XXX: could support exotic global object.
pub fn JS_DefineGlobalFunction(
    ctx: &JSContext,
    prop: JSAtom,
    func: &JSValueConst,
    def_flags: u32,
) -> i32 {
    let gobj = ctx.global_obj();

    let mut flags = JS_PROP_HAS_VALUE | JS_PROP_THROW;

    let writable = gobj
        .find_shape_property(prop)
        .map(|(_, flags)| flags & JS_PROP_CONFIGURABLE != 0)
        .unwrap_or(false);

    if writable {
        flags |= JS_PROP_ENUMERABLE
            | JS_PROP_WRITABLE
            | def_flags
            | JS_PROP_HAS_CONFIGURABLE
            | JS_PROP_HAS_WRITABLE
            | JS_PROP_HAS_ENUMERABLE;
    }

    if gobj
        .define_property(
            ctx,
            prop,
            func,
            &JSValue::UNDEFINED,
            &JSValue::UNDEFINED,
            flags,
        )
        .is_err()
    {
        return -1;
    }

    0
}

pub fn JS_GetGlobalVar(ctx: &JSContext, prop: JSAtom, throw_ref_error: bool) -> JSValue {
    // no exotic behavior is possible in global_var_obj
    let gvobj = ctx.global_var_obj();
    if let Some((idx, _)) = gvobj.find_shape_property(prop) {
        // found prop in global var object
        // XXX: should handle JS_PROP_TMASK properties
        match gvobj.0.prop_values.borrow().get(idx).unwrap() {
            JSProperty::Normal(JSValue::UNINITIALIZED) => {
                JS_ThrowReferenceErrorUninitialized(ctx, prop);
                JSValue::EXCEPTION
            }
            JSProperty::Normal(v) => v.clone(),
            _ => {
                unreachable!()
            }
        }
    } else {
        // search global object
        let gobj = ctx.global_obj();
        gobj.get_property_in_shape_or_exotic(
            ctx,
            prop,
            &JSValue::Object(gobj.clone()),
            throw_ref_error,
        )
    }
}

// construct a reference to a global variable
#[deprecated(note = "use JSContextInner::build_global_varref() instead")]
pub unsafe fn JS_GetGlobalVarRef(ctx: &JSContext, prop: JSAtom, sp: *mut JSValue) -> i32 {
    if let Ok(vs) = ctx.build_global_varref(prop) {
        if let Some((v0, v1)) = vs {
            std::ptr::replace(sp, v0);
            std::ptr::replace(sp.add(1), v1);
        }
        0
    } else {
        -1
    }
}

// use for strict variable access: test if the variable exists
pub fn JS_CheckGlobalVar(ctx: &JSContext, prop: JSAtom) -> i32 {
    // no exotic behavior is possible in global_var_obj
    let p = ctx.global_var_obj();
    if p.has_shape_property(prop) {
        1
    } else {
        if let Ok(ret) = ctx.global_obj().has_property(ctx, prop) {
            ret as i32
        } else {
            -1
        }
    }
}

// flag = 0: normal variable write
// flag = 1: initialize lexical variable
// flag = 2: normal variable write, strict check was done before
pub fn JS_SetGlobalVar(ctx: &JSContext, prop: JSAtom, val: JSValue, flag: u32) -> i32 {
    // no exotic behavior is possible in global_var_obj
    let gvobj = ctx.global_var_obj();
    if let Some((idx, prs_flags)) = gvobj.find_shape_property(prop) {
        // XXX: should handle JS_PROP_AUTOINIT properties?
        let pr = &mut gvobj.0.prop_values.borrow_mut()[idx];

        if flag != 1 {
            match pr {
                JSProperty::Normal(JSValue::UNINITIALIZED) => {
                    JS_ThrowReferenceErrorUninitialized(ctx, prop);
                    return -1;
                }
                _ => {
                    if prs_flags & JS_PROP_WRITABLE == 0 {
                        return if ctx.JS_ThrowTypeErrorReadOnly(JS_PROP_THROW, prop) {
                            -1
                        } else {
                            0
                        };
                    }
                }
            }
        }

        *pr = JSProperty::Normal(val.clone());
        return 0;
    } else {
        // not exist
        let mut flags1 = JS_PROP_THROW_STRICT;
        if is_strict_mode(ctx) {
            flags1 |= JS_PROP_NO_ADD;
        }

        let gobj = &ctx.0.borrow().global_obj;
        JSValue::set_property_internal(ctx, gobj, prop, val, gobj, flags1).map_or(-1, |b| b as i32)
    }
}
