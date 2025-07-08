use crate::{
    context::JSContext,
    error::{JS_ThrowStackOverflow, JSResult},
    function::{AsyncFunction, CallContext, JS_Call},
    module::{JSModuleDefInner, JSModuleStatus, js_create_module_function, js_link_module},
    promise::{JSPromiseStateEnum, js_promise_then},
    runtime::js_check_stack_overflow,
    value::JSValue,
};

use super::JSModuleDef;

impl JSModuleDefInner {
    /// set module status to `evaluated`
    pub fn set_evaluated(&mut self, ctx: &JSContext) {
        self.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
        if !self.promise.is_undefined() {
            debug_assert_eq!(self.cycle_root.as_ref().unwrap().id(), self.id());

            JS_Call(
                ctx,
                &self.resolving_funcs[0],
                &JSValue::UNDEFINED,
                &[JSValue::UNDEFINED],
            );
        }
    }
}

impl JSModuleDef {
    #[inline(always)]
    pub fn status(&self) -> JSModuleStatus {
        self.0.borrow().status
    }

    /// Run the <eval> function of the module and of all its requested modules.
    /// Return a promise or an exception.
    pub fn eval(&self, ctx: &JSContext) -> JSValue {
        debug_assert!(matches!(
            self.status(),
            JSModuleStatus::JS_MODULE_STATUS_UNLINKED
                | JSModuleStatus::JS_MODULE_STATUS_LINKED
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));

        if unsafe { js_create_module_function(ctx, self) } < 0 {
            return JSValue::EXCEPTION;
        }

        if js_link_module(ctx, self) < 0 {
            return JSValue::EXCEPTION;
        }

        let m = match self.status() {
            JSModuleStatus::JS_MODULE_STATUS_LINKED => self.clone(),
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                self.0.borrow().cycle_root.clone().unwrap()
            }
            _ => {
                unreachable!()
            }
        };

        // a promise may be created only on the cycle_root of a cycle
        if !m.0.borrow().promise.is_undefined() {
            return m.0.borrow().promise.clone();
        }

        if let Ok((promise, rr)) = ctx.create_promise(&JSValue::UNDEFINED) {
            m.0.borrow_mut().resolving_funcs = rr;
            m.0.borrow_mut().promise = promise.into();
        } else {
            return JSValue::EXCEPTION;
        }

        let mut stack_top: Option<JSModuleDef> = None;

