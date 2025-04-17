use std::borrow::Cow;

use crate::{
    atom::JSAtom,
    bytecode::run_bytecodes,
    class::js_create_from_ctor,
    context::{is_strict_mode, JSContext},
    error::{JSResult, JS_SetUncatchableError},
    function::{build_arg_list, CallArgs, JSAsyncFunctionState},
    stack_frame::JSStackFrame,
    types::{init_list_head, likely, list_head, unlikely},
    value::{
        object::{JSObject, JS_PROP_THROW, JS_PROP_THROW_STRICT},
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, JS_TAG_NULL,
        JS_TAG_OBJECT, JS_TAG_UNDEFINED, JS_VALUE_GET_PTR, JS_VALUE_GET_TAG, JS_VALUE_REF_COUNT,
    },
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_OBJECT, JS_MKPTR,
};

pub const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

/* must be large enough to have a negligible runtime cost and small
enough to call the interrupt callback often. */
pub const JS_INTERRUPT_COUNTER_INIT: i32 = 10000;

#[inline(never)]
pub unsafe fn __js_poll_interrupts(ctx: &mut JSContext) -> i32 {
    let rt = ctx.rt.as_mut();
    ctx.interrupt_counter = JS_INTERRUPT_COUNTER_INIT;

    if let Some(cb) = rt.interrupt_handler {
        if cb(rt, rt.interrupt_opaque) != 0 {
            // XXX: should set a specific flag to avoid catching
            ctx.JS_ThrowInternalError(c"interrupted");
            JS_SetUncatchableError(ctx, rt.current_exception, true);
            return -1;
        }
    }

    return 0;
}

#[inline(always)]
pub unsafe fn js_poll_interrupts(ctx: &mut JSContext) -> i32 {
    ctx.interrupt_counter -= 1;
    if ctx.interrupt_counter <= 0 {
        __js_poll_interrupts(ctx)
    } else {
        0
    }
}

/// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0.
pub unsafe fn JS_CallInternal(
    caller_ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValue,
    flags: i32,
) -> JSValue {
    if js_poll_interrupts(caller_ctx) != 0 {
        return JSValue::EXCEPTION;
    }

    let rt = caller_ctx.rt.as_mut();

    if JS_VALUE_GET_TAG(func_obj) != JSTag::Object as i32 {
        return if (flags & JS_CALL_FLAG_GENERATOR) != 0 {
            // func_obj 是生成器函数，可取得JSFuncAsyncState
            // JSFuncAsyncState中已设置本async function专用栈帧
            let s = JS_VALUE_GET_PTR(func_obj).cast::<JSAsyncFunctionState>();
            let sf = &mut (*s).frame;
            let p = sf.cur_func.object();
            let b = p.u.func.function_bytecode.as_ref().unwrap();

            let pc = sf.cur_pc_offset;
            let sp = sf.cur_sp_offset;
            sf.cur_sp_offset = 0;
            sf.prev_frame = rt.current_stack_frame;
            rt.current_stack_frame = sf;

            let sp_base = sf.stack_base_ptr().as_ptr();

            run_bytecodes(
                b,
                p.u.func.home_object,
                sf,
                caller_ctx,
                this_obj,
                new_target,
                &b.binary[b.bytecode_offset as usize..],
                pc,
                sp_base,
                sp_base.add(sp),
                &p.u.func.var_refs,
                argc,
                argv,
                b.js_mode as _,
                (*s).throw_flag,
            )
        } else {
            caller_ctx.JS_ThrowTypeError(c"not a function")
        };
    }

    let p = func_obj.object();
    if p.class_id() != JS_CLASS_BYTECODE_FUNCTION {
        return if let Some(cb) = (*rt).class_array[p.class_id() as usize].call {
            #[cfg(debug_assertions)]
            let argv_ref_count0 = if argc > 0 {
                std::slice::from_raw_parts(argv, argc as usize)
                    .iter()
                    .map(|x| JS_VALUE_REF_COUNT(*x))
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };

            let ret = cb(caller_ctx, func_obj, this_obj, argc, argv, flags);

            #[cfg(debug_assertions)]
            let argv_ref_count1 = if argc > 0 {
                std::slice::from_raw_parts(argv, argc as usize)
                    .iter()
                    .map(|x| JS_VALUE_REF_COUNT(*x))
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };

            #[cfg(debug_assertions)]
            for (a, b) in argv_ref_count0.iter().zip(argv_ref_count1.iter()) {
                debug_assert!(a.unwrap_or(0) <= b.unwrap_or(0));
            }

            ret
        } else {
            caller_ctx.JS_ThrowTypeError(c"not a function")
        };
    }

    let b = p.u.func.function_bytecode.as_ref().unwrap();

    // how many args that should be copied to local buf?
    // argc:实际调用时传入参数个数, b.arg_count:函数定义时参数个数
    let arg_copy_num = if argc < b.arg_count as i32 || flags & JS_CALL_FLAG_COPY_ARGV != 0 {
        b.arg_count
    } else {
        0
    };

    // how many bytes to allocate for local buf (args+vars+stack)?
    let buf_values_count = arg_copy_num as usize + b.var_count as usize + b.stack_size as usize;

    // let local_buf_size = std::mem::size_of::<JSValue>() * buf_values_count;
    // if js_check_stack_overflow(rt, local_buf_size) {
    //     return JS_ThrowStackOverflow(caller_ctx);
    // }

    // 新建执行栈帧
    let mut sf_s = JSStackFrame {
        prev_frame: std::ptr::null_mut(),
        cur_func: JSValue::UNDEFINED,
        local_buf: Vec::new(),
        arg_base_ptr: std::ptr::null_mut(),
        arg_count: 0,
        var_base_ptr: std::ptr::null_mut(),
        var_count: 0,
        cur_pc_offset: 0,
        cur_sp_offset: 0,
        var_ref_list: list_head::default(),
        js_mode: 0,
    };
    let sf = &mut sf_s;

    init_list_head(&mut sf.var_ref_list);

    sf.js_mode = b.js_mode as _;
    sf.cur_func = func_obj;

    // allocate local buffer and running bytecodes with it.
    // local_buf layout:
    //
    // +------------------+
    // | argument values  | <- arg_buf (optional, if need to copy arg)
    // | variable values  | <- var_buf
    // | stack space      | <- stack_buf
    // +------------------+
    //
    let mut local_buf = Vec::<JSValue>::new();
    if local_buf.try_reserve_exact(buf_values_count).is_err() {
        caller_ctx.JS_ThrowOutOfMemory();
        return JSValue::EXCEPTION;
    }

    if arg_copy_num != 0 {
        // copy arg values
        for i in 0..arg_copy_num as i32 {
            local_buf.push(if i < argc {
                (*argv.offset(i as isize)).dup() // <-- dup value
            } else {
                JSValue::UNDEFINED
            });
        }

        sf.arg_count = arg_copy_num;
        sf.arg_base_ptr = local_buf.as_mut_ptr()
    } else {
        sf.arg_count = argc as _;
        sf.arg_base_ptr = argv;
    }

    // initialize vars + stack section
    local_buf.extend(std::iter::repeat_n(
        JSValue::UNDEFINED,
        (b.var_count + b.stack_size) as usize,
    ));
    sf.var_base_ptr = local_buf.as_mut_ptr().add(arg_copy_num as usize);
    sf.var_count = b.var_count;

    debug_assert_eq!(local_buf.len(), buf_values_count);
    sf.local_buf = local_buf;

    // calc stack base in local buf
    let sp_base = sf.stack_base_ptr().as_ptr();

    sf.prev_frame = rt.current_stack_frame;
    rt.current_stack_frame = sf;

    run_bytecodes(
        b,
        p.u.func.home_object,
        sf,
        caller_ctx,
        this_obj,
        new_target,
        &b.binary[b.bytecode_offset as usize..],
        0,
        sp_base,
        sp_base,
        &p.u.func.var_refs,
        argc,
        argv,
        b.js_mode as _,
        false,
    )
}

