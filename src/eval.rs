// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ops::Deref, ptr::NonNull};

use crate::{
    atom::Atom,
    builtins::promise::{Promise, PromiseState},
    context::{ContextImpl, ContextScope},
    error::{ErrorText, JSResult},
    function::{Bytecode, FunctionDef, FunctionKind, add_closure_variables},
    module::ModuleDef,
    parse::JSParseState,
    runtime::TickStatus,
    stack_frame::ExecMode,
    value::{JSValue, Value},
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

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct EvalFlags :u8 {
        /// strict mode
        const STRICT = 1 << 0;
        /// strip debug source
        const STRIP = 1 << 1;
        // /// indicates dedicated stack frame for async function execution
        // const ASYNC = 1 << 2;

        const BACKTRACE_BARRIER = 1 << 4;
        /// compile script as root async function,
        /// so that top-level-await is allowed.
        const COMPILE_TO_ASYNC = 1 << 5;
    }
}

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
    /// - `filename` is used to identify the module.
    pub(crate) fn compile_source(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: EvalFlags,
        scope_idx: i32,
    ) -> Result<Compiled, i32> {
        #[cfg(debug_assertions)]
        let _check_scope_level = {
            let l = self.gc_heap().scope_max_depth();
            debug_assert!(l > 0, "expect gc scope for compile_source");

            let rt = self.rt_ptr();
            crate::defer_fn!({
                debug_assert_eq!(rt.gc_heap().scope_max_depth(), l);
            })
        };

        // get (bytecode, module, js_mode) of current stack frame
        let (frame_func_bc, frame_module, js_mode) = if eval_type == EvalType::Direct {
            let cur_func = self
                .rt_ref()
                .current_stack_frame()
                .and_then(|sf| sf.func_obj.as_BCWrapper())
                .unwrap(); // <-- stack function must be a BCWrapper object

            let bc = cur_func.payload().bytecode();
            let mode = bc.mode();

            (Some(bc), None, mode)
        } else {
            let mut mode = ExecMode::empty();
            if eval_flags.contains(EvalFlags::STRICT) {
                mode |= ExecMode::STRICT;
            }
            if eval_flags.contains(EvalFlags::STRIP) {
                mode |= ExecMode::STRIP;
            }
            if eval_type == EvalType::Module {
                let name = Atom::new(self.rt_ptr(), filename)?;
                mode |= ExecMode::STRICT;
                (None, Some(ModuleDef::new(self, name)?), mode)
            } else {
                (None, None, mode)
            }
        };

        // create root function def.
        let mut fd = FunctionDef::new(self.into(), None, true, false, filename, 1)?;

        fd.eval_type = eval_type;
        fd.func_name = Atom::CONST__eval_;
        fd.has_this_binding = eval_type != EvalType::Direct;
        fd.attrs.backtrace_barrier = eval_flags.contains(EvalFlags::BACKTRACE_BARRIER);
        fd.attrs.exec_mode = js_mode;

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
        if fd.module.is_some() || eval_flags.contains(EvalFlags::COMPILE_TO_ASYNC) {
            fd.in_function_body = true;
            fd.attrs.func_kind = FunctionKind::Async;
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

            // parser is dropped here, source and filename are still alive.
        }

        // has top-level await?
        let has_await = fd.has_await;

        // compile function def into bytecode.
        let bc = fd.create_bytecode(self)?;

        #[cfg(debug_assertions)]
        debug_assert!(
            self.gc_heap()
                .current_scope()
                .unwrap()
                .contains(bc.gc_node_ptr()),
            "{:?}",
            bc.gc_node()
        );

        // XXX: Could add a flag to avoid resolution if necessary
        if let Some(mut md) = frame_module {
            // resolve module
            md.bytecode = JSValue::Bytecode(bc);
            md.set_tla(has_await);
            self.resolve_module(md)?;

            // // Note: promote return bytecode to upper gc scope.
            // self.gc_heap()
            //     .current_gc_scope()
            //     .unwrap()
            //     .promote(md.gc_node_ptr());
            Ok(Compiled::Module(md))
        } else {
            // // Note: promote return bytecode to upper gc scope.
            // self.gc_heap()
            //     .current_gc_scope()
            //     .unwrap()
            //     .promote(bc.gc_node_ptr());
            if frame_func_bc.is_some() {
                debug_assert_eq!(eval_type, EvalType::Direct);
                Ok(Compiled::Direct(bc))
            } else {
                Ok(Compiled::GlobalOrIndirect(bc))
            }
        }
    }

    pub(super) fn eval_internal(
        &mut self,
        this_obj: JSValue,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: EvalFlags,
        var_scope_idx: i32,
    ) -> JSResult {
        debug_assert!(self.gc_heap().scope_max_depth() > 0);

        if self.allow_eval {
            match self.compile_source(source, filename, eval_type, eval_flags, var_scope_idx)? {
                Compiled::GlobalOrIndirect(bc) => bc.run_as_function(self, this_obj, &[], None),
                Compiled::Direct(bc) => {
                    let mut rt = self.rt_ptr();
                    bc.run_as_function(
                        self,
                        this_obj,
                        rt.current_stack_frame()
                            .unwrap()
                            .func_obj
                            .as_BCWrapper()
                            .copied()
                            .unwrap() // <-- stack function must be a BCWrapper
                            .payload()
                            .closure_var_refs(),
                        rt.current_stack_frame_mut(),
                    )
                }
                Compiled::Module(mut md) => {
                    // Problem: m has created function? m was linked?
                    // if m has not created function, should use m.eval() instead of m.eval_func()
                    md.eval_func(self).map(|p| p.into())
                }
            }
        } else {
            self.throw_type_error("eval is not supported");
            Err(-1)
        }
    }

    /// Eval `source` as module, with global object as `this` binding.
    ///
    /// Compile source into module def, then eval it. Returns Promise.
    pub(crate) fn eval_module(
        &mut self,
        source: &[u8],
        filename: &str,
        strip: bool,
    ) -> Result<Promise, i32> {
        let mut md = ModuleDef::compile(self, source, filename, strip)?;
        md.set_import_meta(self, filename, true)?;
        md.eval(self)
    }
}

