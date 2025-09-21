use std::{ops::DerefMut, ptr::NonNull};

use crate::{
    atom::{JSAtom, consts::JS_ATOM__eval_},
    context::{JS_MODE_STRICT, JS_MODE_STRIP, JSContext},
    error::JSResult,
    function::{Bytecode, JSFunctionDef, JSFunctionKindEnum, add_closure_variables},
    module::ModuleDef,
    object::JSObject,
    parse::JSParseState,
    stack_frame::StackFrame,
    value::JSValue,
};

/// Native function to handle `eval`, returns eval result.
/// `input` is script source.
pub type EvalFn = fn(
    ctx: &JSContext,
    this_obj: &JSValue,
    input: &[u8],
    filename: &str,
    flags: u32,
    scope_idx: i32,
) -> JSResult;

// Eval types
pub const JS_EVAL_TYPE_GLOBAL: u32 = 0;
pub const JS_EVAL_TYPE_MODULE: u32 = 1;
pub const JS_EVAL_TYPE_DIRECT: u32 = 2;
pub const JS_EVAL_TYPE_INDIRECT: u32 = 3;
pub const JS_EVAL_TYPE_MASK: u32 = 3;

// Eval flags
pub const JS_EVAL_FLAG_STRICT: u32 = 1 << 3;
pub const JS_EVAL_FLAG_STRIP: u32 = 1 << 4;
pub const JS_EVAL_FLAG_COMPILE_ONLY: u32 = 1 << 5;
pub const JS_EVAL_FLAG_BACKTRACE_BARRIER: u32 = 1 << 6;
/// allow top-level-await in eval
pub const JS_EVAL_FLAG_ASYNC: u32 = 1 << 7;

pub(crate) enum Compiled {
    /// compiled for global or indirect mode.
    GlobalOrIndirect(Bytecode),
    /// compiled for direct mode.
    Direct(Bytecode),
    /// compiled as module.
    Module(ModuleDef),
}

impl JSContext {
    /// compile script source according to requested mode.
    /// compile only, won't run. internal use.
    pub(crate) fn compile(
        &self,
        source: &[u8],
        filename: &str,
        flags: u32,
        scope_idx: i32,
    ) -> Result<Compiled, i32> {
        let eval_type = flags & JS_EVAL_TYPE_MASK;

        let sframe: Option<&mut StackFrame>;
        let mut cur_frame_func: Option<JSObject> = None;

        // get (bytecode, module, js_mode) of current stack frame
        let (bytecode, module, js_mode) = if eval_type == JS_EVAL_TYPE_DIRECT {
            let sf_ptr = self.rt().cur_stack_frame.cast::<StackFrame>();
            debug_assert!(!sf_ptr.is_null());
            debug_assert!(unsafe { (*sf_ptr).cur_func.is_object() });

            sframe = unsafe { sf_ptr.as_mut() };
            cur_frame_func = Some(
                sframe
                    .as_ref()
                    .unwrap()
                    .cur_func
                    .as_object()
                    .cloned()
                    .unwrap(),
            );

            let bc = cur_frame_func
                .as_ref()
                .unwrap()
                .0
                .payload
                .borrow()
                .func()
                .and_then(|f| f.bytecode());

            let mode = bc.as_ref().unwrap().0.borrow().js_mode as u32;
            (bc, None, mode)
        } else {
            let mut js_mode = 0;
            if flags & JS_EVAL_FLAG_STRICT != 0 {
                js_mode |= JS_MODE_STRICT as u32;
            }
            if flags & JS_EVAL_FLAG_STRIP != 0 {
                js_mode |= JS_MODE_STRIP as u32;
            }

            if eval_type == JS_EVAL_TYPE_MODULE {
                let module_name = JSAtom::new(self.rt_mut(), filename)?;
                js_mode |= JS_MODE_STRICT as u32;
                (None, Some(ModuleDef::new(self, module_name)), js_mode)
            } else {
                (None, None, js_mode)
            }
        };

        // create root function def.
        let mut fd = JSFunctionDef::new(self, std::ptr::null_mut(), true, false, filename, 1)?;

        fd.eval_type = eval_type;
        fd.has_this_binding = eval_type != JS_EVAL_TYPE_DIRECT;
        fd.backtrace_barrier = flags & JS_EVAL_FLAG_BACKTRACE_BARRIER != 0;
        fd.func_name = JS_ATOM__eval_.dup(self.rt());
        fd.js_mode = js_mode as u8;

        if let Some(bc) = &bytecode {
            debug_assert_eq!(eval_type, JS_EVAL_TYPE_DIRECT, "must be direct eval type");
            debug_assert!(module.is_none(), "must not be module");
            {
                let b = bc.0.borrow();
                fd.new_target_allowed = b.new_target_allowed;
                fd.super_call_allowed = b.super_call_allowed;
                fd.super_allowed = b.super_allowed;
                fd.arguments_allowed = b.arguments_allowed;
            }

            unsafe {
                add_closure_variables(self, &mut fd, &bc, scope_idx)?;
            }
        } else {
            debug_assert_ne!(eval_type, JS_EVAL_TYPE_DIRECT, "can't be direct eval type");
            fd.new_target_allowed = false;
            fd.super_call_allowed = false;
            fd.super_allowed = false;
            fd.arguments_allowed = true;
        }

        fd.module = module.clone();
        if fd.module.is_some() || flags & JS_EVAL_FLAG_ASYNC != 0 {
            fd.in_function_body = true;
            fd.func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
        }

        // add fd body scope.
        // initially there are scope level 0 {-1, -1},
        // so body scope will be level 1.
        fd.body_scope = fd.push_scope(0)? as i32;
        debug_assert_eq!(fd.body_scope, 1);

        // create parse state with root fd, then parse source.
        let mut parser = JSParseState::new(
            self,
            unsafe { NonNull::new_unchecked(fd.as_mut()) },
            source,
            filename,
        );
        debug_assert_eq!(
            parser.cur_func_mut() as *mut JSFunctionDef,
            fd.deref_mut() as *mut JSFunctionDef
        );
        parser.is_module = module.is_some();
        parser.allow_html_comments = module.is_none();

        // parser.skip_shebang();
        parser.parse_program()?;

        // has top-level await?
        let has_await = fd.has_await;

        // compile function def to bytecode.
        let bc_compiled = unsafe { fd.compile(self)? };

        // XXX: Could add a flag to avoid resolution if necessary
        if let Some(md) = module {
            // resolve module
            md.0.bytecode.set(JSValue::Bytecode(bc_compiled.clone()));
            md.0.has_tla.set(has_await);
            self.resolve_module(&md)?;
            Ok(Compiled::Module(md))
        } else {
            if cur_frame_func.is_some() {
                debug_assert_eq!(eval_type, JS_EVAL_TYPE_DIRECT);
                Ok(Compiled::Direct(bc_compiled))
            } else {
                Ok(Compiled::GlobalOrIndirect(bc_compiled))
            }
        }
    }