pub unsafe fn JS_Call(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_CallInternal(
        ctx,
        func_obj,
        this_obj,
        JSValue::UNDEFINED,
        argc,
        argv,
        JS_CALL_FLAG_COPY_ARGV,
    )
}

/// call func_obj, and free func_obj
pub unsafe fn JS_CallFree(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let res: JSValue = JS_CallInternal(
        ctx,
        func_obj,
        this_obj,
        JSValue::UNDEFINED,
        argc,
        argv,
        JS_CALL_FLAG_COPY_ARGV,
    );
    JS_FreeValue(ctx, func_obj);
    res
}

/// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) == 0.
pub unsafe fn JS_CallConstructorInternal(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValue,
    mut flags: i32,
) -> JSValue {
    if js_poll_interrupts(ctx) != 0 {
        return JSValue::EXCEPTION;
    }
    if JS_VALUE_GET_TAG(func_obj) != JS_TAG_OBJECT {
        return ctx.JS_ThrowTypeError(c"not a function");
    }

    let p = func_obj.object();

    if unlikely(!p.detailed.is_constructor()) {
        return ctx.JS_ThrowTypeError(c"not a constructor");
    }

    flags |= JS_CALL_FLAG_CONSTRUCTOR;

    if (*p).class_id() != JS_CLASS_BYTECODE_FUNCTION {
        return if let Some(cb) = ctx.rt().class_array[(*p).class_id() as usize].call {
            cb(
                ctx,
                func_obj,
                new_target,
                argc,
                argv as *const JSValueConst,
                flags,
            )
        } else {
            ctx.JS_ThrowTypeError(c"not a function")
        };
    }

    let b = (*p).u.func.function_bytecode.as_mut().unwrap();
    if b.is_derived_class_constructor {
        return JS_CallInternal(
            ctx,
            func_obj,
            JSValue::UNDEFINED,
            new_target,
            argc,
            argv,
            flags,
        );
    } else {
        /* legacy constructor behavior */
        let this = js_create_from_ctor(ctx, new_target, JS_CLASS_OBJECT);
        if this.is_exception() {
            return JSValue::EXCEPTION;
        }

        let ret = JS_CallInternal(ctx, func_obj, this, new_target, argc, argv, flags);
        return if JS_VALUE_GET_TAG(ret) == JS_TAG_OBJECT || ret.is_exception() {
            JS_FreeValue(ctx, this);
            ret
        } else {
            JS_FreeValue(ctx, ret);
            this
        };
    }
}

