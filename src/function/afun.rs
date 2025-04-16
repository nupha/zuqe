use std::borrow::Cow;

use fallible_collections::FallibleVec;

use crate::{
    class::JSClassID,
    context::{JSContext, JS_MODE_ASYNC},
    error::JS_ThrowStackOverflow,
    gc::{
        add_gc_object, free_zero_refcount, remove_gc_object, JSGCObjectHeader, JSGCObjectTypeEnum,
        JSGCPhaseEnum, JS_MarkFunc,
    },
    mem::{js_free_rt, js_malloc_typed},
    promise::{js_promise_resolve, perform_promise_then},
    runtime::{js_check_stack_overflow, JSRuntime},
    stack_frame::JSStackFrame,
    types::{init_list_head, list_add, list_add_tail, list_del},
    value::{
        object::{JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE},
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_FreeValueRT, JS_UNDEFINED,
    },
    JS_CLASS_ASYNC_FUNCTION_RESOLVE, JS_MKPTR, JS_PROP_STRING_DEF,
};

use super::{
    call::{JS_CallInternal, JS_CALL_FLAG_GENERATOR},
    JSCFunctionListEntry, JS_Call,
};

#[repr(C)]
pub struct JSAsyncFunctionState {
    pub header: JSGCObjectHeader,

    pub this_val: JSValue, /* 'this' argument */
    pub argc: i32,         /* number of function arguments */

    /// used to throw an exception in JS_CallInternal()
    pub throw_flag: bool,

    /* TRUE if the function has returned. The stack frame is no longer valid */
    pub is_completed: bool,

    /// only used in JS async functions
    pub resolving_funcs: [JSValue; 2],

    /// the stack frame to run async function
    pub frame: JSStackFrame,
}

pub unsafe fn __async_func_free(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    /* cannot close the closure variables here because it would
    potentially modify the object graph */
    if !(*s).is_completed {
        async_func_free_frame(rt, s);
    }

    JS_FreeValueRT(rt, (*s).resolving_funcs[0]);
    JS_FreeValueRT(rt, (*s).resolving_funcs[1]);

    remove_gc_object(&mut (*s).header);

    if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES && (*s).header.ref_count != 0 {
        list_add_tail(&mut (*s).header.link, &mut (*rt).gc_zero_ref_count_list);
    } else {
        js_free_rt(rt, s.cast());
    }
}

pub unsafe fn async_func_free(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    (*s).header.ref_count -= 1;
    if (*s).header.ref_count == 0 {
        if (*rt).gc_phase != JSGCPhaseEnum::JS_GC_PHASE_REMOVE_CYCLES {
            list_del(&mut (*s).header.link);
            list_add(&mut (*s).header.link, &mut (*rt).gc_zero_ref_count_list);
            if (*rt).gc_phase == JSGCPhaseEnum::JS_GC_PHASE_NONE {
                free_zero_refcount(rt);
            }
        }
    }
}

// JSAsyncFunctionState (used by generator and async functions)
pub unsafe fn async_func_init(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
) -> *mut JSAsyncFunctionState {
    let mut s = if let Some(p) = js_malloc_typed::<JSAsyncFunctionState>(ctx, 1, true) {
        Box::from_raw(p.as_ptr())
    } else {
        return std::ptr::null_mut();
    };

    s.header.ref_count = 1;
    add_gc_object(
        (*ctx).rt_mut(),
        &mut s.header,
        JSGCObjectTypeEnum::JS_GC_OBJ_TYPE_ASYNC_FUNCTION,
    );

    let sf = &mut s.frame;
    init_list_head(&mut sf.var_ref_list);

    let p = func_obj.object();
    let b = p.u.func.function_bytecode.as_mut().unwrap();

    sf.js_mode = b.js_mode as i32 | JS_MODE_ASYNC;
    sf.cur_pc_offset = b.bytecode_offset as usize; // initialized to start of bytecode

    // make local buf, set up argument, variable, stack pointer
    let arg_count = std::cmp::max((*b).arg_count as usize, argc as usize);
    let mut local_buf = if let Ok(a) = Vec::<JSValue>::try_with_capacity(std::cmp::max(
        arg_count + b.var_count as usize + b.stack_size as usize,
        1,
    )) {
        a
    } else {
        drop(s);
        return std::ptr::null_mut();
    };

    for i in 0..argc as usize {
        local_buf[i] = JS_DupValue(ctx, *argv.add(i));
    }
    for i in argc as usize..arg_count + b.var_count as usize {
        local_buf[i] = JS_UNDEFINED;
    }

    sf.arg_base_ptr = local_buf.as_mut_ptr();
    sf.arg_count = arg_count as _;
    sf.var_base_ptr = sf.arg_base_ptr.add(sf.arg_count as usize);
    sf.var_count = b.var_count as _;
    sf.local_buf = Cow::Owned(local_buf);

    sf.cur_func = JS_DupValue(ctx, func_obj);
    sf.cur_sp_offset = 0;

    // setup async state
    s.argc = argc;
    s.this_val = JS_DupValue(ctx, this_obj);
    s.resolving_funcs[0] = JS_UNDEFINED;
    s.resolving_funcs[1] = JS_UNDEFINED;
    s.is_completed = false;

    Box::into_raw(s)
}

