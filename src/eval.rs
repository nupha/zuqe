use std::{
    ffi::{CStr, CString},
    ops::DerefMut,
    ptr::NonNull,
};

use crate::{
    atom::consts::JS_ATOM__eval_,
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    function::{JSFunctionDef, JSFunctionKindEnum, add_closure_variables},
    module::ModuleDef,
    object::JSObject,
    parse::{JS_DetectModule, JSParseState, skip_shebang},
    stack_frame::StackFrame,
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

// Eval types
pub const JS_EVAL_TYPE_GLOBAL: u32 = 0 << 0;
pub const JS_EVAL_TYPE_MODULE: u32 = 1 << 0;
pub const JS_EVAL_TYPE_DIRECT: u32 = 2 << 0;
pub const JS_EVAL_TYPE_INDIRECT: u32 = 3 << 0;
pub const JS_EVAL_TYPE_MASK: u32 = 3 << 0;

// Eval flags
pub const JS_EVAL_FLAG_STRICT: u32 = 1 << 3;
pub const JS_EVAL_FLAG_STRIP: u32 = 1 << 4;
pub const JS_EVAL_FLAG_COMPILE_ONLY: u32 = 1 << 5;
pub const JS_EVAL_FLAG_BACKTRACE_BARRIER: u32 = 1 << 6;
pub const JS_EVAL_FLAG_ASYNC: u32 = 1 << 7;

impl JSContext {
    /// parse, compile and eval source script.
    pub(crate) fn eval_script(
        &self,
        this_obj: JSValue,
        source: &CStr,
        filename: &str,
        flags: u32,
        scope_idx: i32,
    ) -> JSValue {
        macro_rules! fail {
            ($s:expr, $fd:expr, $m:expr) => {
                $s.free_token();
                return JSValue::EXCEPTION;
            };
        }

        let eval_type = flags & JS_EVAL_TYPE_MASK;

        let mut sframe: Option<&mut StackFrame> = None;
        let mut cur_frame_func: Option<JSObject> = None;

        // get bytecode, module of current stack frame
        let (cur_frame_bc, md, js_mode) = if eval_type == JS_EVAL_TYPE_DIRECT {
            let sf_ptr = self.rt().cur_stack_frame.cast::<StackFrame>();
            debug_assert!(!sf_ptr.is_null());
            debug_assert!(unsafe { (*sf_ptr).cur_running.is_object() });

            sframe = unsafe { sf_ptr.as_mut() };
            cur_frame_func = unsafe {
                Some(
                    sframe
                        .as_ref()
                        .unwrap()
                        .cur_running
                        .object_unchecked()
                        .clone(),
                )
            };

            let b = cur_frame_func
                .as_ref()
                .unwrap()
                .0
                .isa
                .borrow()
                .func()
                .unwrap()
                .bytecode
                .clone();

            let mode = b.as_ref().unwrap().0.borrow().js_mode as u32;
            (b, None, mode)
        } else {
            let mut js_mode = 0;
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
                (None, Some(ModuleDef::new(self, module_name)), js_mode)
            } else {
                (None, None, js_mode)
            }
        };

        // create root function def.
        let mut root_fd = if let Ok(mut fd) =
            JSFunctionDef::new(self, std::ptr::null_mut(), true, false, filename, 1)
        {
            fd.eval_type = eval_type;
            fd.has_this_binding = eval_type != JS_EVAL_TYPE_DIRECT;
            fd.backtrace_barrier = (flags & JS_EVAL_FLAG_BACKTRACE_BARRIER) != 0;
            fd.func_name = JS_ATOM__eval_.dup(self.rt());
            fd.js_mode = js_mode as u8;

            if let Some(bc) = &cur_frame_bc {
                debug_assert_eq!(eval_type, JS_EVAL_TYPE_DIRECT, "must be direct eval type");
                debug_assert!(md.is_none(), "must not have module def");

                let b = bc.0.borrow();

                fd.new_target_allowed = b.new_target_allowed;
                fd.super_call_allowed = b.super_call_allowed;
                fd.super_allowed = b.super_allowed;
                fd.arguments_allowed = b.arguments_allowed;
            } else {
                debug_assert_ne!(
                    eval_type, JS_EVAL_TYPE_DIRECT,
                    "must not be direct eval type"
                );
                fd.new_target_allowed = false;
                fd.super_call_allowed = false;
                fd.super_allowed = false;
                fd.arguments_allowed = true;
            }

            if let Some(b) = cur_frame_bc {
                if unsafe { add_closure_variables(self, &mut fd, &b, scope_idx).is_err() } {
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
                unsafe { NonNull::new_unchecked(root_fd.as_mut()) },
                source.as_ptr().cast(),
                source.count_bytes(),
                filename,
            );
            debug_assert_eq!(
                ps.cur_func_mut() as *mut JSFunctionDef,
                root_fd.deref_mut() as *mut JSFunctionDef
            );
            ps.is_module = md.as_ref().is_some();
            ps.allow_html_comments = md.is_none();

            // begin parsing
            unsafe {
                skip_shebang(&mut ps.buf_ptr, ps.buf_end);
            }
            if unsafe { ps.parse_program().is_err() } {
                fail!(ps, root_fd, m);
            }
        }

        let has_await = root_fd.has_await;

        // compile function def to bytecode.
        let bc_compiled = if let Ok(c) = unsafe { root_fd.compile(self) } {
            c
        } else {
            return JSValue::EXCEPTION;
        };

        // Could add a flag to avoid resolution if necessary
        if let Some(m) = md.as_ref() {
            {
                let mut b = m.0.borrow_mut();
                b.bytecode = JSValue::Bytecode(bc_compiled.clone());
                b.has_tla = has_await;
            }

            if self.resolve_module(&m).is_err() {
                return JSValue::EXCEPTION;
            }
        }

        if flags & JS_EVAL_FLAG_COMPILE_ONLY != 0 {
            // returns compiled bytecode
            return JSValue::Bytecode(bc_compiled);
        }

        // eval bytecode or module.
        if let Some(f) = cur_frame_func.as_ref() {
            // direct eval type: eval bytecode directly.
            debug_assert!(sframe.is_some());
            let isa = f.0.isa.borrow();
            let var_refs = isa.func().unwrap().var_refs.as_slice();
            bc_compiled
                .eval(self, &this_obj, var_refs, sframe)
                .unwrap_or(JSValue::EXCEPTION)
        } else if let Some(md) = md {
            // eval module
            debug_assert_eq!(eval_type, JS_EVAL_TYPE_MODULE);
            md.eval(self)
        } else {
            debug_assert!(matches!(
                eval_type,
                JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_INDIRECT
            ));
            debug_assert!(sframe.is_none());

            bc_compiled
                .eval(self, &this_obj, &[], None)
                .unwrap_or(JSValue::EXCEPTION)
        }
    }

    /// eval script with specified `this` binding.
    #[inline(always)]
    pub fn eval_this(
        &self,
        this_obj: JSValue,
        source: &CStr,
        filename: &str,
        flags: u32,
    ) -> JSValue {
        debug_assert!(matches!(
            flags & JS_EVAL_TYPE_MASK,
            JS_EVAL_TYPE_GLOBAL | JS_EVAL_TYPE_MODULE
        ));
        self.eval_script(this_obj, source, filename, flags, -1)
    }

    /// eval script with global as `this` binding.
    #[inline]
    pub fn eval(&self, source: &CStr, filename: &str, eval_flags: u32) -> JSValue {
        let global = self.0.borrow().global_obj.clone();
        self.eval_this(global, source, filename, eval_flags)
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
                if let JSValue::Module(md) = module_func {
                    if md.0.borrow_mut().set_import_meta(self, url, true).is_err() {
                        JSValue::EXCEPTION
                    } else {
                        md.eval(self)
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

    // /// Eval bytecode or module.
    // pub(crate) fn eval_bytecode_or_module(
    //     &self,
    //     bc_or_module: &JSValue,
    //     this_obj: &JSValue,
    //     var_refs: &[JSVarRef],
    //     parent_frame: Option<&mut StackFrame>,
    // ) -> JSResult {
    //     match bc_or_module {
    //         JSValue::Bytecode(bc) => bc.eval(self, this_obj, var_refs, parent_frame),
    //         JSValue::Module(md) => md.eval(self).check_exception(),
    //         _ => {
    //             self.JS_ThrowTypeError("bytecode expected");
    //             Err(-1)
    //         }
    //     }
    // }
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
                Ok(str) => ctx.eval_script(
                    this_obj,
                    CString::new(str.as_bytes()).unwrap().as_c_str(),
                    "<input>",
                    flags,
                    scope_idx,
                ),
                Err(_) => JSValue::EXCEPTION,
            }
        }
    }
}