        match m.do_eval(ctx, 0, &mut stack_top) {
            Ok(_) => {
                debug_assert!(matches!(
                    m.0.borrow().status,
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                debug_assert!(!m.0.borrow().eval_has_exception);

                if !m.0.borrow().async_evaluation {
                    debug_assert_eq!(
                        m.0.borrow().status,
                        JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                    );

                    JS_Call(
                        ctx,
                        &m.0.borrow().resolving_funcs[0], // resolve func
                        &JSValue::UNDEFINED,
                        &[JSValue::UNDEFINED],
                    );
                }
                debug_assert!(stack_top.is_none());
            }
            Err(result) => {
                while let Some(m1) = stack_top.take() {
                    let mut m1 = m1.0.borrow_mut();

                    debug_assert_eq!(m1.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING);

                    m1.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
                    m1.eval_has_exception = true;
                    m1.eval_exception = result.clone();
                    m1.cycle_root = Some(m.clone()); // spec bug: should be present

                    stack_top = m1.stack_prev.take();
                }

                debug_assert_eq!(
                    m.0.borrow().status,
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                );
                debug_assert!(m.0.borrow().eval_has_exception);

                JS_Call(
                    ctx,
                    &m.0.borrow().resolving_funcs[1], // reject
                    &JSValue::UNDEFINED,
                    &[m.0.borrow().eval_exception.clone()],
                );
            }
        }

        m.0.borrow().promise.clone()
    }

    /// InnerModuleEvaluation, returns depth index or Err.
    /// `index`: depth index
    fn do_eval(
        &self,
        ctx: &JSContext,
        index: i32,
        pstack_top: &mut Option<JSModuleDef>,
    ) -> Result<i32, JSValue> {
        if js_check_stack_overflow(&ctx.rt(), 0) {
            JS_ThrowStackOverflow(ctx);
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

        match self.0.borrow().status {
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                return if self.0.borrow().eval_has_exception {
                    Err(self.0.borrow().eval_exception.clone())
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

        {
            let mut m = self.0.borrow_mut();
            m.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATING;
            m.dfs_index = index;
            m.dfs_ancestor_index = index;
            m.pending_async_dependencies = 0;

            // push 'm' on stack
            m.stack_prev = pstack_top.take();
            pstack_top.replace(self.clone());
        }

        let mut index = index + 1;

        for i in 0..self.0.borrow().req_module_entries_count {
            let rme = unsafe { self.0.borrow().req_module_entries.offset(i as isize) };
            let mut m1 = unsafe { (*rme).module.clone().unwrap() };

            index = m1.do_eval(ctx, index, pstack_top)?;

            debug_assert!(matches!(
                m1.0.borrow().status,
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if m1.0.borrow_mut().status == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
                self.0.borrow_mut().dfs_ancestor_index = std::cmp::min(
                    self.0.borrow().dfs_ancestor_index,
                    m1.0.borrow().dfs_ancestor_index,
                );
            } else {
                let root = m1.0.borrow().cycle_root.clone().unwrap();
                m1 = root;

                debug_assert!(matches!(
                    m1.0.borrow().status,
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                if m1.0.borrow().eval_has_exception {
                    return Err(m1.0.borrow().eval_exception.clone());
                }
            }

            if m1.0.borrow().async_evaluation {
                self.0.borrow_mut().pending_async_dependencies += 1;

                let lst = &mut m1.0.borrow_mut().async_parent_modules;
                if lst.len() == lst.capacity() {
                    if lst.try_reserve(8).is_err() {
                        ctx.JS_ThrowOutOfMemory();
                        return Err(ctx.take_exception());
                    }
                }
                lst.push(self.clone());
            }
        }

        if self.0.borrow().pending_async_dependencies > 0 {
            debug_assert!(!self.0.borrow().async_evaluation);
            self.0.borrow_mut().async_evaluation = true;
            self.0.borrow_mut().async_evaluation_timestamp =
                ctx.rt().module_async_evaluation_next_timestamp;
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
        } else if self.0.borrow().has_tla {
            debug_assert!(!self.0.borrow().async_evaluation);
            self.0.borrow_mut().async_evaluation = true;
            self.0.borrow_mut().async_evaluation_timestamp =
                ctx.rt().module_async_evaluation_next_timestamp;
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
            self.execute_async(ctx).unwrap();
        } else {
            self.execute_sync(ctx)?;
        }

        debug_assert!(self.0.borrow().dfs_ancestor_index <= self.0.borrow().dfs_index);

        if self.0.borrow().dfs_index == self.0.borrow().dfs_ancestor_index {
            loop {
                // pop m1 from stack
                let m1 = pstack_top.take().unwrap();
                *pstack_top = m1.0.borrow_mut().stack_prev.take();

                m1.0.borrow_mut().status = if !m1.0.borrow().async_evaluation {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                } else {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                };

                // spec bug: cycle_root must be assigned before the test
                m1.0.borrow_mut().cycle_root.replace(self.clone());
                if m1.id() == self.id() {
                    break;
                }
            }
        }

        Ok(index)
    }

    /// execute module async
    pub fn execute_async(&self, ctx: &JSContext) -> Result<(), i32> {
        let afunc_obj: AsyncFunction = self.0.borrow().bytecode.clone().try_into().unwrap();
        let promise = afunc_obj.call(ctx, JSValue::UNDEFINED, &[], 0)?;

        let data = [JSValue::Module(self.clone())];

        let resolve_funcs: [JSValue; 2] = [
            ctx.create_native_callable_object(
                JSModuleDef::execute_async_resolve_callback,
                0,
                0,
                &data,
            )?,
            ctx.create_native_callable_object(
                JSModuleDef::execute_async_reject_callback,
                0,
                0,
                &data,
            )?,
        ];

        let cc = CallContext::new(ctx, promise.into(), &resolve_funcs, 0, None);
        // TODO: use PromiseValue::then()
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

        let mut exec_list = match module.with_mut(|md| -> Result<(), JSValue> {
            if md.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!(md.eval_has_exception);
                return Err(JSValue::UNDEFINED);
            }

            debug_assert_eq!(md.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC);
            debug_assert!(!md.eval_has_exception);
            debug_assert!(md.async_evaluation);

            md.async_evaluation = false;
            md.set_evaluated(ctx);
            Ok(())
        }) {
            Ok(_) => {
                let mut lst = Vec::<JSModuleDef>::with_capacity(8);
                unsafe {
                    gather_module_ancestors(ctx, &module, &mut lst)?;
                }
                lst
            }
            Err(v) => {
                return Ok(v);
            }
        };

        // sort by increasing async_evaluation timestamp
        exec_list.sort_by(|m1, m2| {
            let t1 = m1.0.borrow().async_evaluation_timestamp;
            let t2 = m2.0.borrow().async_evaluation_timestamp;
            t1.cmp(&t2)
        });

        for m in exec_list {
            if m.0.borrow().status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!(m.0.borrow().eval_has_exception);
            } else if m.0.borrow().has_tla {
                m.execute_async(ctx)?;
            } else {
                match unsafe { m.execute_sync(ctx) } {
                    Ok(_) => {
                        m.0.borrow_mut().set_evaluated(ctx);
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
            JS_ThrowStackOverflow(ctx);
            return Err(-1);
        }

        let module = if let JSValue::Module(m) = args.data_raw(0) {
            m
        } else {
            unreachable!()
        };

        let mut err_val = args.get_raw(0);

        if module.0.borrow().status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(module.0.borrow().eval_has_exception);
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            module.0.borrow().status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(!module.0.borrow().eval_has_exception);
        debug_assert!(module.0.borrow().async_evaluation);

        module.0.borrow_mut().eval_has_exception = true;
        module.0.borrow_mut().eval_exception = err_val.clone();
        module.0.borrow_mut().status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

        for m in module.0.borrow().async_parent_modules.iter() {
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

        if !module.0.borrow().promise.is_undefined() {
            debug_assert_eq!(
                module.0.borrow().cycle_root.as_ref().unwrap().id(),
                module.id()
            );

            JS_Call(
                ctx,
                &module.0.borrow().resolving_funcs[1],
                &JSValue::UNDEFINED,
                std::slice::from_ref(&err_val),
            );
        }

        Ok(JSValue::UNDEFINED)
    }

    /// execute module in sync mode. returns error value on failure.
    pub fn execute_sync(&self, ctx: &JSContext) -> Result<(), JSValue> {
        if let Some(f) = self.0.borrow().init_func {
            // has native module init fn: no asynchronous execution
            if f(ctx, self) < 0 {
                return Err(ctx.take_exception());
            }
        } else {
            let promise = {
                let afunc: AsyncFunction = self.0.borrow().bytecode.clone().try_into().unwrap();
                afunc
                    .call(ctx, JSValue::UNDEFINED, &[], 0)
                    .map_err(|_| ctx.take_exception())? // return Err with current exception
            };

            match promise.state().unwrap() {
                JSPromiseStateEnum::JS_PROMISE_FULFILLED => {}
                JSPromiseStateEnum::JS_PROMISE_REJECTED => {
                    let val = promise.result();
                    return Err(val);
                }
                JSPromiseStateEnum::JS_PROMISE_PENDING => {
                    ctx.JS_ThrowTypeError("promise is pending");
                    return Err(ctx.take_exception());
                }
            }
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

// spec: InnerModuleEvaluation. Return (index, JSValue::UNDEFINED) or (-1, exception)
#[deprecated(note = "use JSModuleDef::do_eval() instead")]
pub unsafe fn js_inner_module_evaluation(
    ctx: &JSContext,
    m: *mut JSModuleDef,
    index: i32,
    pstack_top: &mut Option<JSModuleDef>,
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

#[deprecated(note = "use JSModuleDef::execute_async() instead")]
pub unsafe fn js_execute_async_module(ctx: &JSContext, m: *mut JSModuleDef) -> i32 {
    (*m).execute_async(ctx).map_or(-1, |_| -1)
}

#[deprecated(note = "use JSModuleDef::execute_sync() instead")]
pub unsafe fn js_execute_sync_module(
    ctx: &JSContext,
    m: *mut JSModuleDef,
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

#[deprecated(note = "use JSModuleDef::execute_async_reject_callback() instead")]
pub fn js_async_module_execution_rejected(ctx: &JSContext, args: CallContext) -> JSResult {
    JSModuleDef::execute_async_reject_callback(ctx, args)

    // let module = unsafe {
    //     JS_VALUE_GET_PTR(args.data_raw(0))
    //         .cast::<JSModuleDef>()
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
    //         module.cycle_root as *mut JSModuleDef,
    //         module as *mut JSModuleDef
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
    pub tab: *mut *mut JSModuleDef,
    pub count: i32,
    pub size: i32,
}

#[deprecated(note = "use JSModuleDef::execute_async_resolve_callback() instead")]
pub fn js_async_module_execution_fulfilled(ctx: &JSContext, args: CallContext) -> JSResult {
    JSModuleDef::execute_async_resolve_callback(ctx, args)

    // let module = unsafe {
    //     JS_VALUE_GET_PTR(args.data_raw(0))
    //         .cast::<JSModuleDef>()
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

#[deprecated(note = "use JSModuleDef::set_evaluated() instead")]
pub unsafe fn js_set_module_evaluated(ctx: &JSContext, m: &JSModuleDef) {
    m.0.borrow_mut().set_evaluated(ctx);
}

fn gather_module_ancestors(
    ctx: &JSContext,
    module: &JSModuleDef,
    result: &mut Vec<JSModuleDef>,
) -> Result<(), i32> {
    if js_check_stack_overflow(&ctx.rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        return Err(-1);
    }

    for m in module.0.borrow().async_parent_modules.iter() {
        if !result.iter().any(|x| x.id() == m.id())
            && m.0
                .borrow()
                .cycle_root
                .as_ref()
                .map_or(false, |r| r.0.borrow().eval_has_exception)
        {
            debug_assert_eq!(
                m.0.borrow().status,
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            );
            debug_assert!(!m.0.borrow().eval_has_exception);
            debug_assert!(m.0.borrow().async_evaluation);
            debug_assert!(m.0.borrow().pending_async_dependencies > 0);

            m.0.borrow_mut().pending_async_dependencies -= 1;

            if m.0.borrow().pending_async_dependencies == 0 {
                if result.len() == result.capacity() {
                    if result.try_reserve(8).is_err() {
                        ctx.JS_ThrowOutOfMemory();
                        return Err(-1);
                    }
                }

                result.push(m.clone());

                if !m.0.borrow().has_tla {
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
    module: *mut JSModuleDef,
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
    //                 std::mem::size_of::<*mut JSModuleDef>() as _,
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

#[deprecated(note = "use JSModuleDef::in_execution_list() instead")]
pub unsafe fn find_in_exec_module_list(
    exec_list: &mut ExecModuleList,
    m: *mut JSModuleDef,
) -> bool {
    (*m).in_execution_list(exec_list)

    // for i in 0..(*exec_list).count {
    //     if *(*exec_list).tab.offset(i as isize) == m {
    //         return true;
    //     }
    // }
    // false
}