pub unsafe fn async_func_resume(ctx: &mut JSContext, s: *mut JSAsyncFunctionState) -> JSValue {
    assert!(!(*s).is_completed);

    let sf = &mut (*s).frame;

    let mut ret = if js_check_stack_overflow(ctx.rt_mut(), 0) {
        JS_ThrowStackOverflow(ctx)
    } else {
        /* the tag does not matter provided it is not an object */
        let func_obj = JS_MKPTR!(JSTag::Int, s.cast());
        JS_CallInternal(
            ctx,
            func_obj,
            (*s).this_val,
            JSValue::UNDEFINED,
            (*s).argc,
            sf.arg_base_ptr,
            JS_CALL_FLAG_GENERATOR,
        )
    };

    if ret.is_undefined_or_null() {
        if ret.is_undefined() {
            debug_assert!(sf.cur_sp_offset > 0);
            let sp_base = sf.stack_base_ptr();
            ret = (*sp_base.as_ptr().add(sf.cur_sp_offset - 1)).replace(JSValue::UNDEFINED);
        }
        /* end of execution */
        (*s).is_completed = true;

        /* close the closure variables. */
        sf.close_var_refs(ctx);

        async_func_free_frame(ctx.rt_mut(), s);
    }

    ret
}

pub unsafe fn js_async_function_resolve_create(
    ctx: &mut JSContext,
    s: *mut JSAsyncFunctionState,
) -> Result<[JSValue; 2], i32> {
    let mut resolving_funcs = [JSValue::UNDEFINED; 2];

    for i in 0..2 {
        let f = JS_NewObjectProtoClass(
            ctx,
            ctx.function_proto,
            JS_CLASS_ASYNC_FUNCTION_RESOLVE + i as JSClassID,
        );

        if f.is_exception() {
            JS_FreeValue(ctx, resolving_funcs[0]);
            JS_FreeValue(ctx, resolving_funcs[1]);
            return Err(-1);
        }

        let p = f.object();
        (*s).header.ref_count += 1;
        p.u.async_function_data = s;

        resolving_funcs[i] = f;
    }

    return Ok(resolving_funcs);
}

pub unsafe fn async_func_free_frame(rt: *mut JSRuntime, s: *mut JSAsyncFunctionState) {
    let sf = &mut (*s).frame;

    if !sf.arg_base_ptr.is_null() {
        // cannot free the function if it is running
        let mut p = sf.arg_base_ptr;
        let cur_sp = sf.stack_current_ptr().as_ptr();
        while p < cur_sp {
            JS_FreeValueRT(rt, *p);
            p = p.add(1);
        }

        sf.arg_base_ptr = std::ptr::null_mut();
    }

    JS_FreeValueRT(rt, sf.cur_func);
    JS_FreeValueRT(rt, (*s).this_val);
}

pub unsafe fn js_async_function_resolve_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let p = func_obj.object();
    let s = p.u.async_function_data;
    let is_reject = (p.class_id() - JS_CLASS_ASYNC_FUNCTION_RESOLVE) != 0;

    let arg = if argc > 0 {
        *argv.offset(0)
    } else {
        JSValue::UNDEFINED
    };

    (*s).throw_flag = is_reject;
    if is_reject {
        ctx.JS_Throw(arg.dup());
    } else {
        // return value of await
        *(*s)
            .frame
            .stack_base_ptr()
            .as_ptr()
            .add((*s).frame.cur_sp_offset - 1) = arg.dup();
    }

    js_async_function_resume(ctx, s);
    JSValue::UNDEFINED
}

