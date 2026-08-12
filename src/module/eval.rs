// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::GcNode;
use url::Url;

use crate::{
    Atom, Callback, ErrorKind, Exception, JS_CLASS_BYTECODE_FUNCTION,
    builtins::promise::{Promise, PromiseState},
    context::ScopedContext,
    error::JSResult,
    eval::{CompiledBytecode, EvalFlags, EvalType},
    function::{CallContext, NativeFunction},
    runtime::js_check_stack_overflow,
    stack_frame::FrameIndex,
    utils::unlikely,
    value::JSValue,
};

use super::{JSModuleStatus, ModuleDef, ModuleDefImpl};

impl ModuleDefImpl {
    #[inline(always)]
    pub const fn status(&self) -> JSModuleStatus {
        self.status
    }

    /// Set module status to EVALUATED and clear async_evaluation.
    ///
    /// These two fields MUST always be set together — a module that has
    /// finished evaluating (whether normally or via rejection) cannot
    /// have async_evaluation = true, otherwise dependent modules would
    /// incorrectly treat it as having pending async dependencies.
    #[inline(always)]
    fn set_status_evaluated(&mut self) {
        debug_assert_ne!(
            self.status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATED,
            "already in evaluated state"
        );
        self.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
        self.async_evaluation = false;
    }

    /// set module status to `evaluated`, resolve module promise
    fn set_evaluated(&mut self, ctx: &mut ScopedContext) {
        self.set_status_evaluated();

        if self.promise.is_some() {
            debug_assert_eq!(self.cycle_root, Some(ModuleDef::from(self.gc_ref())));
            let _ = self.resolving_funcs[0].call(ctx, JSValue::UNDEFINED, &[JSValue::UNDEFINED]);
        }
    }
    #[inline(always)]
    const fn cycle_root(&self) -> Option<ModuleDef> {
        self.cycle_root
    }

    fn set_cycle_root(&mut self, root: ModuleDef) {
        debug_assert!(self.cycle_root.is_none());
        self.cycle_root.replace(root);
    }

    #[inline(always)]
    const fn async_evaluation(&self) -> bool {
        self.async_evaluation
    }

    #[inline(always)]
    const fn set_async_evaluation(&mut self, b: bool) {
        self.async_evaluation = b;
    }
}

impl ModuleDef {
    /// Evaluate the module def to promised module name space object.
    ///
    /// Proceeding in following steps:
    ///
    /// * create module function,
    /// * link modules,
    /// * run module function, returns Promise of name space object.
    pub fn evaluate(&mut self, ctx: &mut ScopedContext) -> Result<Promise, i32> {
        self.create_module_function(ctx)?; // make module function
        self.link(ctx)?; // resolving imports, exports
        self.run_function(ctx) // run module function
    }

    /// Run the function of the module and of all its dependent modules.
    pub(crate) fn run_function(&mut self, ctx: &mut ScopedContext) -> Result<Promise, i32> {
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        let mut md = match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_LINKED => *self,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => self.cycle_root().unwrap(),
            s => {
                unreachable!("status={:?}", s);
            }
        };

        // a promise may be created only on the cycle_root of a cycle
        if let Some(p) = md.promise {
            return Ok(p);
        }

        let (promise, rr) = Promise::create(ctx)?;
        md.promise.replace(promise);
        md.resolving_funcs = rr;

        let mut stack_top: Option<ModuleDef> = None;

