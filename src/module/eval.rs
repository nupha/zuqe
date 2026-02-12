use std::{cell::RefCell, rc::Rc};

use crate::{
    Callback, JS_CLASS_BYTECODE_FUNCTION,
    builtins::promise::{Promise, PromiseState},
    context::ContextImpl,
    error::JSResult,
    eval::{Compiled, EvalFlags, EvalType},
    function::{CallContext, NativeFunction},
    module::{JSModuleStatus, ModuleDefImpl},
    runtime::js_check_stack_overflow,
    utils::unlikely,
    value::JSValue,
};

use super::ModuleDef;

impl ModuleDefImpl {
    #[inline(always)]
    pub const fn status(&self) -> JSModuleStatus {
        self.status
    }

    /// set module status to `evaluated`
    fn set_evaluated(&mut self, ctx: &ContextImpl) {
        debug_assert_ne!(
            self.status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATED,
            "already in evaluated state"
        );

        self.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

        if self.promise.is_some() {
            debug_assert_eq!(
                self.cycle_root,
                Some(unsafe { ModuleDef::from_ref_unchecked(self) })
            );
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
    /// compile `source` to module def.
    /// `filename` is used to identify the module.
    pub fn compile(
        ctx: &mut ContextImpl,
        source: &[u8],
        filename: &str,
        strip: bool,
    ) -> Result<Self, i32> {
        if let Compiled::Module(md) = ctx.compile(
            source,
            filename,
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

    /// eval the module, includes:
    /// - create module function,
    /// - link,
    /// - and eval the function.
    pub fn eval(&mut self, ctx: &mut ContextImpl) -> Result<Promise, i32> {
        // make module function
        self.create_module_function(ctx)?;
        // resolving imports, exports
        self.link(ctx)?;
        // evaluate module function
        self.eval_func(ctx)
    }

    /// Run the <eval> function of the module and of all its dependent modules.
    pub(crate) fn eval_func(&mut self, ctx: &mut ContextImpl) -> Result<Promise, i32> {
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        let mut m = match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_LINKED => *self,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => self.cycle_root().unwrap(),
            s => {
                unreachable!("status={:?}", s);
            }
        };

        // a promise may be created only on the cycle_root of a cycle
        if let Some(p) = m.promise {
            return Ok(p);
        }

        let (promise, rr) = Promise::create(ctx)?;
        self.promise.replace(promise);
        self.resolving_funcs = rr;

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
                    let _ = m.resolving_funcs[0].call(
                        ctx,
                        JSValue::UNDEFINED,
                        std::slice::from_ref(&JSValue::UNDEFINED),
                    );
                }
                debug_assert!(stack_top.is_none());
            }
            Err(result) => {
                while let Some(mut m1) = stack_top.take() {
                    debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATING);

                    m1.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
                    m1.set_eval_exception(Some(result));
                    m1.set_cycle_root(m); // spec bug: should be present

                    stack_top = m1.stack_prev.take();
                }

                debug_assert_eq!(m.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

                let func_obj = m.resolving_funcs[1]; // reject func
                let exc = m.eval_exception().unwrap();
                let _ = func_obj.call(ctx, JSValue::UNDEFINED, std::slice::from_ref(&exc));
            }
        }

        Ok(promise)
    }