/// call an 'async function XXX() {...}' like function
pub unsafe fn js_async_function_call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let s = async_func_init(ctx, func_obj, this_obj, argc, argv);
    if s.is_null() {
        return JSValue::EXCEPTION;
    }

    if let Ok((promise, resolving_funcs)) = ctx.create_promise(JSValue::UNDEFINED) {
        (*s).resolving_funcs = resolving_funcs;
        js_async_function_resume(ctx, s);
        async_func_free(ctx.rt_mut(), s);
        promise
    } else {
        async_func_free(ctx.rt_mut(), s);
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_async_function_resume(ctx: *mut JSContext, s: *mut JSAsyncFunctionState) {
    let func_ret = async_func_resume(ctx.as_mut().unwrap(), s);

    if (*s).is_completed {
        let ret2: JSValue;

        if func_ret.is_exception() {
            let error = (*ctx).get_exception();
            ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[1], // reject
                JSValue::UNDEFINED,
                1,
                [error].as_mut_ptr(),
            );
            JS_FreeValue(ctx, error);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        } else {
            // normal return
            ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[0], // resolve
                JSValue::UNDEFINED,
                1,
                [func_ret].as_mut_ptr(),
            );
            JS_FreeValue(ctx, func_ret);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        }
    } else {
        let mut i: i32;
        let mut res: i32;

        let cur_sp = (*s)
            .frame
            .stack_base_ptr()
            .as_ptr()
            .add((*s).frame.cur_pc_offset);

        let mut value = std::ptr::replace(cur_sp.offset(-1), JSValue::UNDEFINED);

        // await
        JS_FreeValue(ctx, func_ret); // not used

        let promise = js_promise_resolve(ctx, (*ctx).promise_ctor, 1, &mut value, 0);
        JS_FreeValue(ctx, value);
        if promise.is_exception() {
            let mut error = (*ctx).get_exception();
            let ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[1],
                JSValue::UNDEFINED,
                1,
                &mut error,
            );
            JS_FreeValue(ctx, error);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        }

        let resolving_funcs =
            if let Ok(rr) = js_async_function_resolve_create(ctx.as_mut().unwrap(), s) {
                rr
            } else {
                JS_FreeValue(ctx, promise);
                let mut error = (*ctx).get_exception();
                let ret2 = JS_Call(
                    ctx.as_mut().unwrap(),
                    (*s).resolving_funcs[1],
                    JSValue::UNDEFINED,
                    1,
                    &mut error,
                );
                JS_FreeValue(ctx, error);
                JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
                [JSValue::UNDEFINED; 2]
            };

        let res = perform_promise_then(
            ctx.as_mut().unwrap(),
            promise,
            &resolving_funcs,
            &[JSValue::UNDEFINED; 2], // no need to create 'thrownawayCapability' as in the spec
        );
        JS_FreeValue(ctx, promise);
        JS_FreeValue(ctx, resolving_funcs[0]);
        JS_FreeValue(ctx, resolving_funcs[1]);
        if res.is_err() {
            let mut error = (*ctx).get_exception();
            let ret2 = JS_Call(
                ctx.as_mut().unwrap(),
                (*s).resolving_funcs[1],
                JSValue::UNDEFINED,
                1,
                &mut error,
            );
            JS_FreeValue(ctx, error);
            JS_FreeValue(ctx, ret2); /* XXX: what to do if exception ? */
        }
    }
}

pub unsafe fn js_async_function_resolve_finalizer(rt: *mut JSRuntime, val: JSValue) {
    unimplemented!();
}

pub unsafe fn js_async_function_resolve_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    unimplemented!();
}

pub const js_async_function_proto_funcs: [JSCFunctionListEntry; 1] = [JS_PROP_STRING_DEF!(
    c"[Symbol.toStringTag]",
    c"AsyncFunction",
    JS_PROP_CONFIGURABLE
)];
