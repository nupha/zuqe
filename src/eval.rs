use std::{ffi::CStr, ptr::NonNull};

use crate::{
    atom::{consts::JS_ATOM__eval_, JS_DupAtom},
    context::{JSContext, JS_MODE_STRICT, JS_MODE_STRIP},
    function::{
        add_closure_variables, js_class_has_bytecode, js_closure, js_create_function,
        js_free_function_def, js_new_function_def, JSFunctionBytecode, JSFunctionKindEnum,
        JSVarRef, JS_Call, JS_CallFree,
    },
    module::{
        js_create_module_function, js_free_module_def, js_inner_module_evaluation, js_link_module,
        js_resolve_module, JSModuleDef, JSModuleStatus, JS_NewModuleValue,
    },
    parse::JSParseState,
    stack_frame::JSStackFrame,
    unicode::{unicode_from_utf8, UTF8_CHAR_LEN_MAX},
    value::{
        JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, JS_TAG_FUNCTION_BYTECODE,
        JS_TAG_MODULE, JS_TAG_OBJECT, JS_UNDEFINED, JS_VALUE_GET_PTR, JS_VALUE_GET_TAG,
    },
};

// Eval flags
pub const JS_EVAL_TYPE_GLOBAL: u32 = 0 << 0;
pub const JS_EVAL_TYPE_MODULE: u32 = 1 << 0;
pub const JS_EVAL_TYPE_DIRECT: u32 = 2 << 0;
pub const JS_EVAL_TYPE_INDIRECT: u32 = 3 << 0;
pub const JS_EVAL_TYPE_MASK: u32 = 3 << 0;
pub const JS_EVAL_FLAG_STRICT: u32 = 1 << 3;
pub const JS_EVAL_FLAG_STRIP: u32 = 1 << 4;
pub const JS_EVAL_FLAG_COMPILE_ONLY: u32 = 1 << 5;
pub const JS_EVAL_FLAG_BACKTRACE_BARRIER: u32 = 1 << 6;
pub const JS_EVAL_FLAG_ASYNC: u32 = 1 << 7;

