// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ops::Deref, ptr::NonNull};

use crate::{
    Context,
    atom::Atom,
    context::{ScopeResult, ScopedContext},
    error::JSResult,
    function::{BCWrapper, Bytecode, FunctionDef, FunctionKind, add_closure_variables},
    module::ModuleDef,
    parse::JSParseState,
    stack_frame::{ExecMode, FrameIndex},
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

bitflags::bitflags! {
    #[repr(transparent)]
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct EvalFlags :u8 {
        /// strict mode
        const STRICT = 1 << 0;
        /// strip debug source
        const STRIP = 1 << 1;

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

impl<'c> ScopedContext<'c> {
    /// Compile `source` to bytecode or module def, don't run.
    ///
    /// In module type, `filename` is used to identify the module.
    #[inline(never)]
    pub(crate) fn compile(
        &mut self,
        caller_frame_idx: FrameIndex,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        flags: EvalFlags,
        var_scope_idx: i32,
    ) -> Result<Compiled, i32> {
        // get (bytecode, module, js_mode) of current stack frame
        let (frame_func_bc, frame_module, js_mode) = if eval_type == EvalType::Direct {
            // Determine the calling function's BCWrapper.
            //
            // The EVAL / APPLY_EVAL opcodes pass the real running frame index
            // (`caller_frame_idx`), which is always valid while a function body
            // is executing. Fall back to the global `active_frame_index`, which
            // may be NULL when clobbered by a nested trampoline call that reset
            // it on return. If neither is available we cannot capture the
            // caller's lexical scope, so we fall back to compiling without a
            // caller frame (the resulting eval degrades to indirect; see
            // `effective_eval_type` below). This avoids panicking when direct
            // eval is reached with no resolvable caller frame.
            let idx = if !caller_frame_idx.is_null() {
                caller_frame_idx
            } else {
                self.rt().active_frame_index
            };
            if idx.is_null() {
                (None, None, ExecMode::empty())
            } else {
                let head = self.rt().stack_frame_arena().frame(idx);
                let cur_func = BCWrapper::try_from(JSValue::from(head.func_obj))
                    .expect("arena frame func_obj must be a BCWrapper");
                let bc = cur_func.payload().bytecode();
                (Some(bc), None, bc.mode())
            }
        } else {
            let mut mode = ExecMode::empty();
            if flags.contains(EvalFlags::STRICT) {
                mode |= ExecMode::STRICT;
            }
            if flags.contains(EvalFlags::STRIP) {
                mode |= ExecMode::STRIP;
            }
            if eval_type == EvalType::Module {
                mode |= ExecMode::STRICT;
                let atom = Atom::from_str(self, filename)?;
                (None, Some(ModuleDef::new(self, atom)?), mode)
            } else {
                (None, None, mode)
            }
        };

        // Effective eval type: a direct eval only captures the caller's lexical
        // scope when we actually resolved a caller frame. Otherwise it degrades
        // to an indirect (global-scope) eval so we never crash on a missing
        // caller frame.
        //
        // Host-requested Global keeps Script semantics: top-level let/const
        // become global lexical bindings (global_var_obj) visible to later
        // scripts, var/function declarations are non-configurable globals, and
        // strict scripts still declare globals (unlike strict indirect eval,
        // which keeps all its declarations local).
        let effective_eval_type = if frame_module.is_some() {
            EvalType::Module
        } else if frame_func_bc.is_some() {
            EvalType::Direct
        } else if eval_type == EvalType::Global {
            EvalType::Global
        } else {
            EvalType::Indirect
        };

        // create root function def.
        let mut fd = FunctionDef::new(self, std::ptr::null_mut(), true, false, filename, 1)?;

        fd.eval_type = effective_eval_type;
        fd.func_name = Atom::CONST__eval_;
        fd.has_this_binding = effective_eval_type != EvalType::Direct;
        fd.attrs.backtrace_barrier = flags.contains(EvalFlags::BACKTRACE_BARRIER);
        fd.attrs.exec_mode = js_mode;

        if let Some(bc) = &frame_func_bc {
            debug_assert_eq!(eval_type, EvalType::Direct, "must be direct eval type");
            debug_assert!(frame_module.is_none(), "must not be module");

            fd.attrs.new_target_allowed = bc.attrs.new_target_allowed;
            fd.attrs.super_call_allowed = bc.attrs.super_call_allowed;
            fd.attrs.super_allowed = bc.attrs.super_allowed;
            fd.attrs.arguments_allowed = bc.attrs.arguments_allowed;

            add_closure_variables(self, &mut fd, bc, var_scope_idx)?;
        } else {
            debug_assert_ne!(
                effective_eval_type,
                EvalType::Direct,
                "can't be direct eval type"
            );
            fd.attrs.arguments_allowed = true;
        }

        fd.module = frame_module;
        if fd.module.is_some() || flags.contains(EvalFlags::COMPILE_TO_ASYNC) {
            fd.in_function_body = true;
            fd.attrs.func_kind = FunctionKind::Async;
        }

        // add fd body scope.
        // initially there are scope level 0 {-1, -1},
        // so body scope will be level 1.
        fd.body_scope = fd.push_scope(0, 0)? as i32;
        debug_assert_eq!(fd.body_scope, 1);

        // parse source in a dedicated scope, then compile to bytecode.
        //
        // The parser child scope MUST outlive `create_bytecode`: during
        // parsing, atoms and constant-pool objects are allocated as LOCAL in
        // this scope. `create_bytecode` consumes them into a `BytecodeImpl`
        // which only becomes LOCAL at its end (`mkbc.rs::alloc_local`). If the
        // child scope is dropped before that, a GC in between would reclaim
        // those objects leaving dangling references (see
        // bugs/parser-gc-atom-collect.md Bug 1A/1B).
        //
        // `JSParseState::finish` returns the inner `ScopedContext` (the child
        // scope) without dropping it, so we can keep it alive across
        // `create_bytecode`. After compilation, the resulting `Bytecode` is
        // promoted to the parent scope (so it survives child_scope drop).
        let (bc, has_await) = {
            let mut cp = self.ctx_ptr;
            let child_scope = cp.new_scope();
            let mut parser = JSParseState::new(
                child_scope,
                unsafe { NonNull::new_unchecked(fd.as_mut()) },
                source,
                filename,
            );
            debug_assert!(std::ptr::eq(parser.cur_func(), fd.deref()));

            parser.is_module = frame_module.is_some();
            parser.allow_html_comments = frame_module.is_none();

            parser.skip_shebang();
            parser.parse_program()?;

            // Recover the child scope (does NOT drop it) and keep it alive.
            let (mut child_scope, _) = parser.finish(());

            // has top-level await?
            let has_await = fd.has_await;

            // compile function def into bytecode while child_scope is alive,
            // so parser-created atoms / cpool targets stay LOCAL-protected
            // throughout the compilation window.
            let bc = fd.create_bytecode(&mut child_scope)?;

            // IMPORTANT: Promote the resulting Bytecode (and thus its BytecodeImpl) from
            // the child scope to the parent scope, so it remains LOCAL after
            // child_scope drops. The atoms/cpool targets it references are
            // kept alive via BytecodeImpl::trace thereafter.
            let bc_val = JSValue::from(bc);
            let bc_val = bc_val.promote(&child_scope);
            let Some(bc) = bc_val.as_bytecode() else {
                unreachable!();
            };

            // child_scope dropped here: atom descriptors / cpool targets that
            // were only referenced by child_scope LOCAL are now either
            // promoted (bc) or traced through BytecodeImpl.
            (bc, has_await)
        };

        #[cfg(debug_assertions)]
        debug_assert!(
            self.gc_heap()
                .current_scope(self.scope_stack_id())
                .unwrap()
                .contains(bc.gc_node_ptr()),
            "{:?}",
            bc.gc_node()
        );

        // XXX: Could add a flag to avoid resolution if necessary
        if let Some(mut md) = frame_module {
            // resolve module
            md.bytecode = JSValue::from(bc);
            md.set_tla(has_await);
            md.resolve_deps(self)?;
            Ok(Compiled::Module(md))
        } else if frame_func_bc.is_some() {
            debug_assert_eq!(eval_type, EvalType::Direct);
            Ok(Compiled::Direct(bc))
        } else {
            Ok(Compiled::GlobalOrIndirect(bc))
        }
    }

    /// Compile source first, then run as function
    #[allow(clippy::too_many_arguments)]
    fn compile_then_run(
        &mut self,
        caller_frame_idx: FrameIndex,
        this_obj: JSValue,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        eval_flags: EvalFlags,
        var_scope_idx: i32,
    ) -> JSResult {
        if self.allow_eval {
            let compiled = self.compile(
                caller_frame_idx,
                source,
                filename,
                eval_type,
                eval_flags,
                var_scope_idx,
            )?;

            match compiled {
                Compiled::GlobalOrIndirect(bc) => {
                    bc.run_as_function(self, FrameIndex::NULL, this_obj, &[])
                }
                Compiled::Direct(bc) => {
                    // Resolve the calling frame the same way `compile` did:
                    // prefer the explicit frame passed by the EVAL opcode, then
                    // the global active_frame_index (which may be clobbered to
                    // NULL by a nested trampoline). Without a valid caller frame
                    // we run the eval as a fresh function without captured vars.
                    let idx = if !caller_frame_idx.is_null() {
                        caller_frame_idx
                    } else {
                        self.rt().active_frame_index
                    };

                    if idx.is_null() {
                        bc.run_as_function(self, FrameIndex::NULL, this_obj, &[])
                    } else {
                        // get captured_vars from the trampoline frame
                        let bfun =
                            BCWrapper::try_from(self.rt().stack_frame_arena().frame(idx).func_obj)
                                .expect("eval: caller must be bytecode function");
                        let captured_vars = bfun.payload().captured_vars();
                        bc.run_as_function(self, idx, this_obj, captured_vars)
                    }
                }
                Compiled::Module(mut md) => {
                    // Problem: m has created function? m was linked?
                    // if m has not created function, should use m.eval() instead of m.eval_func()
                    md.run_function(self).map(|p| p.into())
                }
            }
        } else {
            self.throw_type_error("eval is not supported");
            Err(-1)
        }
    }

    /// Compile source and run to get result.
    ///
    /// Eval in `self` scope, with `eval_type` semantics:
    /// - `Global`: script — top-level let/const become global lexical
    ///   bindings, var/function declarations are non-configurable globals.
    /// - `Module`: compile and evaluate as a module, returns a Promise.
    /// - `Indirect`: indirect eval — all top-level declarations stay local
    ///   in strict mode, var/function declarations are configurable globals.
    /// - `Direct` is not supported here (needs a caller frame); it degrades
    ///   to `Indirect` in `compile`.
    pub fn eval(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        flags: EvalFlags,
    ) -> JSResult {
        debug_assert_ne!(
            eval_type,
            EvalType::Direct,
            "direct eval needs a caller frame, use Global or Indirect"
        );

        if eval_type == EvalType::Module {
            // compile source into module def, then run the module def. returns Promise
            //
            // `filename` is expected to already be a canonical `file://` URL when
            // entering the engine (the host converts the CLI / entry filename to a
            // URL before calling `eval`). It becomes `module_name` verbatim, and
            // `import.meta.url` reuses it via `set_import_meta`.
            let mut md = self.compile_module(source, filename, flags.contains(EvalFlags::STRIP))?;
            md.set_import_meta(self, true)?;
            md.evaluate(self).map(JSValue::from)
        } else {
            // Pass `eval_type` through: Global keeps Script semantics,
            // Indirect (e.g. the Function constructor in bfun.rs) keeps
            // indirect-eval semantics instead of being coerced to Global.
            let global = self.global_obj();
            self.compile_then_run(
                FrameIndex::NULL,
                global.into(),
                source,
                filename,
                eval_type,
                flags,
                -1,
            )
        }
    }

    /// Eval `source` script in `self` scope, wait till all microtasks are finished or execution is interrupted.
    /// Returns the event loop exit status with optional value.
    #[cfg(not(feature = "async_support"))]
    pub fn eval_wait(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        flags: EvalFlags,
    ) -> ExitStatus {
        debug_assert!(matches!(eval_type, EvalType::Global | EvalType::Module));

        let ret_val = match self.eval(source, filename, eval_type, flags) {
            Ok(v) => v,
            Err(_) => {
                let exc = self.rt_mut().take_exception();
                return ExitStatus::Exception(exc);
            }
        };

        // `ret_val` could be used in tick() phase, must ensure it's alive then.
        // Note: ret_val should already be root or local node.
        debug_assert!(ret_val.gc_node().is_none_or(|h| h.is_root_or_local()));

        let mut promise = crate::builtins::promise::Promise::try_from(ret_val).ok();
        let mut result: ExitStatus = if let Some(p) = promise {
            match p.state() {
                crate::builtins::promise::PromiseState::Fulfilled => {
                    promise = None;
                    ExitStatus::Value(p.result())
                }
                crate::builtins::promise::PromiseState::Rejected => {
                    promise = None;
                    ExitStatus::Exception(Exception::from(p.result()))
                }
                _ => ExitStatus::Value(JSValue::UNDEFINED),
            }
        } else {
            ExitStatus::Value(ret_val)
        };

        // tick to finish
        loop {
            match self.rt_mut().tick() {
                crate::runtime::TickStatus::Continue => {
                    std::thread::yield_now();
                    continue;
                }
                crate::runtime::TickStatus::Stop => {
                    if let Some(code) = self.rt_mut().take_interrupt() {
                        log::debug!("Interrupted with code {}", u8::from(code));
                        result = ExitStatus::Interrupted(code);
                    } else if let Some(p) = promise {
                        match p.state() {
                            crate::builtins::promise::PromiseState::Fulfilled => {
                                result = ExitStatus::Value(p.result());
                            }
                            crate::builtins::promise::PromiseState::Rejected => {
                                // keep initial ExitStatus::Exception from pre-tick assessment
                            }
                            crate::builtins::promise::PromiseState::Pending => {
                                #[cfg(debug_assertions)]
                                eprintln!("warn: has pending Promise when exit");
                            }
                        }
                    }
                    break;
                }
            }
        }

        result
    }
}

impl<'c> Context<'c> {
    /// Compile source and run to get result.
    ///
    /// Eval in either global or module mode, eval in `self` scope.
    pub fn eval(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        flags: EvalFlags,
    ) -> JSResult {
        // Refresh stack_top at the outermost eval entry point.
        // At this point all Rust-side initialization (class registration,
        // native module init, etc.) is complete and unwound, so stack_top
        // anchors at the shallowest frame (main level). Subsequent JS execution
        // (including recursive module linking/eval) gets an accurate stack_size budget.
        crate::runtime::JS_UpdateStackTop(self.rt_mut());

        self.with_new_scope(|cx| cx.eval(source, filename, eval_type, flags))
    }

    /// Eval `source` script in `self` scope, wait till all microtasks are finished or execution is interrupted.
    /// Returns the event loop exit status with optional value.
    #[cfg(not(feature = "async_support"))]
    pub fn eval_wait(
        &mut self,
        source: &[u8],
        filename: &str,
        eval_type: EvalType,
        flags: EvalFlags,
    ) -> ExitStatus {
        // Refresh stack_top at the outermost eval entry point.
        // At this point all Rust-side initialization (class registration,
        // native module init, etc.) is complete and unwound, so stack_top
        // anchors at the shallowest frame (main level). Subsequent JS execution
        // (including recursive module linking/eval) gets an accurate stack_size budget.
        crate::runtime::JS_UpdateStackTop(self.rt_mut());

        self.with_new_scope(|cx| cx.eval_wait(source, filename, eval_type, flags))
    }
}

impl JSValue {
    /// eval value:
    /// - String: eval source script and return result,
    /// - otherwise returns as-is.
    pub(crate) fn eval_object(
        &self,
        ctx: &mut ScopedContext,
        caller_frame_idx: FrameIndex,
        this_obj: JSValue,
        eval_type: EvalType,
        eval_flags: EvalFlags,
        var_scope_idx: i32,
    ) -> JSResult {
        // PerformEval step 2: only a primitive String is evaluated; a String
        // object (new String(...)) is returned as-is (`as_string(false)`), not coerced.
        if let Some(s) = self.as_string(false) {
            ctx.compile_then_run(
                caller_frame_idx,
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
