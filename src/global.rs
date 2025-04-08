use std::ffi::CStr;

use crate::{
    atom::JSAtom,
    context::{is_strict_mode, JSContext},
    error::{JS_ThrowReferenceErrorUninitialized, JS_ThrowSyntaxErrorVarRedeclaration},
    function::{DEFINE_GLOBAL_FUNC_VAR, DEFINE_GLOBAL_LEX_VAR},
    types::unlikely,
    value::{
        object::{
            find_own_property, JSObject, JSProperty, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE,
            JS_PROP_GETSET, JS_PROP_HAS_CONFIGURABLE, JS_PROP_HAS_ENUMERABLE, JS_PROP_HAS_VALUE,
            JS_PROP_HAS_WRITABLE, JS_PROP_NO_ADD, JS_PROP_THROW, JS_PROP_THROW_STRICT,
            JS_PROP_TMASK, JS_PROP_WRITABLE,
        },
        set_value, JSValue, JSValueConst, JS_FreeValue, JS_IsUninitialized, JS_VALUE_GET_OBJ,
    },
};

// flags is 0, DEFINE_GLOBAL_LEX_VAR or DEFINE_GLOBAL_FUNC_VAR
// XXX: could support exotic global object.
pub unsafe fn JS_CheckDefineGlobalVar(ctx: *mut JSContext, prop: JSAtom, flags: u32) -> i32 {
    let p = JS_VALUE_GET_OBJ((*ctx).global_obj);

    if let Some(prs) = (*p).find_own_property1(prop) {
        let prs_flags = prs.flags();

        // XXX: should handle JS_PROP_AUTOINIT
        if flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
            if prs_flags & JS_PROP_CONFIGURABLE == 0 {
                JS_ThrowSyntaxErrorVarRedeclaration(ctx, prop);
                return -1;
            }
        } else {
            if !(*p).detailed.extensible() {
                (*ctx).JS_ThrowTypeError(CStr::from_ptr(
                    format!("cannot define variable '{}'\0", prop.to_str((*ctx).rt()))
                        .as_ptr()
                        .cast(),
                ));
                return -1;
            }

            if flags & DEFINE_GLOBAL_FUNC_VAR as u32 != 0 {
                if prs_flags & JS_PROP_CONFIGURABLE == 0
                    && (prs_flags & JS_PROP_TMASK == JS_PROP_GETSET
                        || prs_flags & (JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
                            != JS_PROP_WRITABLE | JS_PROP_ENUMERABLE)
                {
                    (*ctx).JS_ThrowTypeError(CStr::from_ptr(
                        format!("cannot define variable '{}'\0", prop.to_str((*ctx).rt()))
                            .as_ptr()
                            .cast(),
                    ));
                    return -1;
                }
            }
        }
    }

    // check if there already is a lexical declaration
    let p = JS_VALUE_GET_OBJ((*ctx).global_var_obj);
    if (*p).find_own_property1(prop).is_some() {
        JS_ThrowSyntaxErrorVarRedeclaration(ctx, prop);
        return -1;
    }

    0
}

// def_flags is (0, DEFINE_GLOBAL_LEX_VAR) |
//   JS_PROP_CONFIGURABLE | JS_PROP_WRITABLE
// XXX: could support exotic global object.
pub unsafe fn JS_DefineGlobalVar(ctx: *mut JSContext, prop: JSAtom, def_flags: u32) -> i32 {
    let p: *mut JSObject;
    let val: JSValue;
    let flags: u32;

    if def_flags & DEFINE_GLOBAL_LEX_VAR as u32 != 0 {
        p = JS_VALUE_GET_OBJ((*ctx).global_var_obj);
        flags = JS_PROP_ENUMERABLE | (def_flags & JS_PROP_WRITABLE) | JS_PROP_CONFIGURABLE;
        val = JSValue::UNINITIALIZED;
    } else {
        p = JS_VALUE_GET_OBJ((*ctx).global_obj);
        flags = JS_PROP_ENUMERABLE | JS_PROP_WRITABLE | (def_flags & JS_PROP_CONFIGURABLE);
        val = JSValue::UNDEFINED;
    }

    if (*p).find_own_property1(prop).is_some() {
        return 0;
    } else if !(*p).detailed.extensible() {
        return 0;
    } else if let Ok(pr) = (*p).add_property(ctx.as_mut().unwrap(), prop, flags) {
        (*pr).u.value = val;
        0
    } else {
        -1
    }
}