// 'input' must be zero terminated i.e. input[input_len] = '\0'.
pub unsafe fn __JS_EvalInternal(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    input: *const u8,
    input_len: usize,
    filename: &'static CStr,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    let mut s = JSParseState::new(ctx, input, input_len, filename);

    macro_rules! fail1 {
        ($m:expr) => {
            /* XXX: should free all the unresolved dependencies */
            if !$m.is_null() {
                js_free_module_def(ctx, $m);
            }
            return JSValue::EXCEPTION;
        };
    }
    macro_rules! fail {
        ($fd:expr, $m:expr) => {
            s.free_token();
            js_free_function_def(ctx, $fd);
            fail1!($m);
        };
    }

    skip_shebang(&mut s.buf_ptr, s.buf_end);

    let eval_type = flags & JS_EVAL_TYPE_MASK;
    let mut m: *mut JSModuleDef = std::ptr::null_mut();
    let mut b: *mut JSFunctionBytecode = std::ptr::null_mut();
    let mut sf: *mut JSStackFrame = std::ptr::null_mut();
    let mut var_refs: &[NonNull<JSVarRef>] = &[];
    let mut js_mode: u32 = 0;

    if eval_type == JS_EVAL_TYPE_DIRECT {
        sf = (*ctx).rt().current_stack_frame;
        debug_assert!(!sf.is_null());
        debug_assert!(JS_VALUE_GET_TAG((*sf).cur_func) == JS_TAG_OBJECT);

        let ob = (*sf).cur_func.object();
        debug_assert!(js_class_has_bytecode(ob.class_id()));

        b = ob.u.func.function_bytecode;
        var_refs = (*ob.u.func).var_refs.as_slice();
        js_mode = (*b).js_mode as _;
    } else {
        if flags & JS_EVAL_FLAG_STRICT != 0 {
            js_mode |= JS_MODE_STRICT as u32;
        }
        if flags & JS_EVAL_FLAG_STRIP != 0 {
            js_mode |= JS_MODE_STRIP as u32;
        }
        if eval_type == JS_EVAL_TYPE_MODULE {
            let module_name = if let Ok(a) = (*ctx).create_atom(filename.to_string_lossy().as_ref())
            {
                a
            } else {
                return JSValue::EXCEPTION;
            };

            m = if let Ok(p) = JSModuleDef::new(ctx.as_mut().unwrap(), module_name) {
                Box::into_raw(p)
            } else {
                return JSValue::EXCEPTION;
            };

            js_mode |= JS_MODE_STRICT as u32;
        }
    }

    let fd = js_new_function_def(
        ctx,
        std::ptr::null_mut(),
        true,
        false,
        filename.as_ptr().cast(),
        1,
    );
    if fd.is_null() {
        fail1!(m);
    }

    s.cur_func = fd;

    (*fd).eval_type = eval_type;
    (*fd).has_this_binding = eval_type != JS_EVAL_TYPE_DIRECT;
    (*fd).backtrace_barrier = (flags & JS_EVAL_FLAG_BACKTRACE_BARRIER) != 0;
    if eval_type == JS_EVAL_TYPE_DIRECT {
        (*fd).new_target_allowed = (*b).new_target_allowed;
        (*fd).super_call_allowed = (*b).super_call_allowed;
        (*fd).super_allowed = (*b).super_allowed;
        (*fd).arguments_allowed = (*b).arguments_allowed;
    } else {
        (*fd).new_target_allowed = false;
        (*fd).super_call_allowed = false;
        (*fd).super_allowed = false;
        (*fd).arguments_allowed = true;
    }
    (*fd).js_mode = js_mode as u8;
    (*fd).func_name = JS_DupAtom(ctx, JS_ATOM__eval_);

    if !b.is_null() {
        if add_closure_variables(ctx, fd, b, scope_idx).is_err() {
            fail!(fd, m);
        }
    }

    (*fd).module = m;
    if !m.is_null() || (flags & JS_EVAL_FLAG_ASYNC != 0) {
        (*fd).in_function_body = true;
        (*fd).func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
    }

    s.is_module = !m.is_null();
    s.allow_html_comments = !s.is_module;

    /* enter body scope */
    (*fd).body_scope = if let Ok(l) = s.push_scope() {
        l
    } else {
        fail!(fd, m);
    };

    if s.parse_program().is_err() {
        fail!(fd, m);
    }

    if !m.is_null() {
        (*m).has_tla = (*fd).has_await;
    }

    // create the function object and all the enclosed functions
    let mut fun_obj = js_create_function(ctx.as_mut().unwrap(), fd);
    if fun_obj.is_exception() {
        fail1!(m);
    }

    /* Could add a flag to avoid resolution if necessary */
    if !m.is_null() {
        (*m).func_obj = fun_obj;
        if js_resolve_module(ctx, m) < 0 {
            fail1!(m);
        }
        fun_obj = JS_NewModuleValue(ctx, m);
    }

    // returns compiled or evaluated
    if flags & JS_EVAL_FLAG_COMPILE_ONLY != 0 {
        fun_obj
    } else {
        JS_EvalFunctionInternal(ctx, fun_obj, this_obj, var_refs, sf)
    }
}

pub unsafe fn skip_shebang(pp: *mut *const u8, buf_end: *const u8) {
    let mut p = *pp;

    if *p.offset(0) == b'#' && *p.offset(1) == b'!' {
        p = p.offset(2);
        while p < buf_end {
            match *p {
                b'\n' | b'\r' => {
                    break;
                }
                b if b >= 0x80 => {
                    if let Ok(c) = unicode_from_utf8(p, UTF8_CHAR_LEN_MAX as i32, &mut p) {
                        if matches!(c, '\u{2028}' | '\u{2029}') {
                            break;
                        }
                    } else {
                        p = p.offset(1); // skip invalid UTF-8
                    }
                }
                _ => {
                    p = p.offset(1);
                }
            }
        }
        *pp = p;
    }
}

