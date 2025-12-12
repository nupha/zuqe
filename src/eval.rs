use std::{ops::Deref, ptr::NonNull};

use crate::{
    atom::Atom,
    builtins::promise::{Promise, PromiseState},
    context::{ContextImpl, ContextPtr},
    error::{ErrorText, JSResult},
    function::{Bytecode, JSFunctionDef, JSFunctionKindEnum, add_closure_variables},
    module::ModuleDef,
    parse::JSParseState,
    runtime::TickStatus,
    value::JSValue,
};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvalType {
    #[default]
    Global,
    Module,
    Direct,
    Indirect,
}

/// callback to handle `eval`, returns eval result.
/// `input` is script source.
pub(super) type EvalFn = fn(
    ctx: ContextPtr,
    this_obj: JSValue,
    input: &[u8],
    filename: &str,
    eval_type: EvalType,
    eval_flags: u32,
    scope_idx: i32,
) -> JSResult;

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct EvalMode: u8 {
        const STRICT = 1 << 0;
        const STRIP  = 1 << 1;
        const MATH   = 1 << 2;
        const ASYNC  = 1 << 3; // async function
    }
}

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
/// compile script as root async function,
/// so that top-level-await is allowed.
pub const JS_EVAL_FLAG_ASYNC: u32 = 1 << 7;

pub(crate) enum Compiled {
    /// compiled for global or indirect mode.
    GlobalOrIndirect(Bytecode),
    /// compiled for direct mode.
    Direct(Bytecode),
    /// compiled as module.
    Module(ModuleDef),
}

impl ContextImpl {
    /// compile `source` according to `flags` mode, don't eval.
    /// `filename` is used to identify the module.
    pub(crate) fn compile(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: u32,
        scope_idx: i32,
    ) -> Result<Compiled, i32> {
        // get (bytecode, module, js_mode) of current stack frame
        let (frame_func_bc, frame_module, js_mode) = if eval_type == EvalType::Direct {
            let cur_func = self
                .rt_ref()
                .current_stack_frame()
                .and_then(|sf| sf.cur_func.as_object_BCFunction())
                .unwrap(); // <-- Note: current stack function must be a BCFunction object

            let bc = cur_func.payload().bytecode();
            let mode = bc.mode();

            (Some(bc), None, mode)
        } else {
            let mut js_mode = EvalMode::empty();

            if eval_flags & JS_EVAL_FLAG_STRICT != 0 {
                js_mode |= EvalMode::STRICT;
            }
            if eval_flags & JS_EVAL_FLAG_STRIP != 0 {
                js_mode |= EvalMode::STRIP;
            }
            if eval_type == EvalType::Module {
                let name = Atom::new(self.rt_ptr(), filename)?;
                js_mode |= EvalMode::STRICT;
                (None, Some(ModuleDef::new(self, name)?), js_mode)
            } else {
                (None, None, js_mode)
            }
        };

        // create root function def.
        let mut fd = JSFunctionDef::new(self, None, true, false, filename, 1)?;

        fd.eval_type = eval_type;
        fd.func_name = Atom::CONST__eval_;
        fd.has_this_binding = eval_type != EvalType::Direct;
        fd.attrs.backtrace_barrier = eval_flags & JS_EVAL_FLAG_BACKTRACE_BARRIER != 0;
        fd.attrs.js_mode = js_mode;

        if let Some(bc) = &frame_func_bc {
            debug_assert_eq!(eval_type, EvalType::Direct, "must be direct eval type");
            debug_assert!(frame_module.is_none(), "must not be module");

            fd.attrs.new_target_allowed = bc.attrs.new_target_allowed;
            fd.attrs.super_call_allowed = bc.attrs.super_call_allowed;
            fd.attrs.super_allowed = bc.attrs.super_allowed;
            fd.attrs.arguments_allowed = bc.attrs.arguments_allowed;

            add_closure_variables(self, &mut fd, bc, scope_idx)?;
        } else {
            debug_assert_ne!(eval_type, EvalType::Direct, "can't be direct eval type");
            fd.attrs.arguments_allowed = true;
        }

        fd.module = frame_module;
        if fd.module.is_some() || eval_flags & JS_EVAL_FLAG_ASYNC != 0 {
            fd.in_function_body = true;
            fd.attrs.func_kind = JSFunctionKindEnum::JS_FUNC_ASYNC;
        }

        // add fd body scope.
        // initially there are scope level 0 {-1, -1},
        // so body scope will be level 1.
        fd.body_scope = fd.push_scope(0)? as i32;
        debug_assert_eq!(fd.body_scope, 1);

        {
            // create parse state with root fd, then parse source.
            let mut parser = JSParseState::new(
                self,
                unsafe { NonNull::new_unchecked(fd.as_mut()) },
                source,
                filename,
            );
            debug_assert!(std::ptr::eq(parser.cur_func(), fd.deref()));

            parser.is_module = frame_module.is_some();
            parser.allow_html_comments = frame_module.is_none();

            parser.skip_shebang();
            parser.parse_program()?;

            // parser dropped here, source and filename are still alive.
        }

        // has top-level await?
        let has_await = fd.has_await;

        // compile function def into bytecode.
        let bc = fd.build_bytecode(self)?;

        // XXX: Could add a flag to avoid resolution if necessary
        if let Some(mut md) = frame_module {
            // resolve module
            md.bytecode = JSValue::Bytecode(bc);
            md.set_tla(has_await);
            self.resolve_module(md)?;
            Ok(Compiled::Module(md))
        } else if frame_func_bc.is_some() {
            debug_assert_eq!(eval_type, EvalType::Direct);
            Ok(Compiled::Direct(bc))
        } else {
            Ok(Compiled::GlobalOrIndirect(bc))
        }
    }

