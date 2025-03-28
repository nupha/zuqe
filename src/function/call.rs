use std::borrow::Cow;

use smallvec::SmallVec;

use crate::{
    atom::JSAtom,
    bytecode::{arithmatic::js_same_value, run_bytecodes},
    class::js_create_from_ctor,
    context::{is_strict_mode, JSContext},
    error::{JS_SetUncatchableError, JS_ThrowStackOverflow},
    function::{
        build_arg_list, check_function, free_arg_list, js_create_iterator_result,
        JSAsyncFunctionState, JSCFunctionDataRecord, JSCFunctionEnum,
    },
    runtime::js_check_stack_overflow,
    stack_frame::JSStackFrame,
    types::{init_list_head, likely, unlikely},
    value::{
        object::{JSObject, JS_GetOpaque, JS_GetProperty, JS_PROP_THROW, JS_PROP_THROW_STRICT},
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, JS_ToFloat64,
        JS_TAG_NULL, JS_TAG_OBJECT, JS_TAG_UNDEFINED, JS_UNDEFINED, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_PTR, JS_VALUE_GET_TAG, JS_VALUE_REF_COUNT, SIZEOF_JSVALUE,
    },
    JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_C_FUNCTION_DATA, JS_CLASS_OBJECT, JS_MKPTR,
};

pub const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

/* must be large enough to have a negligible runtime cost and small
enough to call the interrupt callback often. */
pub const JS_INTERRUPT_COUNTER_INIT: i32 = 10000;

#[inline(never)]
pub unsafe fn __js_poll_interrupts(ctx: *mut JSContext) -> i32 {
    let rt = (*ctx).rt.as_mut();
    (*ctx).interrupt_counter = JS_INTERRUPT_COUNTER_INIT;

    if let Some(cb) = rt.interrupt_handler {
        if cb(rt, rt.interrupt_opaque) != 0 {
            // XXX: should set a specific flag to avoid catching
            ctx.as_mut().unwrap().JS_ThrowInternalError(c"interrupted");
            JS_SetUncatchableError(ctx, rt.current_exception, true);
            return -1;
        }
    }

    return 0;
}

#[inline(always)]
pub unsafe fn js_poll_interrupts(ctx: *mut JSContext) -> i32 {
    (*ctx).interrupt_counter -= 1;
    if unlikely((*ctx).interrupt_counter <= 0) {
        __js_poll_interrupts(ctx)
    } else {
        0
    }
}

/* argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0. */
pub unsafe fn JS_CallInternal(
    caller_ctx: *mut JSContext,
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

    let rt = (*caller_ctx).rt.as_mut();

    if JS_VALUE_GET_TAG(func_obj) != JSTag::Object as i32 {
        return if (flags & JS_CALL_FLAG_GENERATOR) != 0 {
            // func_obj 是生成器函数，可取得JSFuncAsyncState
            // JSFuncAsyncState中已设置本async function专用栈帧
            let s = JS_VALUE_GET_PTR(func_obj).cast::<JSAsyncFunctionState>();
            let sf = &mut (*s).frame;
            let p = JS_VALUE_GET_OBJ(sf.cur_func);
            let b = (*p).u.func.function_bytecode.as_ref().unwrap();

            let pc = sf.cur_pc_offset;
            let sp = sf.cur_sp_offset;
            sf.cur_sp_offset = 0;
            sf.prev_frame = rt.current_stack_frame;
            rt.current_stack_frame = sf;

            let sp_base = (*sf).stack_base_ptr().as_ptr();

            run_bytecodes(
                b,
                (*p).u.func.home_object,
                sf,
                caller_ctx.as_mut().unwrap(),
                this_obj,
                new_target,
                &b.binary[b.bytecode_offset as usize..],
                pc,
                sp_base,
                sp_base.add(sp),
                &(*p).u.func.var_refs,
                argc,
                argv,
                b.js_mode as _,
                (*s).throw_flag,
                &mut [],
            )
        } else {
            caller_ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"not a function")
        };
    }

    let p = JS_VALUE_GET_OBJ(func_obj);
    if (*p).class_id() != JS_CLASS_BYTECODE_FUNCTION {
        return if let Some(cb) = (*(*rt).class_array.add((*p).detailed.class_id() as usize)).call {
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
            caller_ctx
                .as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"not a function")
        };
    }

    let b = (*p).u.func.function_bytecode.as_ref().unwrap();

    // how many args that should be copied to local buf?
    // argc:实际调用时传入参数个数, b.arg_count:函数定义时参数个数
    let arg_copy_num = if argc < b.arg_count as i32 || flags & JS_CALL_FLAG_COPY_ARGV != 0 {
        b.arg_count
    } else {
        0
    };

    // how many bytes to allocate for local buf (args+vars+stack)?
    let local_buf_size = std::mem::size_of::<JSValue>()
        * (arg_copy_num as usize + b.var_count as usize + b.stack_size as usize);

    if js_check_stack_overflow(rt, local_buf_size) {
        return JS_ThrowStackOverflow(caller_ctx);
    }

    // 新建执行栈帧
    let mut sf_s = std::mem::zeroed::<JSStackFrame>();
    let sf = &mut sf_s;

    init_list_head(&mut sf.var_ref_list);
    sf.js_mode = b.js_mode as _;
    sf.cur_func = func_obj;

    // allocate local buffer and running bytecodes with it
    let retval = crate::mem::alloca::with_alloca(local_buf_size, |local_buf| {
        // local_buf layout:
        //
        // +------------------+
        // | argument values  | <- arg_buf (optional, if need to copy arg)
        // | variable values  | <- var_buf
        // | stack space      | <- stack_buf
        // +------------------+
        //

        if arg_copy_num != 0 {
            // copy arg values to local buf
            let arg_buf = local_buf.as_mut_ptr().cast::<JSValue>();
            for i in 0..arg_copy_num as i32 {
                *arg_buf.offset(i as isize) = if i < argc {
                    (*argv.offset(i as isize)).dup()
                } else {
                    JSValue::UNDEFINED
                };
            }
            sf.arg_count = (*b).arg_count;
            sf.arg_base_ptr = arg_buf
        } else {
            sf.arg_count = argc as _;
            sf.arg_base_ptr = argv;
        }

        // initial vars in local buf
        {
            sf.var_count = b.var_count;
            sf.var_base_ptr = local_buf
                .as_mut_ptr()
                .cast::<JSValue>()
                .add(arg_copy_num as usize);

            for i in 0..b.var_count {
                *sf.var_base_ptr.offset(i as isize) = JSValue::UNDEFINED;
            }
        }

        sf.local_buf = Cow::Borrowed(std::slice::from_raw_parts(
            local_buf.as_ptr().cast::<JSValue>(),
            local_buf_size / std::mem::size_of::<JSValue>(),
        ));

        // calc stack base in local buf
        let sp_base = sf.stack_base_ptr().as_ptr();

        sf.prev_frame = (*rt).current_stack_frame;
        (*rt).current_stack_frame = sf;

        run_bytecodes(
            b,
            (*p).u.func.home_object,
            sf,
            caller_ctx.as_mut().unwrap(),
            this_obj,
            new_target,
            &b.binary[b.bytecode_offset as usize..],
            0,
            sp_base,
            sp_base,
            &(*p).u.func.var_refs,
            argc,
            argv,
            b.js_mode as _,
            false,
            std::slice::from_raw_parts_mut(
                local_buf.as_mut_ptr().cast::<JSValue>(),
                local_buf_size / std::mem::size_of::<JSValue>(),
            ),
        )
    });

    retval
}

pub unsafe fn JS_Call(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_CallInternal(
        ctx,
        func_obj,
        this_obj,
        JS_UNDEFINED,
        argc,
        argv,
        JS_CALL_FLAG_COPY_ARGV,
    )
}

/// call func_obj, and free func_obj
pub unsafe fn JS_CallFree(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let res: JSValue = JS_CallInternal(
        ctx,
        func_obj,
        this_obj,
        JS_UNDEFINED,
        argc,
        argv,
        JS_CALL_FLAG_COPY_ARGV,
    );
    JS_FreeValue(ctx, func_obj);
    res
}

// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) == 0.
pub unsafe fn JS_CallConstructorInternal(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    new_target: JSValueConst,
    argc: i32,
    argv: *mut JSValue,
    mut flags: i32,
) -> JSValue {
    if js_poll_interrupts(ctx) != 0 {
        return JSValue::EXCEPTION;
    }
    if unlikely(JS_VALUE_GET_TAG(func_obj) != JS_TAG_OBJECT) {
        return ctx.as_mut().unwrap().JS_ThrowTypeError(c"not a function");
    }

    let p = JS_VALUE_GET_OBJ(func_obj).as_mut().unwrap();

    if unlikely(!p.detailed.is_constructor()) {
        return ctx
            .as_mut()
            .unwrap()
            .JS_ThrowTypeError(c"not a constructor");
    }

    flags |= JS_CALL_FLAG_CONSTRUCTOR;

    if (*p).class_id() != JS_CLASS_BYTECODE_FUNCTION {
        return if let Some(cb) = (*(*ctx).rt().class_array.add((*p).class_id() as usize)).call {
            return cb(
                ctx,
                func_obj,
                new_target,
                argc,
                argv as *const JSValueConst,
                flags,
            );
        } else {
            (*ctx).JS_ThrowTypeError(c"not a function")
        };
    }

    let b = (*p).u.func.function_bytecode.as_mut().unwrap();
    if b.is_derived_class_constructor {
        return JS_CallInternal(ctx, func_obj, JS_UNDEFINED, new_target, argc, argv, flags);
    } else {
        /* legacy constructor behavior */
        let this = js_create_from_ctor(ctx, new_target, JS_CLASS_OBJECT);
        if JS_IsException(this) {
            return JSValue::EXCEPTION;
        }
        let ret = JS_CallInternal(ctx, func_obj, this, new_target, argc, argv, flags);
        if JS_VALUE_GET_TAG(ret) == JS_TAG_OBJECT || JS_IsException(ret) {
            JS_FreeValue(ctx, this);
            return ret;
        } else {
            JS_FreeValue(ctx, ret);
            return this;
        }
    }
}

