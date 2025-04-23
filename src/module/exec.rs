use std::ptr::NonNull;

use crate::{
    context::JSContext,
    error::{JSResult, JS_ThrowStackOverflow},
    function::{js_async_function_call, CallArgs, JS_Call},
    mem::{js_free, js_resize_array},
    module::JSModuleStatus,
    promise::{self, js_promise_then, JSPromiseStateEnum},
    runtime::js_check_stack_overflow,
    value::{JSValue, JS_FreeValue, JS_VALUE_GET_PTR},
};

use super::{JSModuleDef, JS_NewModuleValue};

impl JSModuleDef {
    /// Run the <eval> function of the module and of all its requested modules.
    /// Return a promise or an exception.
    pub fn eval(&mut self, ctx: &mut JSContext) -> JSValue {
        let m = match self.status {
            JSModuleStatus::JS_MODULE_STATUS_LINKED => self,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => unsafe {
                self.cycle_root.as_mut().unwrap()
            },
            _ => {
                unreachable!()
            }
        };

        // a promise may be created only on the cycle_root of a cycle
        if !m.promise.is_undefined() {
            return m.promise.dup();
        }

        if let Ok((promise, rr)) = ctx.create_promise(JSValue::UNDEFINED) {
            m.resolving_funcs = rr;
            m.promise = promise.into();
        } else {
            return JSValue::EXCEPTION;
        }

        let mut stack_top = std::ptr::null_mut::<JSModuleDef>();

        match unsafe { m.do_eval(ctx, 0, &mut stack_top) } {
            Ok(_) => {
                debug_assert!(matches!(
                    m.status,
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                debug_assert!(!m.eval_has_exception);

                if !m.async_evaluation {
                    debug_assert_eq!(m.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    let mut value = JSValue::UNDEFINED;
                    let ret_val = unsafe {
                        JS_Call(
                            ctx,
                            m.resolving_funcs[0], // resolve func
                            JSValue::UNDEFINED,
                            1,
                            &mut value,
                        )
                    };

                    unsafe {
                        JS_FreeValue(ctx, ret_val);
                    }
                }
                debug_assert!(stack_top.is_null());
            }
            Err(result) => {
                while !stack_top.is_null() {
                    let m1 = stack_top;

                    unsafe {
                        debug_assert_eq!((*m1).status, JSModuleStatus::JS_MODULE_STATUS_EVALUATING);

                        (*m1).status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
                        (*m1).eval_has_exception = true;
                        (*m1).eval_exception = result.dup();
                        (*m1).cycle_root = m; // spec bug: should be present
                        stack_top = (*m1).stack_prev;
                    }
                }

                unsafe {
                    JS_FreeValue(ctx, result);

                    debug_assert_eq!(m.status, JSModuleStatus::JS_MODULE_STATUS_EVALUATED);
                    debug_assert!(m.eval_has_exception);

                    let ret_val = JS_Call(
                        ctx,
                        m.resolving_funcs[1], // reject
                        JSValue::UNDEFINED,
                        1,
                        &mut m.eval_exception,
                    );
                    JS_FreeValue(ctx, ret_val);
                }
            }
        }

        m.promise.dup()
    }

    /// InnerModuleEvaluation, returns depth index or Err.
    /// `index`: depth index
    unsafe fn do_eval(
        &mut self,
        ctx: &mut JSContext,
        index: i32,
        pstack_top: *mut *mut JSModuleDef,
    ) -> Result<i32, JSValue> {
        if js_check_stack_overflow(ctx.rt(), 0) {
            JS_ThrowStackOverflow(ctx);
            return Err(ctx.get_exception());
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

        match self.status {
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED => {
                return if self.eval_has_exception {
                    Err(self.eval_exception.dup())
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

        let mut index = index + 1;

        // push 'm' on stack
        self.stack_prev = *pstack_top;
        *pstack_top = self;

        let req_module_entries_count = self.req_module_entries_count;
        for i in 0..req_module_entries_count {
            let rme = self.req_module_entries.offset(i as isize);
            let mut m1 = (*rme).module;
            index = (*m1).do_eval(ctx, index, pstack_top)?;

            debug_assert!(matches!(
                (*m1).status,
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));

            if (*m1).status == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
                self.dfs_ancestor_index =
                    std::cmp::min(self.dfs_ancestor_index, (*m1).dfs_ancestor_index);
            } else {
                m1 = (*m1).cycle_root;
                debug_assert!(matches!(
                    (*m1).status,
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                        | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                ));
                if (*m1).eval_has_exception {
                    return Err((*m1).eval_exception.dup());
                }
            }

            if (*m1).async_evaluation {
                self.pending_async_dependencies += 1;
                if js_resize_array(
                    ctx,
                    &mut (*m1).async_parent_modules as *mut _ as *mut *mut _,
                    std::mem::size_of::<*mut JSModuleDef>() as _,
                    &mut (*m1).async_parent_modules_size,
                    (*m1).async_parent_modules_count + 1,
                ) != 0
                {
                    return Err((*ctx).get_exception());
                }

                *(*m1)
                    .async_parent_modules
                    .offset((*m1).async_parent_modules_count as isize) = self;
                (*m1).async_parent_modules_count += 1;
            }
        }

        if self.pending_async_dependencies > 0 {
            debug_assert!(!self.async_evaluation);
            self.async_evaluation = true;
            self.async_evaluation_timestamp = ctx.rt().module_async_evaluation_next_timestamp;
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
        } else if self.has_tla {
            debug_assert!(!self.async_evaluation);
            self.async_evaluation = true;
            self.async_evaluation_timestamp = ctx.rt().module_async_evaluation_next_timestamp;
            ctx.rt_mut().module_async_evaluation_next_timestamp += 1;
            self.execute_async(ctx).unwrap();
        } else {
            self.execute_sync(ctx)?;
        }

        debug_assert!(self.dfs_ancestor_index <= self.dfs_index);

        if self.dfs_index == self.dfs_ancestor_index {
            loop {
                // pop m1 from stack
                let m1 = *pstack_top;
                *pstack_top = (*m1).stack_prev;

                (*m1).status = if !(*m1).async_evaluation {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATED
                } else {
                    JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                };

                // spec bug: cycle_root must be assigned before the test
                (*m1).cycle_root = self;
                if m1 == self {
                    break;
                }
            }
        }

        Ok(index)
    }

    /// execute module async
    pub unsafe fn execute_async(&mut self, ctx: &mut JSContext) -> Result<(), i32> {
        let promise = js_async_function_call(
            ctx,
            self.func_obj,
            JSValue::UNDEFINED,
            0,
            std::ptr::null_mut(),
            0,
        )
        .check_exception()?;

        let data = [JS_NewModuleValue(ctx, self)];
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
        JS_FreeValue(ctx, data[0]);

        let cc = CallArgs::new(
            NonNull::new_unchecked(ctx),
            promise,
            &resolve_funcs,
            0,
            None,
        );
        // TODO: use PromiseValue::then()
        let ret = js_promise_then(ctx, cc);

        JS_FreeValue(ctx, resolve_funcs[0]);
        JS_FreeValue(ctx, resolve_funcs[1]);
        JS_FreeValue(ctx, promise);

        if let Ok(ret_val) = ret {
            JS_FreeValue(ctx, ret_val);
            Ok(())
        } else {
            Err(-1)
        }
    }

    /// async module execution resolve callback
    pub fn execute_async_resolve_callback(ctx: &mut JSContext, args: CallArgs) -> JSResult {
        let module = unsafe {
            JS_VALUE_GET_PTR(args.data_raw(0))
                .cast::<JSModuleDef>()
                .as_mut()
                .unwrap()
        };

        if module.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(module.eval_has_exception);
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            module.status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(!module.eval_has_exception);
        debug_assert!(module.async_evaluation);

        let mut exec_list_s = ExecModuleList {
            tab: std::ptr::null_mut(),
            count: 0,
            size: 0,
        };
        let exec_list = &mut exec_list_s;

        module.async_evaluation = false;
        module.set_evaluated(ctx);

        if unsafe { gather_available_ancestors(ctx, module, exec_list) } < 0 {
            unsafe {
                js_free(ctx, exec_list.tab.cast());
            }
            return Err(-1);
        }

        unsafe {
            // sort by increasing async_evaluation timestamp
            let md_lst = std::slice::from_raw_parts_mut(exec_list.tab, exec_list.size as usize);
            md_lst.sort_by(|m1, m2| {
                let t1 = (*(*m1)).async_evaluation_timestamp;
                let t2 = (*(*m2)).async_evaluation_timestamp;
                t1.cmp(&t2)
            });

            for i in 0..exec_list.count {
                let m = *exec_list.tab.offset(i as isize);
                if (*m).status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                    debug_assert!((*m).eval_has_exception);
                } else if (*m).has_tla {
                    (*m).execute_async(ctx).unwrap();
                } else {
                    match (*m).execute_sync(ctx) {
                        Ok(_) => {
                            (*m).set_evaluated(ctx);
                        }
                        Err(error) => {
                            let arglist = [error];
                            let mut data = [JS_NewModuleValue(ctx, m)];
                            let cc = CallArgs::new(
                                NonNull::new_unchecked(ctx),
                                JSValue::UNDEFINED,
                                &arglist,
                                0,
                                Some(&mut data),
                            );
                            Self::execute_async_reject_callback(ctx, cc).unwrap();
                            JS_FreeValue(ctx, data[0]);
                            JS_FreeValue(ctx, error);
                        }
                    }
                }
            }

            js_free(ctx, exec_list.tab.cast());
        }

        Ok(JSValue::UNDEFINED)
    }

    /// async module execution reject callback
    pub fn execute_async_reject_callback(ctx: &mut JSContext, args: CallArgs) -> JSResult {
        let module = unsafe {
            JS_VALUE_GET_PTR(args.data_raw(0))
                .cast::<JSModuleDef>()
                .as_mut()
                .unwrap()
        };
        let mut err_val = args.get_raw(0);

        if unsafe { js_check_stack_overflow(ctx.rt(), 0) } {
            unsafe {
                JS_ThrowStackOverflow(ctx);
            }
            return Err(-1);
        }

        if module.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
            debug_assert!(module.eval_has_exception);
            return Ok(JSValue::UNDEFINED);
        }

        debug_assert_eq!(
            module.status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
        );
        debug_assert!(!module.eval_has_exception);
        debug_assert!(module.async_evaluation);

        module.eval_has_exception = true;
        module.eval_exception = err_val.dup();
        module.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

        let arglist = [err_val];

        for i in 0..module.async_parent_modules_count {
            unsafe {
                let m = *module.async_parent_modules.offset(i as isize);
                let mut data = [JS_NewModuleValue(ctx, m)];
                let cc = CallArgs::new(
                    NonNull::new_unchecked(ctx),
                    JSValue::UNDEFINED,
                    &arglist,
                    0,
                    Some(&mut data),
                );
                Self::execute_async_reject_callback(ctx, cc).unwrap();
                JS_FreeValue(ctx, data[0]);
            }
        }

        if !module.promise.is_undefined() {
            debug_assert_eq!(
                module.cycle_root as *mut JSModuleDef,
                module as *mut JSModuleDef
            );

            unsafe {
                let ret_val = JS_Call(
                    ctx,
                    module.resolving_funcs[1],
                    JSValue::UNDEFINED,
                    1,
                    &mut err_val,
                );
                JS_FreeValue(ctx, ret_val);
            }
        }

        Ok(JSValue::UNDEFINED)
    }

    /// execute module sync. returns error value on failure.
    pub unsafe fn execute_sync(&mut self, ctx: &mut JSContext) -> Result<(), JSValue> {
        if let Some(cb) = self.init_func {
            // Native module init : no asynchronous execution
            if cb(ctx, self) < 0 {
                return Err(ctx.get_exception());
            }
        } else {
            let ret_val = js_async_function_call(
                ctx,
                self.func_obj,
                JSValue::UNDEFINED,
                0,
                std::ptr::null_mut(),
                0,
            );
            if ret_val.is_exception() {
                return Err(ctx.get_exception());
            }

            let promise = ret_val.as_promise_unchecked(); // must be a Promise

            match promise.state().unwrap() {
                JSPromiseStateEnum::JS_PROMISE_FULFILLED => {
                    JS_FreeValue(ctx, promise.into());
                }
                JSPromiseStateEnum::JS_PROMISE_REJECTED => {
                    let val = promise.result();
                    JS_FreeValue(ctx, promise.into());
                    return Err(val);
                }
                JSPromiseStateEnum::JS_PROMISE_PENDING => {
                    JS_FreeValue(ctx, promise.into());
                    ctx.JS_ThrowTypeError(c"promise is pending");
                    return Err(ctx.get_exception());
                }
            }
        }

        Ok(())
    }

    /// set module status to `evaluated`
    pub fn set_evaluated(&mut self, ctx: &mut JSContext) {
        self.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

        if !self.promise.is_undefined() {
            debug_assert_eq!(
                self.cycle_root as *const JSModuleDef,
                self as *const JSModuleDef
            );

            let mut value = JSValue::UNDEFINED;
            let ret_val = unsafe {
                JS_Call(
                    ctx,
                    self.resolving_funcs[0],
                    JSValue::UNDEFINED,
                    1,
                    &mut value,
                )
            };

            unsafe {
                JS_FreeValue(ctx, ret_val);
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

// spec: InnerModuleEvaluation. Return (index, JSValue::UNDEFINED) or (-1, exception)
#[deprecated(note = "use JSModuleDef::eval_inner() instead")]
pub unsafe fn js_inner_module_evaluation(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    index: i32,
    pstack_top: *mut *mut JSModuleDef,
    pvalue: *mut JSValue,
) -> i32 {
    match (*m).do_eval(ctx.as_mut().unwrap(), index, pstack_top) {
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
pub unsafe fn js_execute_async_module(ctx: *mut JSContext, m: *mut JSModuleDef) -> i32 {
    (*m).execute_async(ctx.as_mut().unwrap()).map_or(-1, |_| -1)
}

#[deprecated(note = "use JSModuleDef::execute_sync() instead")]
pub unsafe fn js_execute_sync_module(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    pvalue: *mut JSValue,
) -> i32 {
    match (*m).execute_sync(ctx.as_mut().unwrap()) {
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
pub fn js_async_module_execution_rejected(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    JSModuleDef::execute_async_reject_callback(ctx, args)

    // let module = unsafe {
    //     JS_VALUE_GET_PTR(args.data_raw(0))
    //         .cast::<JSModuleDef>()
    //         .as_mut()
    //         .unwrap()
    // };
    // let mut error = args.get_raw(0);

    // if unsafe { js_check_stack_overflow((*ctx).rt(), 0) } {
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
    //         let cc = CallArgs::new(
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
pub fn js_async_module_execution_fulfilled(ctx: &mut JSContext, args: CallArgs) -> JSResult {
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
    //                 let cc = CallArgs::new(
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
pub unsafe fn js_set_module_evaluated(ctx: *mut JSContext, m: *mut JSModuleDef) {
    (*m).set_evaluated(ctx.as_mut().unwrap());
}

pub unsafe fn gather_available_ancestors(
    ctx: *mut JSContext,
    module: *mut JSModuleDef,
    exec_list: &mut ExecModuleList,
) -> i32 {
    if js_check_stack_overflow((*ctx).rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        return -1;
    }

    for i in 0..(*module).async_parent_modules_count {
        let m = *(*module).async_parent_modules.offset(i as isize);
        if unsafe { !(*m).in_execution_list(exec_list) } && !(*(*m).cycle_root).eval_has_exception {
            debug_assert_eq!(
                (*m).status,
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            );
            debug_assert!(!(*m).eval_has_exception);
            debug_assert!((*m).async_evaluation);
            debug_assert!((*m).pending_async_dependencies > 0);

            (*m).pending_async_dependencies -= 1;
            if (*m).pending_async_dependencies == 0 {
                if js_resize_array(
                    ctx,
                    &mut exec_list.tab as *mut _ as *mut *mut _,
                    std::mem::size_of::<*mut JSModuleDef>() as _,
                    &mut exec_list.size,
                    exec_list.count + 1,
                ) != 0
                {
                    return -1;
                }
                *exec_list.tab.offset((*exec_list).count as isize) = m;
                exec_list.count += 1;

                if !(*m).has_tla {
                    if gather_available_ancestors(ctx, m, exec_list) < 0 {
                        return -1;
                    }
                }
            }
        }
    }

    0
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
