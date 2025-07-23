use super::ModuleDef;
use crate::eval::{Compiled, JS_EVAL_FLAG_STRIP, JS_EVAL_TYPE_MODULE};
use crate::promise::Promise;
use crate::{
    context::JSContext,
    error::JSResult,
    function::{AsyncFunction, CallContext, JS_Call},
    module::{JSModuleStatus, ModuleDefInner, js_create_module_function},
    promise::js_promise_then,
    runtime::js_check_stack_overflow,
    value::JSValue,
};

impl ModuleDefInner {
    /// set module status to `evaluated`
    pub fn set_evaluated(&self, ctx: &JSContext) {
        self.status.set(JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

        if unsafe { !(*self.promise.as_ptr()).is_undefined() } {
            debug_assert_eq!(self.cycle_root.borrow().as_ref().unwrap().id(), self.id());
            JS_Call(
                ctx,
                unsafe { &(*self.resolving_funcs.as_ptr())[0] },
                &JSValue::UNDEFINED,
                &[JSValue::UNDEFINED],
            );
        }
    }
}

impl ModuleDef {
    #[inline(always)]
    pub fn status(&self) -> JSModuleStatus {
        self.0.status.get()
    }

    /// compile source as module, don't run.
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

    /// Run the <eval> function of the module and of all its dependent modules.
    /// Returns Promise.
    pub fn eval(&self, ctx: &JSContext) -> Result<Promise, i32> {
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        if unsafe { js_create_module_function(ctx, self) } < 0 {
            return Err(-1);
        }

        self.link(ctx)?;

        let m = match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_LINKED => self.clone(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                self.0.cycle_root.borrow().as_ref().cloned().unwrap()
            }
            _ => {
                unreachable!()
            }
        };

        // a promise may be created only on the cycle_root of a cycle
        let p = unsafe { (*m.0.promise.as_ptr()).clone() };
        if !p.is_undefined() {
            return Promise::try_from(p);
        }

        let (promise, rr) = ctx.create_promise(&JSValue::UNDEFINED)?;
        self.0.promise.replace(promise.into());
        self.0.resolving_funcs.replace(rr);

        let mut stack_top: Option<ModuleDef> = None;