pub unsafe fn JS_EvalFunctionInternal(
    ctx: *mut JSContext,
    fun_obj: JSValue,
    this_obj: JSValueConst,
    var_refs: &[NonNull<JSVarRef>],
    sf: *mut JSStackFrame,
) -> JSValue {
    match JS_VALUE_GET_TAG(fun_obj) {
        JS_TAG_FUNCTION_BYTECODE => {
            let fun_closure = js_closure(ctx, fun_obj, var_refs, sf);
            JS_CallFree(
                ctx.as_mut().unwrap(),
                fun_closure,
                this_obj,
                0,
                std::ptr::null_mut(),
            )
        }
        JS_TAG_MODULE => {
            let m = JS_VALUE_GET_PTR(fun_obj).cast::<JSModuleDef>();
            // the module refcount should be >= 2
            JS_FreeValue(ctx, fun_obj);
            if js_create_module_function(ctx, m) < 0 {
                return JSValue::EXCEPTION;
            }
            if js_link_module(ctx, m) < 0 {
                // C2RS: goto fail translated to return JSValue::EXCEPTION
                return JSValue::EXCEPTION;
            }
            let r = js_evaluate_module(ctx, m);
            if JS_IsException(r) {
                JSValue::EXCEPTION
            } else {
                r
            }
        }
        _ => {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"bytecode function expected");
            JSValue::EXCEPTION
        }
    }

    // let retval: JSValue = if tag == JS_TAG_FUNCTION_BYTECODE {
    //     let fun_closure = js_closure(ctx, fun_obj, var_refs, sf);
    //     JS_CallFree(ctx, fun_closure, this_obj, 0, std::ptr::null_mut())
    // } else if tag == JS_TAG_MODULE {
    //     let m = JS_VALUE_GET_PTR(fun_obj).cast::<JSModuleDef>();
    //     // the module refcount should be >= 2
    //     JS_FreeValue(ctx, fun_obj);
    //     if js_create_module_function(ctx, m) < 0 {
    //         return JSValue::EXCEPTION;
    //     }
    //     if js_link_module(ctx, m) < 0 {
    //         // C2RS: goto fail translated to return JSValue::EXCEPTION
    //         return JSValue::EXCEPTION;
    //     }
    //     let r = js_evaluate_module(ctx, m);
    //     if JS_IsException(r) {
    //         JS_EXCEPTION
    //     } else {
    //         r
    //     }
    // } else {
    //     JS_FreeValue(ctx, fun_obj);
    //     ctx.as_mut()
    //         .unwrap()
    //         .JS_ThrowTypeError(c"bytecode function expected")
    // };
    // retval
}

pub unsafe fn JS_EvalFunction(ctx: *mut JSContext, fun_obj: JSValue) -> JSValue {
    return JS_EvalFunctionInternal(ctx, fun_obj, (*ctx).global_obj, &[], std::ptr::null_mut());
}

/// `val` keep alive
pub unsafe fn JS_EvalObject(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    val: JSValueConst,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    if !val.is_string() {
        return val.dup(); // dup and return val
    }

    match val.to_utf8_string(ctx.as_mut().unwrap()) {
        Ok(str) => {
            let ret = JS_EvalInternal(
                ctx,
                this_obj,
                str.as_ptr().cast(),
                str.len(),
                c"<input>",
                flags,
                scope_idx,
            );
            return ret;
        }
        Err(_) => {
            return JSValue::EXCEPTION;
        }
    }

    // original code
    // let mut len: usize = 0;
    // let str = JS_ToCStringLen(ctx, &mut len, val);
    // if str.is_null() {
    //     return JSValue::EXCEPTION;
    // }

    // let ret = JS_EvalInternal(ctx, this_obj, str.cast(), len, c"<input>", flags, scope_idx);
    // JS_FreeCString(ctx, str);

    // ret
}