pub unsafe fn JS_CallConstructor2(
    ctx: *mut JSContext,
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

pub unsafe fn JS_CallConstructor(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    JS_CallConstructorInternal(ctx, func_obj, func_obj, argc, argv, JS_CALL_FLAG_COPY_ARGV)
}

pub unsafe fn JS_Invoke(
    ctx: *mut JSContext,
    this_val: crate::value::JSValueConst,
    atom: JSAtom,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let func_obj: JSValue;
    func_obj = JS_GetProperty(ctx, this_val, atom);
    if JS_IsException(func_obj) {
        return func_obj;
    }
    JS_CallFree(ctx, func_obj, this_val, argc, argv)
}

pub unsafe fn JS_InvokeFree(
    ctx: *mut JSContext,
    this_val: JSValue,
    atom: JSAtom,
    argc: i32,
    argv: *mut JSValueConst,
) -> JSValue {
    let res: JSValue = JS_Invoke(ctx, this_val, atom, argc, argv);
    JS_FreeValue(ctx, this_val);
    res
}

pub unsafe fn js_call_c_function(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    mut this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let rt = (*ctx).rt.as_mut();

    let p = JS_VALUE_GET_OBJ(func_obj);
    let cproto = (*p).u.cfunc.cproto;
    let arg_count = (*p).u.cfunc.length;

    // better to always check stack overflow
    if js_check_stack_overflow(rt, std::mem::size_of::<JSValueConst>() * arg_count as usize) {
        return JS_ThrowStackOverflow(ctx);
    }

    let mut sf_s: JSStackFrame = std::mem::zeroed();
    let sf = &mut sf_s;

    sf.cur_func = func_obj;
    sf.prev_frame = rt.current_stack_frame;
    rt.current_stack_frame = sf;

    let ctx = (*p).u.cfunc.realm.as_mut().unwrap(); // change the current realm

    let mut local_buf: Option<SmallVec<[JSValue; 16]>> = None;
    if unlikely(argc < arg_count as i32) {
        let mut arr = SmallVec::<[JSValue; 16]>::new();
        for i in 0..argc {
            arr.push(*argv.offset(i as isize));
        }
        // empty slots, if any
        for _ in argc..arg_count as i32 {
            arr.push(JS_UNDEFINED);
        }

        (*sf).arg_count = arg_count as _;
        (*sf).arg_base_ptr = arr.as_mut_ptr();

        // keep smallvec alive till exit this function
        local_buf.replace(arr);
    } else {
        (*sf).arg_count = argc as _;
        (*sf).arg_base_ptr = argv as *mut JSValue;
    }

    macro_rules! exit {
        ($retval:expr) => {
            (*rt).current_stack_frame = (*sf).prev_frame;
            return $retval;
        };
    }

    let arg_buf = (*sf).arg_base_ptr;
    let func = &(*p).u.cfunc.c_function;
    let mut ret_val: JSValue;

    match cproto {
        JSCFunctionEnum::Constructor
        | JSCFunctionEnum::ConstructorOrFunc
        | JSCFunctionEnum::Generic => {
            if matches!(
                cproto,
                JSCFunctionEnum::ConstructorOrFunc | JSCFunctionEnum::Constructor
            ) {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    if cproto == JSCFunctionEnum::Constructor {
                        exit!(ctx.JS_ThrowTypeError(c"must be called with new"));
                    } else {
                        this_obj = JS_UNDEFINED;
                    }
                }
            }
            ret_val = (func.generic)(ctx, this_obj, argc, arg_buf as *mut JSValue);
        }
        JSCFunctionEnum::ConstructorMagic
        | JSCFunctionEnum::ConstructorOrFuncMagic
        | JSCFunctionEnum::GenericMagic => {
            if matches!(
                cproto,
                JSCFunctionEnum::ConstructorMagic | JSCFunctionEnum::ConstructorOrFuncMagic
            ) {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    if cproto == JSCFunctionEnum::ConstructorMagic {
                        exit!(ctx.JS_ThrowTypeError(c"must be called with new"));
                    } else {
                        this_obj = JS_UNDEFINED;
                    }
                }
            }
            ret_val =
                (func.generic_magic)(ctx, this_obj, argc, arg_buf as _, (*p).u.cfunc.magic as i32);
        }
        JSCFunctionEnum::Getter => {
            ret_val = (func.getter)(ctx, this_obj);
        }
        JSCFunctionEnum::Setter => {
            ret_val = (func.setter)(ctx, this_obj, *arg_buf.offset(0));
        }
        JSCFunctionEnum::GetterMagic => {
            ret_val = (func.getter_magic)(ctx, this_obj, (*p).u.cfunc.magic as _);
        }
        JSCFunctionEnum::SetterMagic => {
            ret_val =
                (func.setter_magic)(ctx, this_obj, *arg_buf.offset(0), (*p).u.cfunc.magic as _);
        }
        JSCFunctionEnum::FF => {
            let mut d1: f64 = 0.0;
            ret_val = if unlikely(JS_ToFloat64(ctx, &mut d1, *arg_buf.offset(0)) != 0) {
                JSValue::EXCEPTION
            } else {
                JSValue::from_f64((func.f_f)(d1))
            };
        }
        JSCFunctionEnum::FFF => {
            let mut d1: f64 = 0.0;
            let mut d2: f64 = 0.0;
            ret_val = if unlikely(JS_ToFloat64(ctx, &mut d1, *arg_buf.offset(0)) != 0)
                || unlikely(JS_ToFloat64(ctx, &mut d2, *arg_buf.offset(1)) != 0)
            {
                JSValue::EXCEPTION
            } else {
                JSValue::from_f64((func.f_f_f)(d1, d2))
            };
        }
        JSCFunctionEnum::IteratorNext => {
            let mut done = 0;
            ret_val = (func.iterator_next)(
                ctx,
                this_obj,
                argc,
                arg_buf,
                &mut done,
                (*p).u.cfunc.magic as _,
            );
            if !JS_IsException(ret_val) && done != 2 {
                ret_val = js_create_iterator_result(ctx, ret_val, done != 0);
            }
        }
    }

    (*rt).current_stack_frame = (*sf).prev_frame;
    ret_val
}

