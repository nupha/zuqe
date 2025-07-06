use std::{
    ffi::{CStr, CString},
    ops::DerefMut,
    ptr::NonNull,
};

use crate::{
    atom::consts::JS_ATOM__eval_,
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    function::{
        JS_Call, JSFunctionDef, JSFunctionKindEnum, JSVarRef, add_closure_variables, js_closure,
    },
    module::{JSModuleDef, js_create_module_function, js_link_module},
    object::JSObject,
    parse::{JS_DetectModule, JSParseState, skip_shebang},
    stack_frame::JSStackFrame,
    value::JSValue,
};

/// Native function to handle `eval`, returns eval result.
/// `input` is script source with zero-terminated (\0) byte indicating end.
/// `this_obj` must not be consumed.
pub type EvalFunc = fn(
    ctx: &JSContext,
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
    /// evaluate source script `input`.
    pub unsafe fn eval_internal(
        &self,
        this_obj: JSValue,
        input: &CStr,
        filename: &str,
        flags: u32,
        scope_idx: i32,
    ) -> JSValue {
        macro_rules! fail {
            ($s:expr, $fd:expr, $m:expr) => {
                $s.free_token();
                // $fd.dispose(self);
                return JSValue::EXCEPTION;
            };
        }

        let eval_type = flags & JS_EVAL_TYPE_MASK;

        let mut sf: *mut JSStackFrame = std::ptr::null_mut();
        let mut func_obj: Option<JSObject> = None;
        let mut js_mode: u32 = 0;

        // get bytecode, module
        let (bc, md) = if eval_type == JS_EVAL_TYPE_DIRECT {
            sf = self.rt().current_stack_frame.cast::<JSStackFrame>();
            debug_assert!(!sf.is_null());
            debug_assert!((*sf).cur_func.is_object());

            func_obj = Some((*sf).cur_func.object_unchecked().clone());

            let b = func_obj
                .as_ref()
                .unwrap()
                .0
                .isa
                .borrow()
                .func()
                .unwrap()
                .bytecode
                .clone();

            js_mode = b.as_ref().unwrap().0.borrow().js_mode as _;
            (b, None)
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
                js_mode |= JS_MODE_STRICT as u32;
                (None, Some(JSModuleDef::new(self, module_name)))
            } else {
                (None, None)
            }
        };

        // create root function def.
        let mut root_fd = if let Ok(mut fd) =
            JSFunctionDef::new(self, std::ptr::null_mut(), true, false, filename, 1)
        {
            fd.eval_type = eval_type;
            fd.has_this_binding = eval_type != JS_EVAL_TYPE_DIRECT;
            fd.backtrace_barrier = (flags & JS_EVAL_FLAG_BACKTRACE_BARRIER) != 0;

            if eval_type == JS_EVAL_TYPE_DIRECT {
                let b = bc.as_ref().unwrap().0.borrow();
                fd.new_target_allowed = b.new_target_allowed;
                fd.super_call_allowed = b.super_call_allowed;
                fd.super_allowed = b.super_allowed;
                fd.arguments_allowed = b.arguments_allowed;
            } else {
                fd.new_target_allowed = false;
                fd.super_call_allowed = false;
                fd.super_allowed = false;
                fd.arguments_allowed = true;
            }
            fd.js_mode = js_mode as u8;
            fd.func_name = JS_ATOM__eval_.dup(self.rt());

            if let Some(b) = bc {
                if add_closure_variables(self, fd.deref_mut(), &b, scope_idx).is_err() {
                    return JSValue::EXCEPTION;
                }
            }

            fd.module = md.clone();
            if fd.module.is_some() || flags & JS_EVAL_FLAG_ASYNC != 0 {
                fd.in_function_body = true;
                fd.func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
            }

            // add fd body scope.
            // initially there are scope level 0 {-1, -1},
            // so body scope will be level 1.
            fd.body_scope = if let Ok(scope) = fd.push_scope(0) {
                scope as i32
            } else {
                return JSValue::EXCEPTION;
            };
            debug_assert_eq!(fd.body_scope, 1);

            fd
        } else {
            return JSValue::EXCEPTION;
        };

        {
            // create parse state with root fd
            let mut ps = JSParseState::new(
                self,
                NonNull::new_unchecked(root_fd.as_mut()),
                input.as_ptr().cast(),
                input.count_bytes(),
                filename,
            );
            debug_assert_eq!(
                ps.cur_func_mut() as *mut JSFunctionDef,
                root_fd.deref_mut() as *mut JSFunctionDef
            );
            ps.is_module = md.as_ref().is_some();
            ps.allow_html_comments = md.is_none();

            // begin parsing
            skip_shebang(&mut ps.buf_ptr, ps.buf_end);
            if ps.parse_program().is_err() {
                fail!(ps, root_fd, m);
            }
        }

        if let Some(m) = md.as_ref() {
            m.0.borrow_mut().has_tla = root_fd.has_await;
        }

        // create the bytecode object and all the enclosed functions.
        let mut bc_obj = if let Ok(bc) = root_fd.make_function_bytecode(self) {
            bc
        } else {
            return JSValue::EXCEPTION;
        };

        // Could add a flag to avoid resolution if necessary
        if let Some(m) = md.as_ref() {
            m.0.borrow_mut().func_obj = bc_obj;
            if self.resolve_module(&m).is_err() {
                return JSValue::EXCEPTION;
            }
            bc_obj = JSValue::Module(m.clone());
        }

        // returns compiled or evaluated
        if flags & JS_EVAL_FLAG_COMPILE_ONLY != 0 {
            bc_obj
        } else {
            if let Some(f) = func_obj.as_ref() {
                let isa = f.0.isa.borrow();
                let var_refs = isa.func().unwrap().var_refs.as_slice();
                JS_EvalFunctionInternal(self, bc_obj, &this_obj, var_refs, sf)
            } else {
                JS_EvalFunctionInternal(self, bc_obj, &this_obj, &[], sf)
            }
        }
    }

    // pub fn eval_hook(
    //     ctx: &JSContext,
    //     this_obj: JSValue,
    //     input: &[u8],
    //     filename: &str,
    //     flags: u32,
    //     scope_idx: i32,
    // ) -> JSValue {
    //     unsafe { ctx.eval_internal(this_obj, input, filename, flags, scope_idx) }
    // }

    #[inline(always)]
    pub fn eval_this(
        &self,
        this_obj: JSValue,
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

    pub fn eval(&self, input: &CStr, filename: &str, eval_flags: u32) -> JSValue {
        let global = self.0.borrow().global_obj.clone();
        self.eval_this(global, input, filename, eval_flags)
    }

    /// eval source script with auto detecting module or global mode. returns EXCEPTION when error.
    pub fn eval_auto_detect(&self, source: &CStr, url: &str) -> JSValue {
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
                if let JSValue::Module(mdef) = &module_func {
                    if mdef
                        .0
                        .borrow_mut()
                        .set_import_meta(self, url, true)
                        .is_err()
                    {
                        JSValue::EXCEPTION
                    } else {
                        unsafe { JS_EvalFunction(self, module_func) }
                    }
                } else {
                    unreachable!()
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
        &self,
        ctx: &JSContext,
        this_obj: JSValue,
        flags: u32,
        scope_idx: i32,
    ) -> JSValue {
        if !self.is_string() {
            self.clone()
        } else {
            match self.coerce_to_utf8(ctx) {
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

/// `fun_obj` is consumed, `this_obj` is not consumed.
/// `fun_obj` could be Bytecode or Module.
pub unsafe fn JS_EvalFunctionInternal(
    ctx: &JSContext,
    fun_obj: JSValue,
    this_obj: &JSValue,
    var_refs: &[JSVarRef],
    sf: *mut JSStackFrame,
) -> JSValue {
    match fun_obj {
        JSValue::FunctionBytecode(bc) => {
            let fun_closure = js_closure(ctx, &bc, var_refs, sf);
            JS_Call(ctx, &fun_closure, &this_obj, &[])
        }
        JSValue::Module(m) => {
            if js_create_module_function(ctx, &m) < 0 {
                return JSValue::EXCEPTION;
            }
            if js_link_module(ctx, &m) < 0 {
                return JSValue::EXCEPTION;
            }
            m.eval(ctx)
        }
        _ => {
            ctx.JS_ThrowTypeError("bytecode function expected");
            JSValue::EXCEPTION
        }
    }
}

/// `fun_obj` is consumed.
pub unsafe fn JS_EvalFunction(ctx: &JSContext, fun_obj: JSValue) -> JSValue {
    return JS_EvalFunctionInternal(
        ctx,
        fun_obj,
        &ctx.0.borrow().global_obj.clone(),
        &[],
        std::ptr::null_mut(),
    );
}