        match m.do_eval(ctx, 0, &mut stack_top) {
            Ok(_) => {
                debug_assert!(matches!(
                    m.status(),
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                debug_assert!(!m.0.eval_has_exception.get());

                if !m.0.async_evaluation.get() {
                    debug_assert_eq!(m.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    let func_obj = unsafe { (*m.0.resolving_funcs.as_ptr())[0].clone() }; // resolve func
                    JS_Call(ctx, &func_obj, &JSValue::UNDEFINED, &[JSValue::UNDEFINED]);
                }
                debug_assert!(stack_top.is_none());
            }
            Err(result) => {
                while let Some(m1) = stack_top.take() {
                    debug_assert_eq!(m1.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATING);
                    m1.0.status.set(JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    m1.0.eval_has_exception.set(true);
                    m1.0.eval_exception.replace(result.clone());
                    m1.0.cycle_root.replace(Some(m.clone())); // spec bug: should be present
                    stack_top = m1.0.stack_prev.take();
                }

                debug_assert_eq!(m.status(), JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                debug_assert!(m.0.eval_has_exception.get());

                let func_obj = unsafe { (*m.0.resolving_funcs.as_ptr())[1].clone() }; // reject func
                let exc = unsafe { (*m.0.eval_exception.as_ptr()).clone() };
                JS_Call(ctx, &func_obj, &JSValue::UNDEFINED, &[exc]);
            }
        }

        Promise::try_from(unsafe { (*m.0.promise.as_ptr()).clone() })
    }

    /// InnerModuleEvaluation, returns depth index or Err.
    /// `index`: depth index
    fn do_eval(
        &self,
        ctx: &JSContext,
        index: i32,
        pstack_top: &mut Option<ModuleDef>,
    ) -> Result<i32, JSValue> {
        if js_check_stack_overflow(&ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(ctx.take_exception());
        }

        #[cfg(DUMP_MODULE_RESOLVE)]
        {
            let mut buf1: [u8; ATOM_GET_STR_BUF_SIZE] = [0; ATOM_GET_STR_BUF_SIZE];
            println!(
                "js_inner_module_evaluation '{}':\n",
                JS_AtomGetStr(
                    ctx,
                    buf1.as_mut_ptr(),
                    mem::size_of_val(&buf1),
                    (*m).module_name
                )
            );
        }

        match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                return if self.0.eval_has_exception.get() {
                    unsafe {
                        let exc = (*self.0.eval_exception.as_ptr()).clone();
                        Err(exc)
                    }
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

        self.0
            .status
            .set(JSModuleStatus::JS_MODULE_STATUS_EVALUATING);
        self.0.dfs_index.set(index);
        self.0.dfs_ancestor_index.set(index);
        self.0.pending_async_dependencies.set(0);

        // push 'm' on stack
        self.0.stack_prev.replace(pstack_top.take());
        pstack_top.replace(self.clone());

        let mut index = index + 1;

        for dep in self.0.dependencies.borrow().iter() {
            let mut m1 = dep.module.clone().unwrap();
            index = m1.do_eval(ctx, index, pstack_top)?;

            let (
                mut m1_status,
                mut m1_dfs_ancestor_index,
                mut m1_cycle_root,
                mut m1_eval_has_exception,
                mut m1_eval_exception,
                mut m1_async_eval,
            ) = {
                (
                    m1.status(),
                    m1.0.dfs_ancestor_index.get(),
                    m1.0.cycle_root.borrow().as_ref().cloned().unwrap(),
                    m1.0.eval_has_exception.get(),
                    unsafe { (*m1.0.eval_exception.as_ptr()).clone() },
                    m1.0.async_evaluation.get(),
                )
            };

            debug_assert!(matches!(
                m1_status,
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if m1_status == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
                self.0.dfs_ancestor_index.set(std::cmp::min(
                    self.0.dfs_ancestor_index.get(),
                    m1_dfs_ancestor_index,
                ));
            } else {
                m1 = m1_cycle_root;

                // reload state vars
                (
                    m1_status,
                    m1_dfs_ancestor_index,
                    m1_cycle_root,
                    m1_eval_has_exception,
                    m1_eval_exception,
                    m1_async_eval,
                ) = {
                    (
                        m1.status(),
                        m1.0.dfs_ancestor_index.get(),
                        m1.0.cycle_root.borrow().as_ref().cloned().unwrap(),
                        m1.0.eval_has_exception.get(),
                        unsafe { (*m1.0.eval_exception.as_ptr()).clone() },
                        m1.0.async_evaluation.get(),
                    )
                };

                debug_assert!(matches!(
                    m1_status,
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));

                if m1_eval_has_exception {
                    return Err(m1_eval_exception);
                }
            }

            if m1_async_eval {
                self.0
                    .pending_async_dependencies
                    .set(self.0.pending_async_dependencies.get() + 1);

                let lst = &mut m1.0.async_parent_modules.borrow_mut();
                if lst.len() == lst.capacity() {
                    if lst.try_reserve(8).is_err() {
                        ctx.throw_out_of_memory();
                        return Err(ctx.take_exception());
                    }
                }
                lst.push(self.clone());
            }
        }

        let (pending_async_dependencies, has_tla, async_evaluation) = {
            (
                self.0.pending_async_dependencies.get(),
                self.0.has_tla.get(),
                self.0.async_evaluation.get(),
            )
        };
        if pending_async_dependencies > 0 {
            debug_assert!(!async_evaluation);
            self.0.async_evaluation.set(true);
            self.0
                .async_evaluation_timestamp
                .set(ctx.rt().module_async_evaluation_next_timestamp);
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
        } else if has_tla {
            debug_assert!(!async_evaluation);
            {
                self.0.async_evaluation.set(true);
                self.0
                    .async_evaluation_timestamp
                    .set(ctx.rt().module_async_evaluation_next_timestamp);
                ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
            }
            self.execute_async(ctx).unwrap();
        } else {
            self.execute_sync(ctx)?;
        }

        let (dfs_ancestor_index, dfs_index) =
            { (self.0.dfs_ancestor_index.get(), self.0.dfs_index.get()) };
        debug_assert!(dfs_ancestor_index <= dfs_index);
        if dfs_index == dfs_ancestor_index {
            loop {
                // pop m1 from stack
                let m1 = pstack_top.take().unwrap();
                *pstack_top = m1.0.stack_prev.take();

                m1.0.status.set(if !m1.0.async_evaluation.get() {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                } else {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                });

                // spec bug: cycle_root must be assigned before the test
                m1.0.cycle_root.replace(Some(self.clone()));
                if m1.id() == self.id() {
                    break;
                }
            }
        }

        Ok(index)
    }

    /// execute module async
    pub fn execute_async(&self, ctx: &JSContext) -> Result<(), i32> {
        let bc = unsafe { (*self.0.bytecode.as_ptr()).clone() };
        let afunc = AsyncFunction::try_from(bc).unwrap();
        let promise = afunc.call(ctx, JSValue::UNDEFINED, &[], 0)?;

        let data = [JSValue::Module(self.clone())];

        let resolve_funcs: [JSValue; 2] = [
            ctx.create_native_callable_object(
                ModuleDef::execute_async_resolve_callback,
                0,
                0,
                &data,
            )?,
            ctx.create_native_callable_object(
                ModuleDef::execute_async_reject_callback,
                0,
                0,
                &data,
            )?,
        ];

        let cc = CallContext::new(ctx, promise.into(), &resolve_funcs, 0, None);
        // TODO: use Promise::then()
        js_promise_then(ctx, cc)?;

        Ok(())
    }

    /// async module execution resolve callback
    pub fn execute_async_resolve_callback(ctx: &JSContext, args: CallContext) -> JSResult {
        let module = if let JSValue::Module(m) = args.data_raw(0) {
            m
        } else {
            unreachable!()
        };

        if module.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(module.0.eval_has_exception.get());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            module.status(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(!module.0.eval_has_exception.get());
        debug_assert!(module.0.async_evaluation.get());

        module.0.async_evaluation.set(false);
        module.0.set_evaluated(ctx);

        let mut exec_list = Vec::<ModuleDef>::with_capacity(8);
        unsafe {
            gather_module_ancestors(ctx, &module, &mut exec_list)?;
        }
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

        // sort by increasing async_evaluation timestamp
        exec_list.sort_by(|m1, m2| {
            let t1 = m1.0.async_evaluation_timestamp.get();
            let t2 = m2.0.async_evaluation_timestamp.get();
            t1.cmp(&t2)
        });

        for m in exec_list {
            if m.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!(m.0.eval_has_exception.get());
            } else if m.0.has_tla.get() {
                m.execute_async(ctx)?;
            } else {
                match m.execute_sync(ctx) {
                    Ok(_) => {
                        m.0.set_evaluated(ctx);
                    }
                    Err(e) => {
                        let arglist = [e];
                        let mut data = [JSValue::Module(m.clone())];
                        let cc =
                            CallContext::new(ctx, JSValue::UNDEFINED, &arglist, 0, Some(&mut data));
                        Self::execute_async_reject_callback(ctx, cc)?;
                    }
                }
            }
        }

        Ok(JSValue::UNDEFINED)
    }

    /// async module execution reject callback
    pub fn execute_async_reject_callback(ctx: &JSContext, args: CallContext) -> JSResult {
        if unsafe { js_check_stack_overflow(&ctx.rt(), 0) } {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        let module = if let JSValue::Module(m) = args.data_raw(0) {
            m
        } else {
            unreachable!()
        };

        let mut err_val = args.get_raw(0);

        if module.status() == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(module.0.eval_has_exception.get());
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            module.status(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(!module.0.eval_has_exception.get());
        debug_assert!(module.0.async_evaluation.get());

        module.0.eval_has_exception.set(true);
        module.0.eval_exception.set(err_val.clone());
        module
            .0
            .status
            .set(JSModuleStatus::JS_MODULE_STATUS_EVALUATED);

        for m in module.0.async_parent_modules.borrow().iter() {
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

        if unsafe { !(*module.0.promise.as_ptr()).is_undefined() } {
            debug_assert_eq!(
                module.0.cycle_root.borrow().as_ref().unwrap().id(),
                module.id()
            );

            JS_Call(
                ctx,
                unsafe { &(*module.0.resolving_funcs.as_ptr())[1] },
                &JSValue::UNDEFINED,
                std::slice::from_ref(&err_val),
            );
        }

        Ok(JSValue::UNDEFINED)
    }

    /// execute module in sync mode. returns error value on failure.
    pub fn execute_sync(&self, ctx: &JSContext) -> Result<(), JSValue> {
        if let Some(f) = self.0.init_func {
            // has native module init fn: no asynchronous execution
            if f(ctx, self) < 0 {
                return Err(ctx.take_exception());
            }
        } else {
            // is bytecode function
            let func = unsafe { (*self.0.bytecode.as_ptr()).clone() };
            let ret = JS_Call(ctx, &func, &JSValue::UNDEFINED, &[])
                .check_exception()
                .map_err(|_| ctx.take_exception())?;

            // let promise = {
            //     let bc = self.0.borrow().bytecode.clone();
            //     println!("[O.o] func:{}, class_id={}", bc.repr(), unsafe {
            //         bc.object_unchecked().class_id()
            //     });
            //
            //     let afunc = AsyncFunction::try_from(bc).unwrap();
            //     afunc
            //         .call(ctx, JSValue::UNDEFINED, &[], 0)
            //         .map_err(|_| ctx.take_exception())? // return Err with current exception
            // };
            //
            // match promise.state().unwrap() {
            //     JSPromiseStateEnum::JS_PROMISE_FULFILLED => {}
            //     JSPromiseStateEnum::JS_PROMISE_REJECTED => {
            //         let val = promise.result();
            //         return Err(val);
            //     }
            //     JSPromiseStateEnum::JS_PROMISE_PENDING => {
            //         ctx.JS_ThrowTypeError("promise is pending");
            //         return Err(ctx.take_exception());
            //     }
            // }
        }

        Ok(())
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
    /// eval `source` as module. `url` is used to identify the module.
    /// returns the new module def.
    pub fn eval_module_source(
        &self,
        source: &[u8],
        url: &str,
        strip: bool,
    ) -> Result<ModuleDef, i32> {
        // compile, then set import.meta, then run.
        let md = ModuleDef::compile(self, source, url, strip)?;
        md.0.set_import_meta(self, url, true)?;

        let _ = md.eval(self)?;
        Ok(md)
    }
}

// spec: InnerModuleEvaluation. Return (index, JSValue::UNDEFINED) or (-1, exception)
#[deprecated(note = "use ModuleDef::do_eval() instead")]
pub unsafe fn js_inner_module_evaluation(
    ctx: &JSContext,
    m: *mut ModuleDef,
    index: i32,
    pstack_top: &mut Option<ModuleDef>,
    pvalue: *mut JSValue,
) -> i32 {
    match (*m).do_eval(ctx, index, pstack_top) {
        Ok(index) => {
            *pvalue = JSValue::UNDEFINED;
            return index;
        }
        Err(err_val) => {
            *pvalue = err_val;
            return -1;
        }
    }
}

#[deprecated(note = "use ModuleDef::execute_async() instead")]
pub unsafe fn js_execute_async_module(ctx: &JSContext, m: *mut ModuleDef) -> i32 {
    (*m).execute_async(ctx).map_or(-1, |_| -1)
}

#[deprecated(note = "use ModuleDef::execute_sync() instead")]
pub unsafe fn js_execute_sync_module(
    ctx: &JSContext,
    m: *mut ModuleDef,
    pvalue: *mut JSValue,
) -> i32 {
    match (*m).execute_sync(ctx) {
        Ok(()) => {
            *pvalue = JSValue::UNDEFINED;
            0
        }
        Err(err_val) => {
            *pvalue = err_val;
            -1
        }
    }
}

#[deprecated(note = "use ModuleDef::execute_async_reject_callback() instead")]
pub fn js_async_module_execution_rejected(ctx: &JSContext, args: CallContext) -> JSResult {
    ModuleDef::execute_async_reject_callback(ctx, args)

    // let module = unsafe {
    //     JS_VALUE_GET_PTR(args.data_raw(0))
    //         .cast::<ModuleDef>()
    //         .as_mut()
    //         .unwrap()
    // };
    // let mut error = args.get_raw(0);

    // if unsafe { js_check_stack_overflow(ctx.rt(), 0) } {
    //     unsafe {
    //         JS_ThrowStackOverflow(ctx);
    //     }
    //     return Err(-1);
    // }

    // if module.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
    //     debug_assert!(module.eval_has_exception);
    //     return Ok(JSValue::UNDEFINED);
    // }

    // debug_assert_eq!(
    //     module.status,
    //     JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
    // );
    // debug_assert!(!module.eval_has_exception);
    // debug_assert!(module.async_evaluation);

    // module.eval_has_exception = true;
    // module.eval_exception = error.dup();
    // module.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

    // for i in 0..module.async_parent_modules_count {
    //     unsafe {
    //         let m = *module.async_parent_modules.offset(i as isize);
    //         let arglist = [error];
    //         let mut data = [JS_NewModuleValue(ctx, m)];
    //         let cc = CallContext::new(
    //             NonNull::new_unchecked(ctx),
    //             JSValue::UNDEFINED,
    //             &arglist,
    //             0,
    //             Some(&mut data),
    //         );
    //         js_async_module_execution_rejected(ctx, cc);
    //         JS_FreeValue(ctx, data[0]);
    //     }
    // }

    // if !module.promise.is_undefined() {
    //     debug_assert_eq!(
    //         module.cycle_root as *mut ModuleDef,
    //         module as *mut ModuleDef
    //     );

    //     unsafe {
    //         let ret_val = JS_Call(
    //             ctx,
    //             (*module).resolving_funcs[1],
    //             JSValue::UNDEFINED,
    //             1,
    //             &mut error,
    //         );
    //         JS_FreeValue(ctx, ret_val);
    //     }
    // }

    // Ok(JSValue::UNDEFINED)
}

pub struct ExecModuleList {
    pub tab: *mut *mut ModuleDef,
    pub count: i32,
    pub size: i32,
}

#[deprecated(note = "use ModuleDef::execute_async_resolve_callback() instead")]
pub fn js_async_module_execution_fulfilled(ctx: &JSContext, args: CallContext) -> JSResult {
    ModuleDef::execute_async_resolve_callback(ctx, args)

    // let module = unsafe {
    //     JS_VALUE_GET_PTR(args.data_raw(0))
    //         .cast::<ModuleDef>()
    //         .as_mut()
    //         .unwrap()
    // };

    // if module.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
    //     debug_assert!((*module).eval_has_exception);
    //     return Ok(JSValue::UNDEFINED);
    // }

    // debug_assert_eq!(
    //     (*module).status,
    //     JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
    // );
    // debug_assert!(!(*module).eval_has_exception);
    // debug_assert!((*module).async_evaluation);

    // let mut exec_list_s = ExecModuleList {
    //     tab: std::ptr::null_mut(),
    //     count: 0,
    //     size: 0,
    // };
    // let exec_list = &mut exec_list_s;

    // module.async_evaluation = false;
    // unsafe {
    //     js_set_module_evaluated(ctx, module);
    // }

    // if unsafe { gather_available_ancestors(ctx, module, exec_list) } < 0 {
    //     unsafe {
    //         js_free(ctx, (*exec_list).tab.cast());
    //     }
    //     return Err(-1);
    // }

    // unsafe {
    //     // sort by increasing async_evaluation timestamp
    //     let md_lst = std::slice::from_raw_parts_mut((*exec_list).tab, (*exec_list).size as usize);
    //     md_lst.sort_by(|m1, m2| {
    //         let t1 = (*(*m1)).async_evaluation_timestamp;
    //         let t2 = (*(*m2)).async_evaluation_timestamp;
    //         t1.cmp(&t2)
    //     });

    //     for i in 0..(*exec_list).count {
    //         let m = *(*exec_list).tab.offset(i as isize);
    //         if (*m).status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
    //             debug_assert!((*m).eval_has_exception);
    //         } else if (*m).has_tla {
    //             js_execute_async_module(ctx, m);
    //         } else {
    //             let mut error = JSValue::UNDEFINED;
    //             if js_execute_sync_module(ctx, m, &mut error) < 0 {
    //                 let arglist = [error];
    //                 let mut data = [JS_NewModuleValue(ctx, m)];
    //                 let cc = CallContext::new(
    //                     NonNull::new_unchecked(ctx),
    //                     JSValue::UNDEFINED,
    //                     &arglist,
    //                     0,
    //                     Some(&mut data),
    //                 );
    //                 js_async_module_execution_rejected(ctx, cc).unwrap();
    //                 JS_FreeValue(ctx, data[0]);
    //                 JS_FreeValue(ctx, error);
    //             } else {
    //                 js_set_module_evaluated(ctx, m);
    //             }
    //         }
    //     }

    //     js_free(ctx, (*exec_list).tab.cast());
    // }

    // Ok(JSValue::UNDEFINED)
}

#[deprecated(note = "use ModuleDef::set_evaluated() instead")]
pub unsafe fn js_set_module_evaluated(ctx: &JSContext, m: &ModuleDef) {
    m.0.set_evaluated(ctx);
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
            && m.0
                .cycle_root
                .borrow()
                .as_ref()
                .map_or(false, |r| r.0.eval_has_exception.get())
        {
            debug_assert_eq!(
                m.status(),
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            );
            debug_assert!(!m.0.eval_has_exception.get());
            debug_assert!(m.0.async_evaluation.get());
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

                if !m.0.has_tla.get() {
                    gather_module_ancestors(ctx, m, result)?;
                }
            }
        }
    }

    Ok(())
}

#[deprecated(note = "use gather_module_ancestors() instead")]
pub unsafe fn gather_available_ancestors(
    ctx: &JSContext,
    module: *mut ModuleDef,
    exec_list: &mut ExecModuleList,
) -> i32 {
    unreachable!("use gather_module_ancestors() instead");

    // if js_check_stack_overflow(ctx.rt(), 0) {
    //     JS_ThrowStackOverflow(ctx);
    //     return -1;
    // }

    // for i in 0..(*module).async_parent_modules_count {
    //     let m = *(*module).async_parent_modules.offset(i as isize);
    //     if unsafe { !(*m).in_execution_list(exec_list) } && !(*(*m).cycle_root).eval_has_exception {
    //         debug_assert_eq!(
    //             (*m).status,
    //             JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
    //         );
    //         debug_assert!(!(*m).eval_has_exception);
    //         debug_assert!((*m).async_evaluation);
    //         debug_assert!((*m).pending_async_dependencies > 0);

    //         (*m).pending_async_dependencies -= 1;
    //         if (*m).pending_async_dependencies == 0 {
    //             if js_resize_array(
    //                 ctx,
    //                 &mut exec_list.tab as *mut _ as *mut *mut _,
    //                 std::mem::size_of::<*mut ModuleDef>() as _,
    //                 &mut exec_list.size,
    //                 exec_list.count + 1,
    //             ) != 0
    //             {
    //                 return -1;
    //             }
    //             *exec_list.tab.offset((*exec_list).count as isize) = m;
    //             exec_list.count += 1;

    //             if !(*m).has_tla {
    //                 if gather_available_ancestors(ctx, m, exec_list) < 0 {
    //                     return -1;
    //                 }
    //             }
    //         }
    //     }
    // }

    // 0
}

#[deprecated(note = "use ModuleDef::in_execution_list() instead")]
pub unsafe fn find_in_exec_module_list(exec_list: &mut ExecModuleList, m: *mut ModuleDef) -> bool {
    (*m).in_execution_list(exec_list)

    // for i in 0..(*exec_list).count {
    //     if *(*exec_list).tab.offset(i as isize) == m {
    //         return true;
    //     }
    // }
    // false
}
