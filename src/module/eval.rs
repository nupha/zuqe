use std::ops::DerefMut;

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
    branch_hints::unlikely,
    context::JSContext,
    error::JSResult,
    eval::{Compiled, JS_EVAL_FLAG_STRIP, JS_EVAL_TYPE_MODULE},
    function::CallContext,
    module::{JSModuleStatus, ModuleDefInner},
    promise::{JSPromiseStateEnum, Promise},
    runtime::js_check_stack_overflow,
    value::JSValue,
};

use super::ModuleDef;

impl ModuleDefInner {
    /// set module status to `evaluated`
    fn set_evaluated(&self, ctx: &JSContext) {
        debug_assert_ne!(
            self.status.get(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATED,
            "already in evaluated state"
        );

        self.status.set(JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

        if unsafe { (*self.promise.as_ptr()).is_some() } {
            debug_assert_eq!(self.cycle_root.borrow().as_ref().unwrap().id(), self.id());

            let func = unsafe { &(*self.resolving_funcs.as_ptr())[0] };
            let _ = func.call(ctx, &JSValue::UNDEFINED, &[JSValue::UNDEFINED]);
        }
    }

    #[inline(always)]
    fn set_status(&self, status: JSModuleStatus) {
        self.status.set(status);
    }
}

impl ModuleDef {
    #[inline(always)]
    pub fn status(&self) -> JSModuleStatus {
        self.0.status.get()
    }

    #[inline(always)]
    fn set_status(&self, status: JSModuleStatus) {
        self.0.set_status(status);
    }

    #[inline(always)]
    fn cycle_root(&self) -> Option<ModuleDef> {
        self.0.cycle_root.borrow().as_ref().cloned()
    }

    fn set_cycle_root(&self, root: ModuleDef) {
        debug_assert!(self.0.cycle_root.borrow().as_ref().is_none());
        self.0.cycle_root.borrow_mut().deref_mut().replace(root);
    }

    #[inline(always)]
    fn async_evaluation(&self) -> bool {
        self.0.async_evaluation.get()
    }

    #[inline(always)]
    fn set_async_evaluation(&self, b: bool) {
        self.0.async_evaluation.set(b)
    }

    /// compile source as module, don't run. `filename` is used to identify the module.
    pub fn compile(
        ctx: &JSContext,
        source: &[u8],
        filename: &str,
        strip: bool,
    ) -> Result<ModuleDef, i32> {
        if let Compiled::Module(md) = ctx.compile(
            source,
            filename,
            JS_EVAL_TYPE_MODULE | if strip { JS_EVAL_FLAG_STRIP } else { 0 },
            -1,
        )? {
            Ok(md)
        } else {
            unreachable!()
        }
    }

    /// Run the <eval> function of the `self` module and of all its dependent modules.
    pub(crate) fn eval(&self, ctx: &JSContext) -> Result<Promise, i32> {
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        let m = match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_LINKED => self.clone(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => self.cycle_root().unwrap(),
            _ => {
                unreachable!()
            }
        };

        // a promise may be created only on the cycle_root of a cycle
        if let Some(p) = m.promise() {
            return Ok(p);
        }

        let (promise, rr) = ctx.create_promise(&JSValue::UNDEFINED)?;
        self.0.promise.replace(Some(promise.clone()));
        self.0.resolving_funcs.replace(rr);

        let mut stack_top: Option<ModuleDef> = None;

        match m.eval_internal(ctx, 0, &mut stack_top) {
            Ok(_) => {
                debug_assert!(matches!(
                    m.status(),
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                debug_assert!(m.eval_exception().is_none());

                if !m.async_evaluation() {
                    debug_assert_eq!(m.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    let func_obj = unsafe { (*m.0.resolving_funcs.as_ptr())[0].clone() }; // resolve func
                    let _ = func_obj.call(
                        ctx,
                        &JSValue::UNDEFINED,
                        std::slice::from_ref(&JSValue::UNDEFINED),
                    );
                }
                debug_assert!(stack_top.is_none());
            }
            Err(result) => {
                while let Some(m1) = stack_top.take() {
                    debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATING);

                    m1.set_status(JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    m1.set_eval_exception(Some(result.clone()));
                    m1.set_cycle_root(m.clone()); // spec bug: should be present

                    stack_top = m1.0.stack_prev.take();
                }

                debug_assert_eq!(m.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

                let func_obj = unsafe { (*m.0.resolving_funcs.as_ptr())[1].clone() }; // reject func
                let exc = m.eval_exception().unwrap();
                let _ = func_obj.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&exc));
            }
        }

        Ok(promise)
    }

    /// InnerModuleEvaluation, returns depth index or Err value.
    /// `index`: depth index
    fn eval_internal(
        &self,
        ctx: &JSContext,
        index: i32,
        stack_top: &mut Option<ModuleDef>,
    ) -> Result<i32, JSValue> {
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(ctx.take_exception());
        }

        match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                return if let Some(exc) = self.eval_exception() {
                    Err(exc)
                } else {
                    Ok(index)
                };
            }
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING => {
                return Ok(index);
            }
            JSModuleStatus::JS_MODULE_STATUS_LINKED => {}
            _ => {
                unreachable!();
            }
        }

        self.set_status(JSModuleStatus::JS_MODULE_STATUS_EVALUATING);
        self.0.dfs_index.set(index);
        self.0.dfs_ancestor_index.set(index);
        self.0.pending_async_dependencies.set(0);

        // push 'self' on stack
        self.0.stack_prev.replace(stack_top.take());
        stack_top.replace(self.clone());

        let mut index = index + 1;

        for dep in self.0.dependencies.borrow().iter() {
            let mut m1 = dep.module.clone().unwrap();

            index = m1.eval_internal(ctx, index, stack_top)?;
            debug_assert!(matches!(
                m1.status(),
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if m1.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
                self.0.dfs_ancestor_index.set(std::cmp::min(
                    self.0.dfs_ancestor_index.get(),
                    m1.0.dfs_ancestor_index.get(),
                ));
            } else {
                m1 = m1.cycle_root().unwrap();
                debug_assert!(matches!(
                    m1.status(),
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                if let Some(exc) = m1.eval_exception() {
                    return Err(exc);
                }
            }

            if m1.async_evaluation() {
                self.0
                    .pending_async_dependencies
                    .set(self.0.pending_async_dependencies.get() + 1);

                let lst = &mut m1.0.async_parent_modules.borrow_mut();
                if lst.len() == lst.capacity() {
                    if lst.try_reserve(1).is_err() {
                        ctx.throw_out_of_memory();
                        return Err(ctx.take_exception());
                    }
                }
                lst.push(self.clone());
            }
        }

        if self.0.pending_async_dependencies.get() > 0 {
            debug_assert!(!self.async_evaluation());
            self.set_async_evaluation(true);
            self.0
                .async_evaluation_timestamp
                .set(ctx.rt().module_async_evaluation_next_timestamp);
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
        } else if self.has_tla() {
            debug_assert!(!self.async_evaluation());
            self.set_async_evaluation(true);
            self.0
                .async_evaluation_timestamp
                .set(ctx.rt().module_async_evaluation_next_timestamp);
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
            self.execute_async(ctx)?;
        } else {
            self.execute_sync(ctx)?;
        }

        let (dfs_ancestor_index, dfs_index) =
            { (self.0.dfs_ancestor_index.get(), self.0.dfs_index.get()) };

        debug_assert!(dfs_ancestor_index <= dfs_index);
        if dfs_index == dfs_ancestor_index {
            loop {
                // pop m1 from stack
                let m1 = stack_top.take().unwrap();
                *stack_top = m1.0.stack_prev.take();

                m1.set_status(if m1.async_evaluation() {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                } else {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                });

                // spec bug: cycle_root must be assigned before the test
                m1.set_cycle_root(self.clone());

                if m1.eq(self) {
                    break;
                }
            }
        }

        Ok(index)
    }

    /// execute module function in async manner
    fn execute_async(&self, ctx: &JSContext) -> Result<(), i32> {
        // Hint: bytecode is now a Function object
        let md_func = unsafe { (*self.0.bytecode.as_ptr()).clone() };
        debug_assert_eq!(
            md_func.as_object().unwrap().class_id(),
            JS_CLASS_BYTECODE_FUNCTION
        );

        // execute bytecode function in async manner.
        let promise = md_func.call_async(ctx, &JSValue::UNDEFINED, &[], 0)?;

        let data = [JSValue::Module(self.clone())];
        promise.then(
            ctx,
            ctx.create_native_function_with_data(
                ModuleDef::execute_async_fulfill_callback,
                0,
                0,
                &data,
            )?
            .into(),
            ctx.create_native_function_with_data(
                ModuleDef::execute_async_reject_callback,
                0,
                0,
                &data,
            )?
            .into(),
        )?;

        Ok(())
    }

    /// async module execution callback
    fn execute_async_fulfill_callback(ctx: &JSContext, cc: CallContext) -> JSResult {
        let JSValue::Module(md) = cc.data(0) else {
            unreachable!()
        };

        if md.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(md.eval_exception().is_some());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            md.status(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(md.eval_exception().is_none());
        debug_assert!(md.async_evaluation());

        md.set_async_evaluation(false);
        md.0.set_evaluated(ctx);

        let mut exec_list = Vec::<ModuleDef>::with_capacity(8);
        gather_module_ancestors(ctx, &md, &mut exec_list)?;

        //
        // let mut exec_list = match module.with_mut(|md| -> Result<(), JSValue> {
        //     if module.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
        //         debug_assert!(md.eval_has_exception);
        //         return Err(JSValue::UNDEFINED);
        //     }
        //
        //     debug_assert_eq!(md.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC);
        //     debug_assert!(!md.eval_has_exception);
        //     debug_assert!(md.async_evaluation);
        //
        //     md.async_evaluation = false;
        //     md.set_evaluated(ctx);
        //     Ok(())
        // }) {
        //     Ok(_) => {
        //         let mut lst = Vec::<ModuleDef>::with_capacity(8);
        //         unsafe {
        //             gather_module_ancestors(ctx, &module, &mut lst)?;
        //         }
        //         lst
        //     }
        //     Err(v) => {
        //         return Ok(v);
        //     }
        // };

        // sort by async_evaluation timestamp
        exec_list.sort_by(|m1, m2| {
            let t1 = m1.0.async_evaluation_timestamp.get();
            let t2 = m2.0.async_evaluation_timestamp.get();
            t1.cmp(&t2)
        });

        for m in exec_list {
            if m.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!(m.eval_exception().is_some());
            } else if m.has_tla() {
                m.execute_async(ctx)?;
            } else {
                match m.execute_sync(ctx) {
                    Ok(_) => {
                        m.0.set_evaluated(ctx);
                    }
                    Err(e) => {
                        let arglist = [e];
                        let mut data = [JSValue::Module(m.clone())];
                        Self::execute_async_reject_callback(
                            ctx,
                            CallContext::new(ctx, JSValue::UNDEFINED, &arglist, 0, Some(&mut data)),
                        )?;
                    }
                }
            }
        }

        Ok(JSValue::UNDEFINED)
    }

    /// async module execution reject callback
    fn execute_async_reject_callback(ctx: &JSContext, cc: CallContext) -> JSResult {
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        let md = if let JSValue::Module(m) = cc.data(0) {
            m
        } else {
            unreachable!()
        };

        let err_val = cc.arg(0);

        if md.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(md.eval_exception().is_some());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            md.status(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(md.eval_exception().is_none());
        debug_assert!(md.async_evaluation());

        md.set_eval_exception(Some(err_val.clone()));
        md.set_status(JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

        for m in md.0.async_parent_modules.borrow().iter() {
            let mut data = [JSValue::Module(m.clone())];
            let cc = CallContext::new(
                ctx,
                JSValue::UNDEFINED,
                std::slice::from_ref(&err_val),
                0,
                Some(&mut data),
            );
            Self::execute_async_reject_callback(ctx, cc).unwrap();
        }

        if md.promise().is_some() {
            debug_assert_eq!(md.cycle_root().unwrap().id(), md.id());

            let func = unsafe { &(*md.0.resolving_funcs.as_ptr())[1] };
            let _ = func.call(ctx, &JSValue::UNDEFINED, std::slice::from_ref(&err_val));
        }

        Ok(JSValue::UNDEFINED)
    }

    fn execute_sync(&self, ctx: &JSContext) -> Result<(), JSValue> {
        if let Some(f) = self.0.init_func {
            // is native module: no asynchronous execution
            if unlikely(f(ctx, self) < 0) {
                Err(ctx.take_exception())
            } else {
                Ok(())
            }
        } else {
            // is bytecode function
            let func_obj = unsafe { (*self.0.bytecode.as_ptr()).clone() };
            debug_assert_eq!(
                func_obj.as_object().unwrap().class_id(),
                JS_CLASS_BYTECODE_FUNCTION
            );

            // eval bytecode function
            let promise = func_obj
                .call_async(ctx, &JSValue::UNDEFINED, &[], 0)
                .map_err(|_| ctx.take_exception())?;

            match promise.state() {
                JSPromiseStateEnum::JS_PROMISE_FULFILLED => Ok(()),
                JSPromiseStateEnum::JS_PROMISE_REJECTED => Err(promise.result()),
                JSPromiseStateEnum::JS_PROMISE_PENDING => {
                    ctx.throw_type_error("promise is pending");
                    Err(ctx.take_exception())
                }
            }
        }
    }

    /// test if module `self` is in execution list `exec_list
    pub fn in_execution_list(&mut self, exec_list: &mut ExecModuleList) -> bool {
        for i in 0..exec_list.count {
            if unsafe { *exec_list.tab.offset(i as isize) } == self {
                return true;
            }
        }

        false
    }
}

impl JSContext {
    /// eval `source` as module.
    pub fn eval_module(&self, source: &[u8], filename: &str, strip: bool) -> Result<Promise, i32> {
        //
        // compile module
        //
        let md = ModuleDef::compile(self, source, filename, strip)?;
        md.0.set_import_meta(self, filename, true)?;

        //
        // eval module function
        //
        md.create_module_function(self)?; // make module function
        md.link(self)?; // resolving imports, exports
        md.eval(self) // evaluate module function
    }
}

pub struct ExecModuleList {
    pub tab: *mut *mut ModuleDef,
    pub count: i32,
    pub size: i32,
}

fn gather_module_ancestors(
    ctx: &JSContext,
    module: &ModuleDef,
    result: &mut Vec<ModuleDef>,
) -> Result<(), i32> {
    if js_check_stack_overflow(&ctx.rt(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    for m in module.0.async_parent_modules.borrow().iter() {
        if !result.iter().any(|x| x.id() == m.id())
            && m.cycle_root()
                .map_or(false, |x| x.eval_exception().is_some())
        {
            debug_assert_eq!(
                m.status(),
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            );
            debug_assert!(m.async_evaluation());
            debug_assert!(m.0.pending_async_dependencies.get() > 0);

            let adep = m.0.pending_async_dependencies.get();
            m.0.pending_async_dependencies.set(adep - 1);
            if adep == 1 {
                // pending_async_dependencies == 0
                if result.len() == result.capacity() {
                    if result.try_reserve(8).is_err() {
                        ctx.throw_out_of_memory();
                        return Err(-1);
                    }
                }

                result.push(m.clone());

                if !m.has_tla() {
                    gather_module_ancestors(ctx, m, result)?;
                }
            }
        }
    }

    Ok(())
}