        match md.run_internal(ctx, 0, &mut stack_top) {
            Ok(_) => {
                debug_assert!(matches!(
                    md.status(),
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                debug_assert!(md.get_eval_error().is_undefined());

                if !md.async_evaluation() {
                    debug_assert_eq!(md.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    let _ = md.resolving_funcs[0].call(
                        ctx,
                        JSValue::UNDEFINED,
                        std::slice::from_ref(&JSValue::UNDEFINED),
                    );
                }

                debug_assert!(stack_top.is_none());
            }
            Err(err) => {
                while let Some(mut m1) = stack_top.take() {
                    debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATING);

                    m1.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
                    m1.set_eval_error(err);
                    m1.set_cycle_root(md); // spec bug: should be present

                    stack_top = m1.stack_prev.take();
                }

                debug_assert_eq!(md.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

                // reject
                let err_val = md.get_eval_error();
                let _ = md.resolving_funcs[1].call(
                    ctx,
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                );
            }
        }

        Ok(promise)
    }

    /// InnerModuleEvaluation, returns depth index or error value.
    /// `index`: depth index
    ///
    /// Iterative version: uses an explicit worklist instead of recursion
    /// to avoid C stack overflow on deep module dependency trees.
    fn run_internal(
        &mut self,
        ctx: &mut ScopedContext,
        index: i32,
        stack_top: &mut Option<ModuleDef>,
    ) -> Result<i32, JSValue> {
        /// Per-module state during iterative evaluation.
        struct EvalState {
            module: ModuleDef,
            /// Index into `module.dependencies` to track which dependency is next.
            dep_index: usize,
            /// Whether the entry phase (overflow check, status init, stack push) is done.
            entered: bool,
        }

        let mut worklist: Vec<EvalState> = Vec::new();
        worklist.push(EvalState {
            module: *self,
            dep_index: 0,
            entered: false,
        });

        let mut result: Result<(), JSValue> = Ok(());
        let mut index = index;

        while !worklist.is_empty() && result.is_ok() {
            let top = worklist.len() - 1;

            // === ENTER phase ===
            if !worklist[top].entered {
                worklist[top].entered = true;

                if js_check_stack_overflow(ctx.rt(), 0) {
                    result = Err(ctx
                        .create_error_object(
                            ErrorKind::Internal,
                            "stack overflow",
                            ctx.throw_need_backtrace(),
                        )
                        .map_or_else(|| Exception::OutOfMemory.to_value(ctx), JSValue::from));
                    break;
                }

                // Status check (same as recursive early return)
                match worklist[top].module.status() {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                        let err = worklist[top].module.get_eval_error();
                        if !err.is_undefined() {
                            debug_assert!(!err.is_null());
                            result = Err(err);
                            break;
                        }
                        // Already processed — pop immediately
                        worklist.pop();
                        continue;
                    }
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING => {
                        // Already being evaluated in another path (cycle)
                        worklist.pop();
                        continue;
                    }
                    JSModuleStatus::JS_MODULE_STATUS_LINKED => {}
                    _ => {
                        unreachable!();
                    }
                }

                worklist[top].module.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATING;
                worklist[top].module.dfs_index = index;
                worklist[top].module.dfs_ancestor_index = index;
                worklist[top].module.pending_async_dependencies = 0;

                // push 'self' on stack
                worklist[top].module.stack_prev = stack_top.take();
                stack_top.replace(worklist[top].module);

                index += 1;
            }

            // === Check previous dependency's result ===
            // This mirrors the post-recursive-call logic in the recursive version.
            if worklist[top].dep_index > 0 {
                let mut prev = worklist[top].module.dependencies[worklist[top].dep_index - 1]
                    .module
                    .unwrap();

                if prev.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
                    // Child is still evaluating (in same cycle): Tarjan ancestor update
                    worklist[top].module.dfs_ancestor_index = std::cmp::min(
                        worklist[top].module.dfs_ancestor_index,
                        prev.dfs_ancestor_index,
                    );
                } else {
                    // Child is EVALUATING_ASYNC or EVALUATED: check for errors
                    prev = prev.cycle_root().unwrap();
                    debug_assert!(matches!(
                        prev.status(),
                        JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                    ));
                    let err = prev.get_eval_error();
                    if !err.is_undefined() {
                        result = Err(err);
                        break;
                    }
                }

                // Async dependency tracking
                if prev.async_evaluation() {
                    worklist[top].module.pending_async_dependencies += 1;

                    if prev.async_parent_modules.len() == prev.async_parent_modules.capacity()
                        && prev.async_parent_modules.try_reserve(1).is_err()
                    {
                        result = Err(ctx
                            .create_error_object(
                                ErrorKind::Internal,
                                "stack overflow",
                                ctx.throw_need_backtrace(),
                            )
                            .map_or_else(|| Exception::OutOfMemory.to_value(ctx), JSValue::from));
                        break;
                    }

                    prev.async_parent_modules.push(worklist[top].module);
                }
            }

            if result.is_err() {
                break;
            }

            // === All dependencies done → post-processing ===
            if worklist[top].dep_index >= worklist[top].module.dependencies.len() {
                let cur_module = &mut worklist[top].module;

                // `set_import_meta` is only reached for the main entry and
                // dynamic imports. Wire up `import.meta` for static imports too
                // so `import.meta.url === module_name` holds for every module
                // (native ones have no bytecode to read it lazily). Skip when
                // already set (main entry / dynamic import) to keep a single
                // call per module and preserve the `main` flag.
                if cur_module.meta_obj.is_none()
                    && cur_module.set_import_meta(ctx, false).is_err()
                {
                    result = Err(ctx.take_exception().to_value(ctx));
                    break;
                }

                // Module execution (same as recursive post-deps logic)
                if cur_module.pending_async_dependencies > 0 {
                    debug_assert!(!cur_module.async_evaluation());
                    cur_module.set_async_evaluation(true);
                    cur_module.async_evaluation_timestamp =
                        ctx.rt_ptr().module_async_evaluation_next_timestamp;
                    ctx.rt_ptr().module_async_evaluation_next_timestamp += 1;
                } else if cur_module.has_tla() {
                    debug_assert!(!cur_module.async_evaluation());
                    cur_module.set_async_evaluation(true);
                    cur_module.async_evaluation_timestamp =
                        ctx.rt_ptr().module_async_evaluation_next_timestamp;
                    ctx.rt_ptr().module_async_evaluation_next_timestamp += 1;
                    match cur_module.execute_async_module(ctx) {
                        Ok(()) => {}
                        Err(e) => {
                            result = Err(JSValue::from(e));
                            break;
                        }
                    }
                } else {
                    match cur_module.execute_sync_module(ctx) {
                        Ok(()) => {}
                        Err(e) => {
                            result = Err(e);
                            break;
                        }
                    }
                }

                if result.is_err() {
                    break;
                }

                // Tarjan SCC: if this module is the root of a strongly connected component,
                // pop all modules in the cycle from stack_top.
                if cur_module.dfs_index == cur_module.dfs_ancestor_index {
                    loop {
                        let mut md = stack_top.take().unwrap();
                        *stack_top = md.stack_prev.take();

                        md.status = if md.async_evaluation() {
                            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        } else {
                            JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                        };

                        // spec bug: cycle_root must be assigned before the test
                        md.set_cycle_root(*cur_module);

                        if md.eq(cur_module) {
                            break;
                        }
                    }
                }

                // Pop from worklist without touching stack_top.
                // For non-SCC-root modules, stack_top cleanup is handled by the SCC root's pop.
                // For SCC-root modules, the pop above already restored stack_top.
                worklist.pop();
                continue;
            }

            // === PROCESS next dependency ===
            {
                let dep_idx = worklist[top].dep_index;
                let child = worklist[top].module.dependencies[dep_idx].module.unwrap();

                worklist[top].dep_index += 1;
                worklist.push(EvalState {
                    module: child,
                    dep_index: 0,
                    entered: false,
                });
            }
        }

        result.map(|_| index)
    }

