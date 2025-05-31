use std::{
    cell::RefCell,
    ffi::{CStr, CString},
    ops::DerefMut,
    pin::Pin,
    ptr::NonNull,
    rc::Rc,
};

use crate::{
    atom::consts::JS_ATOM__eval_,
    context::{JSContext, JS_MODE_STRICT, JS_MODE_STRIP},
    function::{
        add_closure_variables, js_class_has_bytecode, js_closure, JSFunctionBytecode,
        JSFunctionDef, JSFunctionKindEnum, JSVarRef, JS_CallFree,
    },
    gc::RefCounted,
    module::{
        js_create_module_function, js_free_module_def, js_link_module, js_resolve_module,
        JSModuleDef, JS_NewModuleValue,
    },
    parse::{skip_shebang, JSParseState, JS_DetectModule},
    stack_frame::JSStackFrame,
    value::{
        JSValue, JSValueConst, JS_FreeValue, JS_TAG_FUNCTION_BYTECODE, JS_TAG_MODULE,
        JS_TAG_OBJECT, JS_VALUE_GET_PTR, JS_VALUE_GET_TAG,
    },
};

/// Native function to handle `eval`, returns eval result.
/// `input` is script source with zero-terminated (\0) byte indicating end.
/// `this_obj` must not be consumed.
pub type EvalFunc = fn(
    ctx: &mut JSContext,
    this_obj: JSValue,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue;

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

impl JSContext {
    pub unsafe fn eval_internal(
        &mut self,
        this_obj: JSValueConst,
        input: &CStr,
        filename: &str,
        flags: u32,
        scope_idx: i32,
    ) -> JSValue {
        macro_rules! fail1 {
            ($m:expr) => {
                /* XXX: should free all the unresolved dependencies */
                if !$m.is_null() {
                    js_free_module_def(self, $m);
                }
                return JSValue::EXCEPTION;
            };
        }
        macro_rules! fail {
            ($s:expr, $fd:expr, $m:expr) => {
                $s.free_token();
                $fd.dispose(self);
                fail1!($m);
            };
        }

        let eval_type = flags & JS_EVAL_TYPE_MASK;
        let mut m: *mut JSModuleDef = std::ptr::null_mut();
        let mut b: *mut JSFunctionBytecode = std::ptr::null_mut();
        let mut sf: *mut JSStackFrame = std::ptr::null_mut();
        let mut var_refs: &[NonNull<Pin<JSVarRef>>] = &[];
        let mut js_mode: u32 = 0;

        if eval_type == JS_EVAL_TYPE_DIRECT {
            sf = self.rt().current_stack_frame;
            debug_assert!(!sf.is_null());
            debug_assert!(JS_VALUE_GET_TAG((*sf).cur_func) == JS_TAG_OBJECT);

            let ob = (*sf).cur_func.object_unchecked();
            let func = ob.isa().func().expect("expect bytecode object");
            b = func.function_bytecode;
            var_refs = func.var_refs.as_slice();
            js_mode = (*b).js_mode as _;
        } else {
            if flags & JS_EVAL_FLAG_STRICT != 0 {
                js_mode |= JS_MODE_STRICT as u32;
            }
            if flags & JS_EVAL_FLAG_STRIP != 0 {
                js_mode |= JS_MODE_STRIP as u32;
            }
            if eval_type == JS_EVAL_TYPE_MODULE {
                let module_name = if let Ok(a) = self.create_atom(filename) {
                    a
                } else {
                    return JSValue::EXCEPTION;
                };

                m = if let Ok(p) = JSModuleDef::new(self, module_name) {
                    Box::into_raw(p)
                } else {
                    return JSValue::EXCEPTION;
                };

                js_mode |= JS_MODE_STRICT as u32;
            }
        }

        // create root function def.
        let mut root_fd = if let Ok(mut fd) = JSFunctionDef::new(
            self.handle(),
            std::ptr::null_mut(),
            true,
            false,
            filename,
            1,
        ) {
            fd.eval_type = eval_type;
            fd.has_this_binding = eval_type != JS_EVAL_TYPE_DIRECT;
            fd.backtrace_barrier = (flags & JS_EVAL_FLAG_BACKTRACE_BARRIER) != 0;

            if eval_type == JS_EVAL_TYPE_DIRECT {
                fd.new_target_allowed = (*b).new_target_allowed;
                fd.super_call_allowed = (*b).super_call_allowed;
                fd.super_allowed = (*b).super_allowed;
                fd.arguments_allowed = (*b).arguments_allowed;
            } else {
                fd.new_target_allowed = false;
                fd.super_call_allowed = false;
                fd.super_allowed = false;
                fd.arguments_allowed = true;
            }
            fd.js_mode = js_mode as u8;
            fd.func_name = JS_ATOM__eval_.dup(self.rt());

            if !b.is_null() {
                if add_closure_variables(self, fd.deref_mut(), b, scope_idx).is_err() {
                    fd.dispose(self);
                    fail1!(m);
                }
            }

            fd.module = m;
            if !m.is_null() || flags & JS_EVAL_FLAG_ASYNC != 0 {
                fd.in_function_body = true;
                fd.func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
            }

            // add fd body scope.
            // initially there are scope level 0 {-1, -1},
            // so body scope will be level 1.
            fd.body_scope = if let Ok(scope) = fd.push_scope(0) {
                scope as i32
            } else {
                fd.dispose(self);
                fail1!(m);
            };
            debug_assert_eq!(fd.body_scope, 1);

            fd
        } else {
            fail1!(m);
        };

        {
            // create parse state with root fd
            let mut ps = JSParseState::new(
                self.handle(),
                NonNull::new_unchecked(root_fd.as_mut()),
                input.as_ptr().cast(),
                input.count_bytes(),
                filename,
            );
            debug_assert_eq!(
                ps.cur_func_mut() as *mut JSFunctionDef,
                root_fd.deref_mut() as *mut JSFunctionDef
            );
            ps.is_module = !m.is_null();
            ps.allow_html_comments = m.is_null();

            // begin parsing
            skip_shebang(&mut ps.buf_ptr, ps.buf_end);
            if ps.parse_program().is_err() {
                fail!(ps, root_fd, m);
            }
        }

        if !m.is_null() {
            (*m).has_tla = root_fd.has_await;
        }

        // create the function object and all the enclosed functions.
        let mut fun_obj = root_fd.make_function(self);
        if fun_obj.is_exception() {
            fail1!(m);
        }

        // Could add a flag to avoid resolution if necessary
        if !m.is_null() {
            (*m).func_obj = fun_obj;
            if js_resolve_module(self, m) < 0 {
                fail1!(m);
            }
            fun_obj = JS_NewModuleValue(self, m);
        }

        // returns compiled or evaluated
        if flags & JS_EVAL_FLAG_COMPILE_ONLY != 0 {
            fun_obj
        } else {
            JS_EvalFunctionInternal(self, fun_obj, this_obj, var_refs, sf)
        }
    }

    // pub fn eval_hook(
    //     ctx: &mut JSContext,
    //     this_obj: JSValueConst,
    //     input: &[u8],
    //     filename: &str,
    //     flags: u32,
    //     scope_idx: i32,
    // ) -> JSValue {
    //     unsafe { ctx.eval_internal(this_obj, input, filename, flags, scope_idx) }
    // }

    #[inline(always)]
    pub fn eval_this(
        &mut self,
        this_obj: JSValueConst,
        input: &CStr,
        filename: &str,
        flags: u32,
    ) -> JSValue {
        debug_assert!(matches!(
            flags & JS_EVAL_TYPE_MASK,
            JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE
        ));
        unsafe { self.eval_internal(this_obj, input, filename, flags, -1) }
    }

    #[inline(always)]
    pub fn eval(&mut self, input: &CStr, filename: &str, eval_flags: u32) -> JSValue {
        self.eval_this(self.global_obj, input, filename, eval_flags)
    }

    /// eval source script with auto detecting module or global mode. returns EXCEPTION when error.
    pub fn eval_auto_detect(&mut self, source: &CStr, url: &str) -> JSValue {
        let eval_flags = if url.ends_with(".mjs") {
            JS_EVAL_TYPE_MODULE
        } else if unsafe { JS_DetectModule(source) } {
            JS_EVAL_TYPE_MODULE
        } else {
            JS_EVAL_TYPE_GLOBAL
        };

        if eval_flags & JS_EVAL_TYPE_MASK == JS_EVAL_TYPE_MODULE {
            // for the modules, we compile then run to be able to set import.meta
            let module_func = self.eval(source, url, eval_flags | JS_EVAL_FLAG_COMPILE_ONLY);
            if !module_func.is_exception() {
                unsafe {
                    debug_assert_eq!(JS_VALUE_GET_TAG(module_func), JS_TAG_MODULE);

                    let mdef = JS_VALUE_GET_PTR(module_func).cast::<JSModuleDef>();
                    if (*mdef).set_import_meta(self, url, true).is_err() {
                        JS_FreeValue(self, module_func);
                    };

                    JS_EvalFunction(self, module_func)
                }
            } else {
                JSValue::EXCEPTION
            }
        } else {
            self.eval(source, url, eval_flags)
        }
    }
}

impl JSValue {
    /// eval js value `self`.
    /// When `self` is a:
    /// - String: eval source script and return result,
    /// - otherwise eval'ed as-is.
    ///
    /// both `this_obj` and `self` are not consumed.
    pub fn eval_object(
        self,
        ctx: &mut JSContext,
        this_obj: JSValueConst,
        flags: u32,
        scope_idx: i32,
    ) -> JSValue {
        if !self.is_string() {
            self.dup() // as-is: dup and return val
        } else {
            match self.coerce_to_utf8(ctx.handle()) {
                Ok(str) => unsafe {
                    ctx.eval_internal(
                        this_obj,
                        CString::new(str.as_bytes()).unwrap().as_c_str(),
                        "<input>",
                        flags,
                        scope_idx,
                    )
                },
                Err(_) => JSValue::EXCEPTION,
            }
        }
    }
}

/// 'input' must be zero terminated i.e. input[input_len] = '\0'.
#[deprecated(note = "no use")]
pub fn __JS_EvalInternal(
    ctx: &mut JSContext,
    this_obj: JSValueConst,
    input: &CStr,
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSValue {
    unsafe { ctx.eval_internal(this_obj, input, filename, flags, scope_idx) }
}

/// `fun_obj` is consumed, `this_obj` is not consumed.
pub unsafe fn JS_EvalFunctionInternal(
    ctx: *mut JSContext,
    fun_obj: JSValue,
    this_obj: JSValueConst,
    var_refs: &[NonNull<Pin<JSVarRef>>],
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
            debug_assert!((*m).ref_count() >= 2, "the module refcount should be >= 2");

            JS_FreeValue(ctx, fun_obj);
            if js_create_module_function(ctx, m) < 0 {
                return JSValue::EXCEPTION;
            }
            if js_link_module(ctx, m) < 0 {
                return JSValue::EXCEPTION;
            }
            (*m).eval(ctx.as_mut().unwrap())
        }
        _ => {
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"bytecode function expected");
            JSValue::EXCEPTION
        }
    }
}

/// `fun_obj` is consumed.
pub unsafe fn JS_EvalFunction(ctx: *mut JSContext, fun_obj: JSValue) -> JSValue {
    return JS_EvalFunctionInternal(ctx, fun_obj, (*ctx).global_obj, &[], std::ptr::null_mut());
}