    /// compile source, then eval it.
    pub(super) fn eval_internal(
        &mut self,
        this_obj: JSValue,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: u32,
        scope_idx: i32,
    ) -> JSResult {
        #[cfg(debug_assertions)]
        if eval_flags & JS_EVAL_FLAG_COMPILE_ONLY != 0 {
            panic!(
                "don't use JS_EVAL_FLAG_COMPILE_ONLY flag in eval().\nif you want compile only, use ::compile() method."
            );
        }

        match self.compile(source, filename, eval_type, eval_flags, scope_idx)? {
            Compiled::GlobalOrIndirect(bc) => bc.eval(self, this_obj, &[], None),
            Compiled::Direct(bc) => {
                let mut rt = self.rt_ptr();

                bc.eval(
                    self,
                    this_obj,
                    &rt.current_stack_frame()
                        .unwrap()
                        .cur_func
                        .as_object_BCFunction()
                        .copied()
                        .unwrap() // <-- current stack frame function must be a BCFunction
                        .payload()
                        .var_refs,
                    rt.current_stack_frame_mut(),
                )
            }
            Compiled::Module(mut m) => {
                // Problem: m has created function? m was linked?
                // if m has not created function, should use m.eval() instead of m.eval_func()
                debug_assert!(m.func_created);
                m.eval_func(self).map(|p| p.into())
            }
        }
    }

    /// eval source as global type, with global as `this` binding.
    #[inline]
    pub fn eval_global(&mut self, source: &[u8], filename: &str, eval_flags: u32) -> JSResult {
        self.eval_internal(
            self.global_obj().into(),
            source,
            filename,
            EvalType::Global,
            eval_flags,
            -1,
        )
    }

    /// eval `source` as module type, with global as `this` binding.
    pub fn eval_module(
        &mut self,
        source: &[u8],
        filename: &str,
        strip: bool,
    ) -> Result<Promise, i32> {
        // compile source into module, then eval it.
        let mut md = ModuleDef::compile(self, source, filename, strip)?;
        md.set_import_meta(self, filename, true)?;
        md.eval(self)
    }

    /// run script, handle result value with callback. wait till execution terminates.
    pub fn run_wait_with<R>(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: u32,
        handle: impl FnOnce(JSValue) -> R,
    ) -> Result<R, ErrorText> {
        debug_assert!(matches!(eval_type, EvalType::Global | EvalType::Module));

        let mut rt = self.rt_ptr();

        // eval
        let val = if eval_type == EvalType::Module {
            self.eval_module(source, filename, eval_flags & JS_EVAL_FLAG_STRIP != 0)
                .map(|p| p.into())
        } else {
            self.eval_global(source, filename, eval_flags)
        }
        .map_err(|_| {
            let exc = rt.take_exception();
            ErrorText::from(self, exc)
        })?;

        // to set val as gc root - is this nessesary?
        // rt.gc_set_root_value(val, true);

        let mut promise = Promise::try_from(val).ok();
        let mut result = if let Some(p) = promise {
            match p.state() {
                PromiseState::Fulfilled => {
                    promise = None;
                    Ok(p.result())
                }
                PromiseState::Rejected => {
                    promise = None;
                    Err(ErrorText::from(self, p.result()))
                }
                _ => Ok(JSValue::UNDEFINED),
            }
        } else {
            Ok(val)
        };

        // tick to finish
        let exit_code = loop {
            match rt.tick() {
                Ok(TickStatus::Continue) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
                Ok(TickStatus::Stop(code)) => {
                    if let Some(p) = promise {
                        match p.state() {
                            PromiseState::Fulfilled => {
                                result = Ok(p.result());
                            }
                            PromiseState::Rejected => {
                                result = Err(ErrorText::from(self, p.result()));
                            }
                            _ => {
                                self.throw_type_error("Promise is pending");
                                let exc = rt.take_exception();
                                result = Err(ErrorText::from(self, exc));
                            }
                        }
                    }
                    break code;
                }
                Err(_) => {
                    let exc = rt.take_exception();
                    result = Err(ErrorText::from(self, exc));
                    break -1;
                }
            }
        };

        match result {
            Ok(v) => Ok(handle(v)),
            Err(e) => Err(e),
        }
    }

    /// run script, wait till execution terminates.
    pub fn run_wait(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: u32,
    ) -> Result<(), ErrorText> {
        self.run_wait_with(source, filename, eval_type, eval_flags, |_| {})
    }
}

impl JSValue {
    /// eval value:
    /// - String: eval source script and return result,
    /// - otherwise returns as-is.
    pub(crate) fn eval_object(
        &self,
        ctx: &mut ContextImpl,
        this_obj: JSValue,
        eval_type: EvalType,
        eval_flags: u32,
        scope_idx: i32,
    ) -> JSResult {
        if let Self::String(s) = self {
            ctx.eval_internal(
                this_obj,
                s.content().encode_utf8_bytes().as_ref(),
                "<string>",
                eval_type,
                eval_flags,
                scope_idx,
            )
        } else {
            Ok(*self)
        }
    }
}