pub unsafe fn js_c_function_data_call(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    this_val: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let s = JS_GetOpaque(func_obj, JS_CLASS_C_FUNCTION_DATA)
        .cast::<JSCFunctionDataRecord>()
        .as_mut()
        .unwrap();

    // XXX: could add the function on the stack for debug
    if unlikely(argc < s.length as i32) {
        crate::mem::alloca::with_alloca_zeroed(SIZEOF_JSVALUE * s.length as usize, |sbuf| {
            let args = std::slice::from_raw_parts_mut(
                sbuf.as_mut_ptr().cast::<JSValue>(),
                s.length as usize,
            );
            for i in 0..argc as usize {
                args[i] = *argv.offset(i as isize);
            }
            for i in argc as usize..s.length as usize {
                args[i] = JS_UNDEFINED;
            }

            (s.func)(
                ctx,
                this_val,
                argc,
                args.as_mut_ptr(),
                s.magic as _,
                s.data.as_mut_ptr(),
            )
        })
    } else {
        (s.func)(
            ctx,
            this_val,
            argc,
            argv as _,
            s.magic as _,
            s.data.as_mut_ptr(),
        )
    }
}

pub unsafe fn js_call_bound_function(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    argc: i32,
    argv: *const JSValueConst,
    flags: i32,
) -> JSValue {
    let p = JS_VALUE_GET_OBJ(func_obj).as_mut().unwrap();
    let bf = p.u.bound_function.as_mut().unwrap();
    let arg_count = bf.argc + argc;

    let alloca_size = SIZEOF_JSVALUE * arg_count as usize;
    if js_check_stack_overflow((*ctx).rt_mut(), alloca_size) {
        return JS_ThrowStackOverflow(ctx);
    }

    crate::mem::alloca::with_alloca_zeroed(alloca_size, |sbuf| {
        let arg_list =
            std::slice::from_raw_parts_mut(sbuf.as_mut_ptr().cast::<JSValue>(), arg_count as usize);

        for i in 0..bf.argc as usize {
            arg_list[i] = *(bf.argv.as_ptr().add(i));
        }
        for i in 0..argc as usize {
            arg_list[bf.argc as usize + i] = *argv.offset(i as isize);
        }

        if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
            let mut new_target = this_obj;
            if js_same_value(ctx, func_obj, new_target) {
                new_target = bf.func_obj;
            }

            JS_CallConstructor2(
                ctx,
                (*bf).func_obj,
                new_target,
                arg_count,
                arg_list.as_mut_ptr(),
            )
        } else {
            JS_Call(
                ctx,
                bf.func_obj,
                bf.this_val,
                arg_count,
                arg_list.as_mut_ptr(),
            )
        }
    })

    // original C code
    // let arg_buf = alloca(alloca_size) as *mut JSValueConst; // gcc code
    // for i in 0..(*bf).argc {
    //     *arg_buf.offset(i as isize) = *(bf.argv.as_ptr().offset(i as isize));
    // }
    // for i in 0..argc {
    //     *arg_buf.offset(bf.argc as isize + i as isize) = *argv.offset(i as isize);
    // }

    // if flags & JS_CALL_FLAG_CONSTRUCTOR != 0 {
    //     let mut new_target = this_obj;
    //     if js_same_value(ctx, func_obj, new_target) {
    //         new_target = bf.func_obj;
    //     }
    //     JS_CallConstructor2(ctx, (*bf).func_obj, new_target, arg_count, arg_buf)
    // } else {
    //     JS_Call(ctx, (*bf).func_obj, (*bf).this_val, arg_count, arg_buf)
    // }
}

pub unsafe fn call_setter(
    ctx: *mut JSContext,
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
            ctx.as_mut()
                .unwrap()
                .JS_ThrowTypeError(c"no setter for property");
            return -1;
        }
        return 0;
    }
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub unsafe fn js_function_apply(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    if check_function(ctx, this_val).is_err() {
        return JSValue::EXCEPTION;
    }

    let this_arg = *argv.offset(0);
    let array_arg = *argv.offset(1);
    if (JS_VALUE_GET_TAG(array_arg) == JS_TAG_UNDEFINED
        || JS_VALUE_GET_TAG(array_arg) == JS_TAG_NULL)
        && magic != 2
    {
        return JS_Call(ctx, this_val, this_arg, 0, std::ptr::null_mut());
    }

    let mut len: u32 = 0;
    let tab = build_arg_list(ctx, &mut len, array_arg);
    if tab.is_null() {
        return JSValue::EXCEPTION;
    }

    let ret = if magic & 1 != 0 {
        JS_CallConstructor2(ctx, this_val, this_arg, len as _, tab)
    } else {
        JS_Call(ctx, this_val, this_arg, len as _, tab)
    };

    free_arg_list(ctx, tab, len);

    ret
}