    /// InnerModuleEvaluation, returns depth index or Err value.
    /// `index`: depth index
    fn eval_internal(
        &mut self,
        ctx: &mut ContextImpl,
        index: i32,
        stack_top: &mut Option<ModuleDef>,
    ) -> Result<i32, JSValue> {
        if js_check_stack_overflow(ctx.rt_ref(), 0) {
            ctx.throw_stack_overflow();
            return Err(ctx.rt_ptr().take_exception());
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

        self.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATING;
        self.dfs_index = index;
        self.dfs_ancestor_index = index;
        self.pending_async_dependencies = 0;

        // push 'self' on stack
        self.stack_prev = stack_top.take();
        stack_top.replace(*self);

        let mut index = index + 1;

        for i in 0..self.dependencies.len() {
            let mut m1 = self.dependencies[i].module.unwrap();
            index = m1.eval_internal(ctx, index, stack_top)?;

            debug_assert!(matches!(
                m1.status(),
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if m1.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
                self.dfs_ancestor_index =
                    std::cmp::min(self.dfs_ancestor_index, m1.dfs_ancestor_index);
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
                self.pending_async_dependencies += 1;

                if m1.async_parent_modules.len() == m1.async_parent_modules.capacity()
                    && m1.async_parent_modules.try_reserve(1).is_err()
                {
                    ctx.throw_out_of_memory();
                    return Err(ctx.rt_ptr().take_exception());
                }

                m1.async_parent_modules.push(*self);
            }
        }

        // // old logic
        // for dep in self.dependencies.iter() {
        //     let mut m1 = dep.module.unwrap();

        //     index = m1.eval_internal(ctx, index, stack_top)?;
        //     debug_assert!(matches!(
        //         m1.status(),
        //         JSModuleStatus::JS_MODULE_STATUS_EVALUATING
        //             | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        //             | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        //     ));

        //     if m1.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
        //         self.dfs_ancestor_index =
        //             std::cmp::min(self.dfs_ancestor_index, m1.dfs_ancestor_index);
        //     } else {
        //         m1 = m1.cycle_root().unwrap();
        //         debug_assert!(matches!(
        //             m1.status(),
        //             JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        //                 | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        //         ));
        //         if let Some(exc) = m1.eval_exception() {
        //             return Err(exc);
        //         }
        //     }

        //     if m1.async_evaluation() {
        //         self.pending_async_dependencies += 1;

        //         if m1.async_parent_modules.len() == m1.async_parent_modules.capacity()
        //             && m1.async_parent_modules.try_reserve(1).is_err()
        //         {
        //             ctx.throw_out_of_memory();
        //             return Err(ctx.rt().take_exception());
        //         }

        //         m1.async_parent_modules.push(self.clone());
        //     }
        // }

        if self.pending_async_dependencies > 0 {
            debug_assert!(!self.async_evaluation());
            self.set_async_evaluation(true);
            self.async_evaluation_timestamp = ctx.rt_ptr().module_async_evaluation_next_timestamp;
            ctx.rt_ptr().module_async_evaluation_next_timestamp += 1;
        } else if self.has_tla() {
            debug_assert!(!self.async_evaluation());
            self.set_async_evaluation(true);
            self.async_evaluation_timestamp = ctx.rt_ptr().module_async_evaluation_next_timestamp;
            ctx.rt_ptr().module_async_evaluation_next_timestamp += 1;
            self.execute_async_module(ctx).map_err(JSValue::Int)?;
        } else {
            self.execute_sync_module(ctx)?;
        }

        debug_assert!(self.dfs_ancestor_index <= self.dfs_index);

        if self.dfs_index == self.dfs_ancestor_index {
            loop {
                // pop module from stack
                let mut md = stack_top.take().unwrap();
                *stack_top = md.stack_prev.take();

                md.status = if md.async_evaluation() {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                } else {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                };

                // spec bug: cycle_root must be assigned before the test
                md.set_cycle_root(*self);

                if md.eq(self) {
                    break;
                }
            }
        }

        Ok(index)
    }

    /// execute module function in async manner
    fn execute_async_module(&self, ctx: &mut ContextImpl) -> Result<(), i32> {
        // Hint: bytecode is now a Function object
        let md_func = self.bytecode;
        debug_assert_eq!(
            md_func.as_object().unwrap().class_id(),
            JS_CLASS_BYTECODE_FUNCTION
        );

        // execute bytecode function in async manner.
        let mut promise = md_func.call_async(ctx, JSValue::UNDEFINED, &[], 0)?;

        promise.then(
            ctx,
            NativeFunction::create(
                ctx,
                Callback::Func(ModuleDef::execute_async_fulfilled),
                "",
                0,
                0,
                None,
                Some([JSValue::Module(*self)].into()),
            )?
            .into(),
            NativeFunction::create(
                ctx,
                Callback::Func(ModuleDef::execute_async_rejected),
                "",
                0,
                0,
                None,
                Some([JSValue::Module(*self)].into()),
            )?
            .into(),
        )?;

        Ok(())
    }

    /// async module execution callback
    fn execute_async_fulfilled(mut cc: CallContext) -> JSResult {
        let JSValue::Module(mut md) = cc.data(0) else {
            unreachable!()
        };

        if md.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(md.eval_exception().is_some());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(md.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC);
        debug_assert!(md.eval_exception().is_none());
        debug_assert!(md.async_evaluation());

        md.set_async_evaluation(false);
        md.set_evaluated(cc.ctx());

        let mut exec_list = Vec::<ModuleDef>::with_capacity(8);
        gather_module_ancestors(cc.ctx(), md, &mut exec_list)?;

        // sort by async_evaluation timestamp
        exec_list.sort_by(|m1, m2| {
            let t1 = m1.async_evaluation_timestamp;
            let t2 = m2.async_evaluation_timestamp;
            t1.cmp(&t2)
        });

        for mut m in exec_list {
            if m.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!(m.eval_exception().is_some());
            } else if m.has_tla() {
                m.execute_async_module(cc.ctx_mut())?;
            } else {
                match m.execute_sync_module(cc.ctx_mut()) {
                    Ok(_) => {
                        m.set_evaluated(cc.ctx());
                    }
                    Err(e) => {
                        Self::execute_async_rejected(CallContext::new(
                            cc.ctx_ptr(),
                            JSValue::UNDEFINED,
                            std::slice::from_ref(&e),
                            0,
                            Some(Rc::new(RefCell::new([JSValue::Module(m)].into()))),
                        ))?;
                    }
                }
            }
        }

        Ok(JSValue::UNDEFINED)
    }

    /// async module execution reject callback
    fn execute_async_rejected(cc: CallContext) -> JSResult {
        if js_check_stack_overflow(cc.ctx().rt_ref(), 0) {
            cc.ctx().throw_stack_overflow();
            return Err(-1);
        }

        let mut md = if let JSValue::Module(m) = cc.data(0) {
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

        md.set_eval_exception(Some(*err_val));
        md.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

        for m in md.async_parent_modules.iter() {
            let cc = CallContext::new(
                cc.ctx_ptr(),
                JSValue::UNDEFINED,
                std::slice::from_ref(err_val),
                0,
                Some(Rc::new(RefCell::new([JSValue::Module(*m)].into()))),
            );
            Self::execute_async_rejected(cc).unwrap();
        }

        if md.promise.is_some() {
            debug_assert_eq!(md.cycle_root(), Some(md));
            let func = md.resolving_funcs[1];
            let _ = func.call(cc.ctx(), JSValue::UNDEFINED, std::slice::from_ref(err_val));
        }

        Ok(JSValue::UNDEFINED)
    }

    fn execute_sync_module(&self, ctx: &mut ContextImpl) -> Result<(), JSValue> {
        if let Some(f) = self.init_func.as_ref() {
            // is native module: no asynchronous execution
            if unlikely(f(ctx, self).is_err()) {
                Err(ctx.rt_ptr().take_exception())
            } else {
                Ok(())
            }
        } else {
            // is bytecode function
            let bc = self.bytecode;
            debug_assert!(bc.as_object_BCFunction().is_some());

            // eval bytecode function
            let promise = bc
                .call_async(ctx, JSValue::UNDEFINED, &[], 0)
                .map_err(|_| ctx.rt_ptr().take_exception())?;

            match promise.state() {
                PromiseState::Fulfilled => Ok(()),
                PromiseState::Rejected => Err(promise.result()),
                PromiseState::Pending => {
                    ctx.throw_type_error("promise is pending");
                    Err(ctx.rt_ptr().take_exception())
                }
            }
        }
    }
}

fn gather_module_ancestors(
    ctx: &ContextImpl,
    module: ModuleDef,
    exec_list: &mut Vec<ModuleDef>,
) -> Result<(), i32> {
    if js_check_stack_overflow(ctx.rt_ref(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    for mut m in module.async_parent_modules.iter().copied() {
        if !exec_list.contains(&m) && m.cycle_root().is_some_and(|x| x.eval_exception().is_none()) {
            debug_assert_eq!(
                m.status(),
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            );
            debug_assert!(m.async_evaluation());
            debug_assert!(m.eval_exception().is_none());
            debug_assert!(m.pending_async_dependencies > 0);

            let adep = m.pending_async_dependencies;
            m.pending_async_dependencies = adep - 1;
            if adep == 1 {
                // pending_async_dependencies == 0
                if exec_list.len() == exec_list.capacity() && exec_list.try_reserve(8).is_err() {
                    ctx.throw_out_of_memory();
                    return Err(-1);
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