// 'def_flags' is 0 or JS_PROP_CONFIGURABLE.
// XXX: could support exotic global object.
pub unsafe fn JS_DefineGlobalFunction(
    ctx: *mut JSContext,
    prop: JSAtom,
    func: JSValueConst,
    def_flags: u32,
) -> i32 {
    let p = JS_VALUE_GET_OBJ((*ctx).global_obj);

    let mut flags = JS_PROP_HAS_VALUE | JS_PROP_THROW;

    if (*p).find_own_property1(prop).map_or(
        true,                                          // not exists
        |prs| prs.flags() & JS_PROP_CONFIGURABLE != 0, // writable
    ) {
        flags |= JS_PROP_ENUMERABLE
            | JS_PROP_WRITABLE
            | def_flags
            | JS_PROP_HAS_CONFIGURABLE
            | JS_PROP_HAS_WRITABLE
            | JS_PROP_HAS_ENUMERABLE;
    }

    if (*ctx)
        .global_obj
        .define_property(
            ctx.as_mut().unwrap(),
            prop,
            func,
            JSValue::UNDEFINED,
            JSValue::UNDEFINED,
            flags,
        )
        .is_err()
    {
        return -1;
    }

    0
}

pub unsafe fn JS_GetGlobalVar(ctx: *mut JSContext, prop: JSAtom, throw_ref_error: bool) -> JSValue {
    // no exotic behavior is possible in global_var_obj
    let p = JS_VALUE_GET_OBJ((*ctx).global_var_obj);

    let mut pr = std::ptr::null_mut::<JSProperty>();
    let prs = find_own_property(&mut pr, p, prop);
    if !prs.is_null() {
        // XXX: should handle JS_PROP_TMASK properties
        if unlikely(JS_IsUninitialized((*pr).u.value)) {
            return JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
        }
        return (*pr).u.value.dup();
    }

    (*ctx).global_obj.get_property_internal(
        ctx.as_mut().unwrap(),
        prop,
        (*ctx).global_obj,
        throw_ref_error,
    )
}

// construct a reference to a global variable
pub unsafe fn JS_GetGlobalVarRef(ctx: *mut JSContext, prop: JSAtom, sp: *mut JSValue) -> i32 {
    // no exotic behavior is possible in global_var_obj
    let p = JS_VALUE_GET_OBJ((*ctx).global_var_obj);

    let mut pr = std::ptr::null_mut::<JSProperty>();
    let prs = find_own_property(&mut pr, p, prop);
    if !prs.is_null() {
        // XXX: should handle JS_PROP_AUTOINIT properties?
        // XXX: conformance: do these tests in
        //           OP_put_var_ref/OP_get_var_ref ?
        if unlikely(JS_IsUninitialized((*pr).u.value)) {
            JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
            return -1;
        }
        if unlikely((*prs).flags() & JS_PROP_WRITABLE == 0) {
            return if (*ctx).JS_ThrowTypeErrorReadOnly(JS_PROP_THROW, prop) {
                0
            } else {
                -1
            };
        }
        *sp.offset(0) = (*ctx).global_var_obj.dup();
    } else {
        if let Ok(ret) = (*ctx).global_obj.has_property(ctx.as_mut().unwrap(), prop) {
            *sp.offset(0) = if ret {
                (*ctx).global_obj.dup()
            } else {
                JSValue::UNDEFINED
            };
        } else {
            return -1;
        }
    }

    *sp.offset(1) = prop.to_value(ctx.as_mut().unwrap(), false);
    0
}

// use for strict variable access: test if the variable exists
pub unsafe fn JS_CheckGlobalVar(ctx: *mut JSContext, prop: JSAtom) -> i32 {
    // no exotic behavior is possible in global_var_obj
    let p = JS_VALUE_GET_OBJ((*ctx).global_var_obj);
    if (*p).find_own_property1(prop).is_some() {
        1
    } else {
        if let Ok(ret) = (*ctx).global_obj.has_property(ctx.as_mut().unwrap(), prop) {
            ret as i32
        } else {
            -1
        }
    }
}

// flag = 0: normal variable write
// flag = 1: initialize lexical variable
// flag = 2: normal variable write, strict check was done before
pub unsafe fn JS_SetGlobalVar(ctx: *mut JSContext, prop: JSAtom, val: JSValue, flag: u32) -> i32 {
    // no exotic behavior is possible in global_var_obj
    let p = JS_VALUE_GET_OBJ((*ctx).global_var_obj);
    let mut pr = std::ptr::null_mut();
    let prs = find_own_property(&mut pr, p, prop);

    if !prs.is_null() {
        // XXX: should handle JS_PROP_AUTOINIT properties?
        if flag != 1 {
            if unlikely(JS_IsUninitialized((*pr).u.value)) {
                JS_FreeValue(ctx, val);
                JS_ThrowReferenceErrorUninitialized(ctx, (*prs).atom);
                return -1;
            }
            if unlikely((*prs).flags() & JS_PROP_WRITABLE == 0) {
                JS_FreeValue(ctx, val);
                return if (*ctx).JS_ThrowTypeErrorReadOnly(JS_PROP_THROW, prop) {
                    0
                } else {
                    -1
                };
            }
        }
        set_value(ctx, &mut (*pr).u.value, val);
        return 0;
    }

    let mut flags1 = JS_PROP_THROW_STRICT;
    if is_strict_mode(ctx) {
        flags1 |= JS_PROP_NO_ADD;
    }

    (*ctx)
        .global_obj
        .set_property_internal(ctx.as_mut().unwrap(), prop, val, (*ctx).global_obj, flags1)
        .map_or(-1, |b| b as i32)
}