pub unsafe fn JS_CallConstructor2(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_CallConstructorInternal(
        ctx,
        func_obj,
        new_target,
        argc,
        argv,
        JS_CALL_FLAG_COPY_ARGV,
    )
}

#[inline(always)]
pub unsafe fn JS_CallConstructor(
    ctx: &mut JSContext,
    func_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_CallConstructorInternal(ctx, func_obj, func_obj, argc, argv, JS_CALL_FLAG_COPY_ARGV)
}

/// `this_val` is not consumed
pub unsafe fn JS_Invoke(
    ctx: &mut JSContext,
    this_val: crate::value::JSValueConst,
    atom: JSAtom,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let func_obj: JSValue;
    func_obj = this_val.get_property(ctx, atom);
    if func_obj.is_exception() {
        return func_obj;
    }
    JS_CallFree(ctx, func_obj, this_val, argc, argv)
}

/// `this_val` is consumed
pub unsafe fn JS_InvokeFree(
    ctx: &mut JSContext,
    this_val: JSValue,
    atom: JSAtom,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let res: JSValue = JS_Invoke(ctx, this_val, atom, argc, argv);
    JS_FreeValue(ctx, this_val);
    res
}

/// returns 0, 1, -1
pub unsafe fn call_setter(
    ctx: &mut JSContext,
    setter: *mut JSObject,
    this_obj: JSValueConst,
    val: JSValue,
    flags: u32,
) -> i32 {
    if likely(!setter.is_null()) {
        /* Note: the field could be removed in the setter */
        let func = JS_DupValue(ctx, JS_MKPTR!(JSTag::Object, setter.cast()));

        let args_ptr: *const JSValueConst = &val;
        let ret = JS_CallFree(ctx, func, this_obj, 1, args_ptr as _);

        JS_FreeValue(ctx, val);
        if JS_IsException(ret) {
            return -1;
        }
        JS_FreeValue(ctx, ret);
        return 1;
    } else {
        JS_FreeValue(ctx, val);

        if (flags & JS_PROP_THROW) != 0
            || ((flags & JS_PROP_THROW_STRICT) != 0 && is_strict_mode(ctx))
        {
            ctx.JS_ThrowTypeError(c"no setter for property");
            return -1;
        }
        return 0;
    }
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub fn js_function_apply(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    args.this.ensure_callable(ctx)?;

    let this_arg = args.get_raw(0);
    let array_arg = args.get_raw(1);
    if (JS_VALUE_GET_TAG(array_arg) == JS_TAG_UNDEFINED
        || JS_VALUE_GET_TAG(array_arg) == JS_TAG_NULL)
        && args.magic != 2
    {
        return Ok(unsafe { JS_Call(ctx, args.this, this_arg, 0, std::ptr::null_mut()) });
    }

    unsafe {
        let mut arg_list = build_arg_list(ctx, array_arg)?;
        let ret = if args.magic & 1 != 0 {
            JS_CallConstructor2(
                ctx,
                args.this,
                this_arg,
                arg_list.len() as _,
                arg_list.as_mut_ptr(),
            )
        } else {
            JS_Call(
                ctx,
                args.this,
                this_arg,
                arg_list.len() as _,
                arg_list.as_mut_ptr(),
            )
        };

        for v in arg_list {
            JS_FreeValue(ctx, v);
        }

        Ok(ret)
    }
}

// // magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
// pub unsafe fn js_function_apply(
//     ctx: *mut JSContext,
//     this_val: JSValueConst,
//     argc: i32,
//     argv: *mut JSValueConst,
//     magic: i32,
// ) -> JSValue {
//     if check_function(ctx, this_val).is_err() {
//         return JSValue::EXCEPTION;
//     }

//     let this_arg = *argv.offset(0);
//     let array_arg = *argv.offset(1);
//     if (JS_VALUE_GET_TAG(array_arg) == JS_TAG_UNDEFINED
//         || JS_VALUE_GET_TAG(array_arg) == JS_TAG_NULL)
//         && magic != 2
//     {
//         return JS_Call(
//             ctx.as_mut().unwrap(),
//             this_val,
//             this_arg,
//             0,
//             std::ptr::null_mut(),
//         );
//     }

//     let mut args = if let Ok(a) = build_arg_list(ctx, array_arg) {
//         a
//     } else {
//         return JSValue::EXCEPTION;
//     };

//     let ret = if magic & 1 != 0 {
//         JS_CallConstructor2(
//             ctx.as_mut().unwrap(),
//             this_val,
//             this_arg,
//             args.len() as _,
//             args.as_mut_ptr(),
//         )
//     } else {
//         JS_Call(
//             ctx.as_mut().unwrap(),
//             this_val,
//             this_arg,
//             args.len() as _,
//             args.as_mut_ptr(),
//         )
//     };

//     for v in args {
//         JS_FreeValue(ctx, v);
//     }

//     ret
// }