    /// execute module function in async manner
    fn execute_async_module(&self, ctx: &mut ScopedContext) -> Result<(), i32> {
        debug_assert_eq!(
            self.bytecode.as_object().unwrap().class_id(),
            JS_CLASS_BYTECODE_FUNCTION,
            "expect BytecodeFunction object"
        );

        // execute module function in async manner.
        let md_func = self.bytecode;
        let mut promise = md_func.call_async(ctx, JSValue::UNDEFINED, &[], 0)?;

        // Hint: `promise.then()` don't call bytecode execution inside, won't trigger gc collection,
        // protect promise, success, fail nodes is not required.
        let success = NativeFunction::create(
            ctx,
            Callback::Func(ModuleDef::execute_async_fulfilled),
            Atom::CONST_empty_string,
            0,
            0,
            ctx.function_proto(),
            Some([JSValue::from(*self)].into()),
        )?;
        let fail = NativeFunction::create(
            ctx,
            Callback::Func(ModuleDef::execute_async_rejected),
            Atom::CONST_empty_string,
            0,
            0,
            ctx.function_proto(),
            Some([JSValue::from(*self)].into()),
        )?;
        promise.then(ctx, success.into(), fail.into())?;

        Ok(())
    }

    /// async module execution callback
    fn execute_async_fulfilled(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
        let Some(mut md) = cc.data(0).as_module() else {
            unreachable!()
        };

        if md.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(!md.get_eval_error().is_undefined());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(md.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC);
        debug_assert!(md.get_eval_error().is_undefined());
        debug_assert!(md.async_evaluation());

        md.set_evaluated(ctx);

        let mut exec_list = Vec::<ModuleDef>::with_capacity(8);
        gather_module_ancestors(ctx, md, &mut exec_list)?;

        // sort by async_evaluation timestamp
        exec_list.sort_by(|m1, m2| {
            let t1 = m1.async_evaluation_timestamp;
            let t2 = m2.async_evaluation_timestamp;
            t1.cmp(&t2)
        });

        for mut m in exec_list {
            if m.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!(!m.get_eval_error().is_undefined());
            } else if m.has_tla() {
                m.execute_async_module(ctx)?;
            } else {
                match m.execute_sync_module(ctx) {
                    Ok(_) => {
                        m.set_evaluated(ctx);
                    }
                    Err(e) => {
                        Self::execute_async_rejected(
                            ctx,
                            CallContext::new(
                                JSValue::UNDEFINED,
                                std::slice::from_ref(&e),
                                0,
                                Some(&mut [JSValue::from(m)]),
                            ),
                        )?;
                    }
                }
            }
        }

