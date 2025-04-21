use std::ptr::NonNull;

use crate::{
    context::JSContext,
    error::{JSResult, JS_ThrowStackOverflow},
    function::{js_async_function_call, CallArgs, JS_Call},
    mem::{js_free, js_resize_array},
    module::JSModuleStatus,
    obsoletes::{JS_NewCFunctionData, JS_PromiseState},
    promise::{js_promise_then, JSPromiseStateEnum, JS_PromiseResult},
    runtime::js_check_stack_overflow,
    value::{JSValue, JSValueConst, JS_FreeValue, JS_VALUE_GET_PTR},
};

use super::{JSModuleDef, JS_NewModuleValue};

// spec: InnerModuleEvaluation. Return (index, JSValue::UNDEFINED) or (-1, exception)
pub unsafe fn js_inner_module_evaluation(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    index: i32,
    pstack_top: *mut *mut JSModuleDef,
    pvalue: *mut JSValue,
) -> i32 {
    let mut m1: *mut JSModuleDef;
    let mut i: i32;

    if js_check_stack_overflow((*ctx).rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        *pvalue = (*ctx).get_exception();
        return -1;
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

    if matches!(
        (*m).status,
        JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
            | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
    ) {
        if (*m).eval_has_exception {
            *pvalue = (*m).eval_exception.dup();
            return -1;
        } else {
            *pvalue = JSValue::UNDEFINED;
            return index;
        }
    }

    if (*m).status == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
        *pvalue = JSValue::UNDEFINED;
        return index;
    }
    debug_assert_eq!((*m).status, JSModuleStatus::JS_MODULE_STATUS_LINKED);

    (*m).status = JSModuleStatus::JS_MODULE_STATUS_EVALUATING;
    (*m).dfs_index = index;
    (*m).dfs_ancestor_index = index;
    (*m).pending_async_dependencies = 0;
    let mut index = index + 1;
    // push 'm' on stack
    (*m).stack_prev = *pstack_top;
    *pstack_top = m;

    let req_module_entries_count = (*m).req_module_entries_count;
    for i in 0..req_module_entries_count {
        let rme = (*m).req_module_entries.offset(i as isize);
        m1 = (*rme).module;
        index = js_inner_module_evaluation(ctx, m1, index, pstack_top, pvalue);
        if index < 0 {
            return -1;
        }
        debug_assert!(matches!(
            (*m1).status,
            JSModuleStatus::JS_MODULE_STATUS_EVALUATING
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
        ));
        if (*m1).status == JSModuleStatus::JS_MODULE_STATUS_EVALUATING {
            (*m).dfs_ancestor_index =
                std::cmp::min((*m).dfs_ancestor_index, (*m1).dfs_ancestor_index);
        } else {
            m1 = (*m1).cycle_root;
            debug_assert!(matches!(
                (*m1).status,
                JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
                    | JSModuleStatus::JS_MODULE_STATUS_EVALUATED
            ));
            if (*m1).eval_has_exception {
                *pvalue = (*m1).eval_exception.dup();
                return -1;
            }
        }

        if (*m1).async_evaluation {
            (*m).pending_async_dependencies += 1;
            if js_resize_array(
                ctx,
                &mut (*m1).async_parent_modules as *mut _ as *mut *mut _,
                std::mem::size_of::<*mut JSModuleDef>() as _,
                &mut (*m1).async_parent_modules_size,
                (*m1).async_parent_modules_count + 1,
            ) != 0
            {
                *pvalue = (*ctx).get_exception();
                return -1;
            }

            *(*m1)
                .async_parent_modules
                .offset((*m1).async_parent_modules_count as isize) = m;
            (*m1).async_parent_modules_count += 1;
        }
    }

    if (*m).pending_async_dependencies > 0 {
        debug_assert!(!(*m).async_evaluation);
        (*m).async_evaluation = true;
        (*m).async_evaluation_timestamp = (*ctx).rt().module_async_evaluation_next_timestamp;
        (*ctx).rt_mut().module_async_evaluation_next_timestamp += 1;
    } else if (*m).has_tla {
        debug_assert!(!(*m).async_evaluation);
        (*m).async_evaluation = true;
        (*m).async_evaluation_timestamp = (*ctx).rt().module_async_evaluation_next_timestamp;
        (*ctx).rt_mut().module_async_evaluation_next_timestamp += 1;
        js_execute_async_module(ctx, m);
    } else {
        if js_execute_sync_module(ctx, m, pvalue) < 0 {
            return -1;
        }
    }

    debug_assert!((*m).dfs_ancestor_index <= (*m).dfs_index);
    if (*m).dfs_index == (*m).dfs_ancestor_index {
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
            (*m1).cycle_root = m;
            if m1 == m {
                break;
            }
        }
    }

    *pvalue = JSValue::UNDEFINED;
    index
}