impl<'a> ContextScope<'a> {
    /// Eval `source` as global type, with global object as `this` binding.
    /// Execute in `self` scope, returned value will be promoted to upper scope.
    pub fn eval_global(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_flags: EvalFlags,
    ) -> JSResult {
        let global = self.global_obj();

        let ret_val = self.eval_internal(
            global.into(),
            source,
            filename,
            EvalType::Global,
            eval_flags,
            -1,
        )?;

        self.set_return_value(ret_val);
        Ok(ret_val)
    }

    /// Eval script `source`, wait till execution terminates, returns the result value.
    /// Execute in `self` scope, returned value is valid at least in `self` scope.
    pub fn run_wait(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: EvalFlags,
    ) -> Result<Value<'a>, ErrorText> {
        self.do_run_wait(source, filename, eval_type, eval_flags)
            .map(|v| v.into())
    }

    /// run `source` script, wait till execution terminates, returns the result value.
    fn do_run_wait(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: EvalFlags,
    ) -> Result<JSValue, ErrorText> {
        debug_assert!(matches!(eval_type, EvalType::Global | EvalType::Module));

        let val = if eval_type == EvalType::Module {
            self.eval_module(source, filename, eval_flags.contains(EvalFlags::STRIP))
                .map(|p| p.into())
        } else {
            self.eval_global(source, filename, eval_flags)
        }
        .map_err(|_| {
            let exc = self.rt_mut().take_exception();
            ErrorText::from(self, exc)
        })?;

        #[cfg(debug_assertions)]
        debug_assert!(
            val.gc_node().is_none_or(|n| n.is_local() || n.is_root()),
            "eval return value must be local: {val:?}"
        );

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

        #[cfg(debug_assertions)]
        debug_assert!(
            result
                .as_ref()
                .ok()
                .is_none_or(|v| v.gc_node().is_none_or(|n| n.is_local() || n.is_root())),
            "Promise result value must be local"
        );

        // tick to finish
        let exit_code = loop {
            match self.rt_mut().tick() {
                Ok(TickStatus::Continue) => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Ok(TickStatus::Stop(code)) => {
                    if let Some(p) = promise {
                        match p.state() {
                            PromiseState::Fulfilled => {
                                result = Ok(p.result());
                            }
                            PromiseState::Rejected => {
                                // no need to return reject value
                                // result = Err(ErrorText::from(self, p.result()));
                            }
                            PromiseState::Pending => {
                                #[cfg(debug_assertions)]
                                eprintln!("warn: has pending Promise when exit");
                                // self.throw_type_error("Promise is pending");
                                // let exc = rt.take_exception();
                                // result = Err(ErrorText::from(self, exc));
                            }
                        }
                    }
                    break code;
                }
                Err(_) => {
                    let exc = self.rt_mut().take_exception();
                    result = Err(ErrorText::from(self, exc));
                    break -1;
                }
            }
        };

        match result {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        }
    }
}

impl JSValue {
    /// eval value:
    /// - String: eval source script and return result,
    /// - otherwise returns as-is.
    pub(crate) fn eval_object(
        &self,
        ctx: &mut ContextScope,
        this_obj: JSValue,
        eval_type: EvalType,
        eval_flags: EvalFlags,
        var_scope_idx: i32,
    ) -> JSResult {
        if let Self::String(s) = self {
            ctx.eval_internal(
                this_obj,
                s.content().encode_utf8_bytes(true).as_ref(),
                "<string>",
                eval_type,
                eval_flags,
                var_scope_idx,
            )
        } else {
            Ok(*self)
        }
    }
}