    /// compile source, then eval it. allowed eval types:
    /// * MODULE
    /// * GLOBAL
    /// * DIRECT
    /// * INDIRECT
    pub(crate) fn eval_internal(
        &self,
        this_obj: &JSValue,
        source: &[u8],
        filename: &str,
        flags: u32,
        scope_idx: i32,
    ) -> JSResult {
        if flags & JS_EVAL_FLAG_COMPILE_ONLY != 0 {
            panic!(
                "don't use JS_EVAL_FLAG_COMPILE_ONLY flag in eval().\nif you want compile only, use ::compile() method."
            );
        }

        match self.compile(source, filename, flags, scope_idx)? {
            Compiled::GlobalOrIndirect(bc) => bc.eval(self, this_obj, &[], None),
            Compiled::Direct(bc) => {
                // stack frame
                let sframe = unsafe {
                    let sf_ptr = self.rt().cur_stack_frame.cast::<StackFrame>();
                    debug_assert!(!sf_ptr.is_null());
                    debug_assert!((*sf_ptr).cur_func.is_object());
                    sf_ptr.as_mut().unwrap()
                };
                // current function of stack frame
                let frame_func = sframe.cur_func.as_object().cloned().unwrap();
                let payload = frame_func.0.payload.borrow();
                let var_refs = payload.func().unwrap().var_refs.as_slice();

                bc.eval(self, this_obj, var_refs, Some(sframe))
            }
            Compiled::Module(m) => m.eval(self).map(|p| p.into()),
        }
    }

    /// eval source in global mode, with specified `this` binding.
    #[inline(always)]
    pub fn eval_this(
        &self,
        this_obj: JSValue,
        source: &[u8],
        filename: &str,
        flags: u32,
    ) -> JSResult {
        debug_assert!(matches!(flags & JS_EVAL_TYPE_MASK, JS_EVAL_TYPE_GLOBAL));
        self.eval_internal(&this_obj, source, filename, flags, -1)
    }

    /// eval script with global as `this` binding.
    pub fn eval(&self, source: &[u8], filename: &str, eval_flags: u32) -> JSResult {
        let global = self.0.borrow().global_obj.clone();
        self.eval_this(global, source, filename, eval_flags)
    }
}

impl JSValue {
    /// eval js value `self`.
    /// When `self` is a:
    /// - String: eval source script and return result,
    /// - otherwise evaluated as-is.
    ///
    /// both `this_obj` and `self` are not consumed.
    pub fn eval_object(
        &self,
        ctx: &JSContext,
        this_obj: JSValue,
        flags: u32,
        scope_idx: i32,
    ) -> JSResult {
        if let Self::String(s) = self {
            ctx.eval_internal(
                &this_obj,
                s.content().to_utf8().as_bytes(),
                "<string>",
                flags,
                scope_idx,
            )
        } else {
            Ok(self.clone())
        }
    }
}