pub unsafe fn js_execute_async_module(ctx: *mut JSContext, m: *mut JSModuleDef) -> i32 {
    let promise = js_async_function_call(
        ctx.as_mut().unwrap(),
        (*m).func_obj,
        JSValue::UNDEFINED,
        0,
        std::ptr::null_mut(),
        0,
    );
    if promise.is_exception() {
        return -1;
    }

    let m_obj = JS_NewModuleValue(ctx, m);
    let mut resolve_funcs: [JSValue; 2] = [
        JS_NewCFunctionData(ctx, js_async_module_execution_fulfilled, 0, 0, 1, &m_obj),
        JS_NewCFunctionData(ctx, js_async_module_execution_rejected, 0, 0, 1, &m_obj),
    ];

    let ret_val = js_promise_then(ctx, promise, 2, resolve_funcs.as_mut_ptr());

    JS_FreeValue(ctx, ret_val);
    JS_FreeValue(ctx, m_obj);
    JS_FreeValue(ctx, resolve_funcs[0]);
    JS_FreeValue(ctx, resolve_funcs[1]);
    JS_FreeValue(ctx, promise);

    0
}

pub unsafe fn js_execute_sync_module(
    ctx: *mut JSContext,
    m: *mut JSModuleDef,
    pvalue: *mut JSValue,
) -> i32 {
    if let Some(cb) = (*m).init_func {
        // Native module init : no asynchronous execution
        if cb(ctx, m) < 0 {
            *pvalue = (*ctx).get_exception();
            return -1;
        }
    } else {
        let promise = js_async_function_call(
            ctx.as_mut().unwrap(),
            (*m).func_obj,
            JSValue::UNDEFINED,
            0,
            std::ptr::null_mut(),
            0,
        );
        if promise.is_exception() {
            *pvalue = (*ctx).get_exception();
            return -1;
        }

        let state = JS_PromiseState(ctx, promise).unwrap();
        if state == JSPromiseStateEnum::JS_PROMISE_FULFILLED {
            JS_FreeValue(ctx, promise);
        } else if state == JSPromiseStateEnum::JS_PROMISE_REJECTED {
            *pvalue = JS_PromiseResult(ctx, promise);
            JS_FreeValue(ctx, promise);
            return -1;
        } else {
            JS_FreeValue(ctx, promise);
            (*ctx).JS_ThrowTypeError(c"promise is pending");
            *pvalue = (*ctx).get_exception();
            return -1;
        }
    }

    *pvalue = JSValue::UNDEFINED;
    0
}

pub fn js_async_module_execution_rejected(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let module = unsafe {
        JS_VALUE_GET_PTR(args.data_raw(0))
            .cast::<JSModuleDef>()
            .as_mut()
            .unwrap()
    };
    let mut error = args.get_raw(0);

    if unsafe { js_check_stack_overflow((*ctx).rt(), 0) } {
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
    module.eval_exception = error.dup();
    module.status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;

    for i in 0..module.async_parent_modules_count {
        unsafe {
            let m = *module.async_parent_modules.offset(i as isize);
            let arglist = [error];
            let mut data = [JS_NewModuleValue(ctx, m)];
            let cc = CallArgs::new(
                NonNull::new_unchecked(ctx),
                JSValue::UNDEFINED,
                &arglist,
                0,
                Some(&mut data),
            );
            js_async_module_execution_rejected(ctx, cc);
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
                (*module).resolving_funcs[1],
                JSValue::UNDEFINED,
                1,
                &mut error,
            );
            JS_FreeValue(ctx, ret_val);
        }
    }

    Ok(JSValue::UNDEFINED)
}

struct ExecModuleList {
    pub tab: *mut *mut JSModuleDef,
    pub count: i32,
    pub size: i32,
}