        Ok(JSValue::UNDEFINED)
    }

    /// async module execution reject callback
    fn execute_async_rejected(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
        let Some(mut md) = cc.data(0).as_module() else {
            unreachable!()
        };
        let err_val = cc.arg(0);

        if md.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(!md.get_eval_error().is_undefined());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            md.status(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(md.get_eval_error().is_undefined());
        debug_assert!(md.async_evaluation());

        md.set_eval_error(err_val);
        md.set_status_evaluated();

        // DON'T FOLLOWING CHANGE ORDER

        // Per spec (AsyncModuleExecutionRejected):
        // 9. Reject own TopLevelCapability first
        if md.promise.is_some() {
            debug_assert_eq!(md.cycle_root(), Some(md));
            let func = md.resolving_funcs[1];
            let _ = func.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&err_val));
        }
        // 10. Then propagate rejection to async parent modules (leaf-to-root order)
        for m in md.async_parent_modules.iter() {
            Self::execute_async_rejected(
                ctx,
                CallContext::new(
                    JSValue::UNDEFINED,
                    std::slice::from_ref(&err_val),
                    0,
                    Some(&mut [JSValue::from(*m)]),
                ),
            )
            .unwrap();
        }

        Ok(JSValue::UNDEFINED)
    }

    fn execute_sync_module(&self, ctx: &mut ScopedContext) -> Result<(), JSValue> {
        if let Some(f) = self.init_func.as_ref() {
            // is native module: no asynchronous execution
            if unlikely(f(ctx, self).is_err()) {
                Err(ctx.take_exception().to_value(ctx))
            } else {
                Ok(())
            }
        } else {
            // is bytecode function
            let bc = self.bytecode;
            debug_assert!(bc.as_object_BCWrapper().is_some());

            // eval bytecode function
            let promise = bc
                .call_async(ctx, JSValue::UNDEFINED, &[], 0)
                .map_err(|_| ctx.take_exception().to_value(ctx))?;

            match promise.state() {
                PromiseState::Fulfilled => Ok(()),
                PromiseState::Rejected => Err(promise.result()),
                PromiseState::Pending => Err(ctx
                    .create_error_object(
                        ErrorKind::Type,
                        "promise is pending",
                        ctx.throw_need_backtrace(),
                    )
                    .map_or_else(|| Exception::OutOfMemory.to_value(ctx), JSValue::from)),
            }
        }
    }
}

fn gather_module_ancestors(
    ctx: &mut ScopedContext,
    module: ModuleDef,
    exec_list: &mut Vec<ModuleDef>,
) -> Result<(), i32> {
    if js_check_stack_overflow(ctx.rt(), 0) {
        return Err(ctx.throw_stack_overflow());
    }

    for mut m in module.async_parent_modules.iter().copied() {
        if !exec_list.contains(&m)
            && m.cycle_root()
                .is_some_and(|x| x.get_eval_error().is_undefined())
        {
            debug_assert_eq!(
                m.status(),
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            );
            debug_assert!(m.async_evaluation());
            debug_assert!(m.get_eval_error().is_undefined());
            debug_assert!(m.pending_async_dependencies > 0);

            let adep = m.pending_async_dependencies;
            m.pending_async_dependencies = adep - 1;
            if adep == 1 {
                // pending_async_dependencies == 0
                if exec_list.len() == exec_list.capacity() && exec_list.try_reserve(8).is_err() {
                    return Err(ctx.throw_out_of_memory());
                }

                exec_list.push(m);

                if !m.has_tla() {
                    gather_module_ancestors(ctx, m, exec_list)?;
                }
            }
        }
    }

    Ok(())
}

impl<'c> ScopedContext<'c> {
    /// Compile `module_source` in module mode, return a module def. `module_url` is used to
    /// identify the module (module cache key and `import.meta.url` base).
    pub fn compile_module(
        &mut self,
        module_source: &[u8],
        module_url: &Url,
        strip: bool,
    ) -> Result<ModuleDef, i32> {
        if let CompiledBytecode::Module(md) = self.compile(
            FrameIndex::NULL,
            module_source,
            module_url.as_str(),
            EvalType::Module,
            if strip {
                EvalFlags::STRIP
            } else {
                EvalFlags::empty()
            },
            -1,
        )? {
            Ok(md)
        } else {
            unreachable!()
        }
    }
}