/// Run the <eval> function of the module and of all its requested modules.
/// Return a promise or an exception.
pub unsafe fn js_evaluate_module(ctx: *mut JSContext, mut m: *mut JSModuleDef) -> JSValue {
    debug_assert!(matches!(
        (*m).status,
        JSModuleStatus::JS_MODULE_STATUS_LINKED
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ));

    if matches!(
        (*m).status,
        JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ) {
        m = (*m).cycle_root;
    }

    // a promise may be created only on the cycle_root of a cycle
    if !(*m).promise.is_undefined() {
        return (*m).promise.dup();
    }

    if let Ok((promise, rr)) = (*ctx).create_promise(JSValue::UNDEFINED) {
        (*m).resolving_funcs = rr;
        (*m).promise = promise;
    } else {
        return JSValue::EXCEPTION;
    }

    let mut result = JSValue::UNDEFINED;
    let mut stack_top = std::ptr::null_mut::<JSModuleDef>();

    if js_inner_module_evaluation(ctx, m, 0, &mut stack_top, &mut result) < 0 {
        while !stack_top.is_null() {
            let m1 = stack_top;
            debug_assert_eq!((*m1).status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING);

            (*m1).status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
            (*m1).eval_has_exception = true;
            (*m1).eval_exception = JS_DupValue(ctx, result);
            (*m1).cycle_root = m; // spec bug: should be present
            stack_top = (*m1).stack_prev;
        }
        JS_FreeValue(ctx, result);
        debug_assert_eq!((*m).status, JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
        debug_assert!((*m).eval_has_exception);

        let ret_val = JS_Call(
            ctx.as_mut().unwrap(),
            *(*m).resolving_funcs.as_ptr().offset(1), // reject
            JS_UNDEFINED,
            1,
            &mut (*m).eval_exception,
        );
        JS_FreeValue(ctx, ret_val);
    } else {
        assert!(matches!(
            (*m).status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));
        debug_assert!(!(*m).eval_has_exception);

        if !(*m).async_evaluation {
            debug_assert_eq!((*m).status, JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
            let mut value = JS_UNDEFINED;
            let ret_val = JS_Call(
                ctx.as_mut().unwrap(),
                *(*m).resolving_funcs.as_ptr().offset(0), // resolve
                JS_UNDEFINED,
                1,
                &mut value,
            );
            JS_FreeValue(ctx, ret_val);
        }
        debug_assert!(stack_top.is_null());
    }

    JS_DupValue(ctx, (*m).promise)
}

// the indirection is needed to make 'eval' optional
pub unsafe fn JS_EvalInternal(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    input: *const u8,
    input_len: usize,
    filename: &'static CStr,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    if let Some(cb) = (*ctx).eval_internal {
        cb(ctx, this_obj, input, input_len, filename, flags, scope_idx)
    } else {
        ctx.as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"eval is not supported")
    }
}

pub unsafe fn JS_EvalThis(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    input: *const u8,
    input_len: usize,
    filename: &'static CStr,
    eval_flags: u32,
) -> JSValue {
    let eval_type = eval_flags & JS_EVAL_TYPE_MASK;
    debug_assert!(matches!(
        eval_type,
        JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE
    ));

    JS_EvalInternal(ctx, this_obj, input, input_len, filename, eval_flags, -1)
}

pub unsafe fn JS_Eval(
    ctx: *mut JSContext,
    input: *const u8,
    input_len: usize,
    filename: &'static CStr,
    eval_flags: u32,
) -> JSValue {
    JS_EvalThis(
        ctx,
        (*ctx).global_obj,
        input,
        input_len,
        filename,
        eval_flags,
    )
}