pub fn js_async_module_execution_fulfilled(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let module = unsafe {
        JS_VALUE_GET_PTR(args.data_raw(0))
            .cast::<JSModuleDef>()
            .as_mut()
            .unwrap()
    };

    if module.status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
        debug_assert!((*module).eval_has_exception);
        return Ok(JSValue::UNDEFINED);
    }

    debug_assert_eq!(
        (*module).status,
        JSModuleStatus::JS_MODULE_STATUS_EVALUATING_ASYNC
    );
    debug_assert!(!(*module).eval_has_exception);
    debug_assert!((*module).async_evaluation);

    let mut exec_list_s = ExecModuleList {
        tab: std::ptr::null_mut(),
        count: 0,
        size: 0,
    };
    let exec_list = &mut exec_list_s;

    module.async_evaluation = false;
    unsafe {
        js_set_module_evaluated(ctx, module);
    }

    if unsafe { gather_available_ancestors(ctx, module, exec_list) } < 0 {
        unsafe {
            js_free(ctx, (*exec_list).tab.cast());
        }
        return Err(-1);
    }

    unsafe {
        // sort by increasing async_evaluation timestamp
        let md_lst = std::slice::from_raw_parts_mut((*exec_list).tab, (*exec_list).size as usize);
        md_lst.sort_by(|m1, m2| {
            let t1 = (*(*m1)).async_evaluation_timestamp;
            let t2 = (*(*m2)).async_evaluation_timestamp;
            t1.cmp(&t2)
        });

        for i in 0..(*exec_list).count {
            let m = *(*exec_list).tab.offset(i as isize);
            if (*m).status == JSModuleStatus::JS_MODULE_STATUS_EVALUATED {
                debug_assert!((*m).eval_has_exception);
            } else if (*m).has_tla {
                js_execute_async_module(ctx, m);
            } else {
                let mut error = JSValue::UNDEFINED;
                if js_execute_sync_module(ctx, m, &mut error) < 0 {
                    let arglist = [error];
                    let mut data = [JS_NewModuleValue(ctx, m)];
                    let cc = CallArgs::new(
                        NonNull::new_unchecked(ctx),
                        JSValue::UNDEFINED,
                        &arglist,
                        0,
                        Some(&mut data),
                    );
                    js_async_module_execution_rejected(ctx, cc).unwrap();
                    JS_FreeValue(ctx, data[0]);
                    JS_FreeValue(ctx, error);
                } else {
                    js_set_module_evaluated(ctx, m);
                }
            }
        }

        js_free(ctx, (*exec_list).tab.cast());
    }

    Ok(JSValue::UNDEFINED)
}

pub unsafe fn js_set_module_evaluated(ctx: *mut JSContext, m: *mut JSModuleDef) {
    (*m).status = JSModuleStatus::JS_MODULE_STATUS_EVALUATED;
    if !(*m).promise.is_undefined() {
        debug_assert_eq!((*m).cycle_root, m);

        let mut value = JSValue::UNDEFINED;
        let ret_val = JS_Call(
            ctx.as_mut().unwrap(),
            (*m).resolving_funcs[0],
            JSValue::UNDEFINED,
            1,
            &mut value,
        );
        JS_FreeValue(ctx, ret_val);
    }
}

pub unsafe fn gather_available_ancestors(
    ctx: *mut JSContext,
    module: *mut JSModuleDef,
    exec_list: *mut ExecModuleList,
) -> i32 {
    if js_check_stack_overflow((*ctx).rt(), 0) {
        JS_ThrowStackOverflow(ctx);
        return -1;
    }

    for i in 0..(*module).async_parent_modules_count {
        let m = *(*module).async_parent_modules.offset(i as isize);
        if unsafe { !find_in_exec_module_list(exec_list, m) }
            && !(*(*m).cycle_root).eval_has_exception
        {
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
                    &mut (*exec_list).tab as *mut _ as *mut *mut _,
                    std::mem::size_of::<*mut JSModuleDef>() as _,
                    &mut (*exec_list).size,
                    (*exec_list).count + 1,
                ) != 0
                {
                    return -1;
                }
                *(*exec_list).tab.offset((*exec_list).count as isize) = m;
                (*exec_list).count += 1;

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

pub unsafe fn find_in_exec_module_list(
    exec_list: *mut ExecModuleList,
    m: *mut JSModuleDef,
) -> bool {
    for i in 0..(*exec_list).count {
        if *(*exec_list).tab.offset(i as isize) == m {
            return true;
        }
    }
    false
}
