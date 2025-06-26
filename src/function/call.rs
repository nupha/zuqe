use std::ops::{Deref, DerefMut};

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
    atom::JSAtom,
    bytecode::run_bytecodes,
    context::JSContext,
    error::JSResult,
    function::{CallContext, build_arg_list},
    stack_frame::JSStackFrame,
    value::{JSValue, JSValueConst, object::JSObjectIsa},
};

pub const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

/// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0.
/// both `this_obj` and `argv` values are not consumed.
pub fn JS_CallInternal(
    caller_ctx: &JSContext,
    func_obj: &JSValue,
    this_obj: &JSValue,
    new_target: JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSValue {
    if caller_ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    let rt = caller_ctx.rt_mut();

    if !func_obj.is_object() {
        return if let JSValue::AsyncFuncResume(s) = func_obj {
            // `func_obj` is a generator function, get attached JSFuncAsyncState.
            // the JSFuncAsyncState provides a dedicated stack frame for the async function
            debug_assert!(flags & JS_CALL_FLAG_GENERATOR != 0);

            let mut s = s.0.borrow_mut();
            let throw_flag = s.throw_flag;
            let sf = unsafe { &mut s.as_mut().get_unchecked_mut().frame };

            let p = unsafe { sf.cur_func.object_unchecked().clone() };
            let (b, home_obj) =
                p.0.isa
                    .borrow()
                    .func()
                    .map(|f| (f.function_bytecode.clone().unwrap(), f.home_object.clone()))
                    .unwrap();

            let pc_offset = sf.cur_pc_offset;
            let sp_offset = std::mem::replace(&mut sf.cur_sp_offset, -1);
            debug_assert!(sp_offset >= 0);

            sf.prev_frame = rt.current_stack_frame;
            rt.current_stack_frame = (sf.deref_mut() as *mut JSStackFrame).cast();

            unsafe {
                run_bytecodes(
                    &b,
                    home_obj.as_ref(),
                    sf,
                    caller_ctx,
                    this_obj.clone(),
                    new_target,
                    b.bytecode_slice().deref(),
                    pc_offset,
                    sp_offset as usize,
                    p.0.isa.borrow().func().unwrap().var_refs.as_slice(),
                    args,
                    b.0.borrow().js_mode as _,
                    throw_flag,
                )
            }
        } else {
            caller_ctx.JS_ThrowTypeError("not a function");
            JSValue::EXCEPTION
        };
    }

    let p = unsafe { func_obj.object_unchecked() };
    let class_id = p.class_id();

    if class_id != JS_CLASS_BYTECODE_FUNCTION {
        // for non-bytecode function (e.g. bound function, native function, async function etc),
        // use callback defined on the class.
        return if let Some(cb) = rt.class_array[class_id as usize].call {
            cb(caller_ctx, func_obj, this_obj, args, flags).unwrap_or(JSValue::EXCEPTION)
        } else {
            caller_ctx.JS_ThrowTypeError("not a function");
            JSValue::EXCEPTION
        };
    }

    //
    // bytecode function
    //

    unsafe {
        func_obj
            .clone()
            .as_bytecode_function_unchecked()
            .call_internal(caller_ctx, this_obj.clone(), new_target, args, flags)
    }

    // let bf = p.u.func.function_bytecode.as_ref().unwrap();

    // // how many args that should be copied to local buf?
    // // argc:实际调用时传入参数个数, b.arg_count:函数定义时参数个数
    // let arg_copy_num = if argc < bf.arg_count as i32 || flags & JS_CALL_FLAG_COPY_ARGV != 0 {
    //     bf.arg_count
    // } else {
    //     0
    // };

    // // how many bytes to allocate for local buf (args+vars+stack)?
    // let buf_values_count = arg_copy_num as usize + bf.var_count as usize + bf.stack_size as usize;

    // let mut sf_s = JSStackFrame {
    //     prev_frame: std::ptr::null_mut(),
    //     cur_func: func_obj,
    //     local_buf: Vec::<JSValue>::new(),
    //     arg_base_ptr: std::ptr::null_mut(),
    //     arg_count: 0,
    //     var_base_ptr: std::ptr::null_mut(),
    //     var_count: 0,
    //     cur_pc_offset: 0,
    //     cur_sp_offset: 0,
    //     var_ref_list: list_head::default(),
    //     js_mode: bf.js_mode as _,
    // };
    // let sf = &mut sf_s;

    // init_list_head(&mut sf.var_ref_list);

    // // make local buffer and running bytecodes with it.
    // // local_buf layout:
    // //
    // // +------------------+
    // // | argument values  | <- arg_buf (optional, if need to copy arg)
    // // | variable values  | <- var_buf
    // // | stack space      | <- stack_buf
    // // +------------------+
    // //
    // if sf.local_buf.try_reserve_exact(buf_values_count).is_err() {
    //     caller_ctx.JS_ThrowOutOfMemory();
    //     return JSValue::EXCEPTION;
    // }

    // if arg_copy_num != 0 {
    //     // copy arg values
    //     for i in 0..arg_copy_num as i32 {
    //         sf.local_buf.push(if i < argc {
    //             (*argv.offset(i as isize)).dup() // <-- dup value
    //         } else {
    //             JSValue::UNDEFINED
    //         });
    //     }
    //     sf.arg_count = arg_copy_num;
    //     sf.arg_base_ptr = sf.local_buf.as_mut_ptr()
    // } else {
    //     sf.arg_count = argc as _;
    //     sf.arg_base_ptr = argv;
    // }

    // // initialize vars + stack section
    // sf.local_buf.extend(std::iter::repeat_n(
    //     JSValue::UNDEFINED,
    //     (bf.var_count + bf.stack_size) as usize,
    // ));
    // debug_assert_eq!(sf.local_buf.len(), buf_values_count);

    // sf.var_base_ptr = sf.local_buf.as_mut_ptr().add(arg_copy_num as usize);
    // sf.var_count = bf.var_count;

    // sf.prev_frame = rt.current_stack_frame;
    // rt.current_stack_frame = sf;

    // run_bytecodes(
    //     bf,
    //     p.u.func.home_object,
    //     sf,
    //     caller_ctx,
    //     this_obj,
    //     new_target,
    //     &bf.binary[bf.bytecode_offset as usize..],
    //     0,
    //     0,
    //     &p.u.func.var_refs,
    //     argc,
    //     argv,
    //     bf.js_mode as _,
    //     false,
    // )
}

#[inline(always)]
pub fn JS_Call(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_obj: &JSValue,
    args: &[JSValue],
) -> JSValue {
    JS_CallInternal(
        ctx,
        func_obj,
        this_obj,
        JSValue::UNDEFINED,
        args,
        JS_CALL_FLAG_COPY_ARGV,
    )
}

/// call func_obj, and free func_obj
#[deprecated(note = "use JS_Call() instead")]
pub fn JS_CallFree(
    ctx: &JSContext,
    func_obj: JSValueConst,
    this_obj: JSValueConst,
    args: &[JSValue],
) -> JSValue {
    JS_Call(ctx, &func_obj, &this_obj, args)
}

pub fn JS_CallConstructorInternal(
    ctx: &JSContext,
    func_obj: &JSValueConst,
    new_target: JSValueConst,
    args: &[JSValue],
    mut flags: i32,
) -> JSValue {
    if ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    if !func_obj.is_object() {
        ctx.JS_ThrowTypeError("not a function");
        return JSValue::EXCEPTION;
    }

    let p = unsafe { func_obj.object_unchecked() };
    if !p.flags().is_constructor() {
        ctx.JS_ThrowTypeError("not a constructor");
        return JSValue::EXCEPTION;
    }

    flags |= JS_CALL_FLAG_CONSTRUCTOR;
    let class_id = p.class_id();

    if class_id != JS_CLASS_BYTECODE_FUNCTION {
        return if let Some(cb) = ctx.rt().class_array[class_id as usize].call {
            cb(ctx, func_obj, &new_target, args, flags).unwrap_or(JSValue::EXCEPTION)
        } else {
            ctx.JS_ThrowTypeError("not a function");
            JSValue::EXCEPTION
        };
    }

    let bc =
        p.0.isa
            .borrow()
            .func()
            .unwrap()
            .function_bytecode
            .clone()
            .unwrap();
    if bc.0.borrow().is_derived_class_constructor {
        JS_CallInternal(ctx, func_obj, &JSValue::UNDEFINED, new_target, args, flags)
    } else {
        // legacy constructor behavior
        let this = if let Ok(v) = ctx.create_instance_from_ctor(&new_target, JSObjectIsa::OBJECT) {
            v
        } else {
            return JSValue::EXCEPTION;
        };

        let ret = JS_CallInternal(ctx, func_obj, &this, new_target, args, flags);

        if ret.is_object() || ret.is_exception() {
            ret
        } else {
            this
        }
    }
}

pub fn JS_CallConstructor2(
    ctx: &JSContext,
    func_obj: &JSValueConst,
    new_target: JSValueConst,
    args: &[JSValue],
) -> JSValue {
    JS_CallConstructorInternal(ctx, func_obj, new_target, args, JS_CALL_FLAG_COPY_ARGV)
}

#[inline(always)]
pub fn JS_CallConstructor(ctx: &JSContext, func_obj: &JSValue, args: &[JSValue]) -> JSValue {
    JS_CallConstructorInternal(
        ctx,
        func_obj,
        func_obj.clone(),
        args,
        JS_CALL_FLAG_COPY_ARGV,
    )
}

/// `this_val` is not consumed
pub fn JS_Invoke(ctx: &JSContext, this_val: &JSValue, atom: JSAtom, args: &[JSValue]) -> JSValue {
    let func_obj = this_val.get_property(ctx, atom);
    if func_obj.is_exception() {
        return JSValue::EXCEPTION;
    }
    JS_Call(ctx, &func_obj, this_val, args)
}

/// `this_val` is consumed
#[deprecated(note = "use JS_Invoke() instead")]
pub fn JS_InvokeFree(
    ctx: &JSContext,
    this_val: JSValue,
    atom: JSAtom,
    args: &[JSValue],
) -> JSValue {
    JS_Invoke(ctx, &this_val, atom, args)
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub fn js_function_apply(ctx: &JSContext, args: CallContext) -> JSResult {
    let this_val = args.this_raw();
    this_val.ensure_callable(ctx)?;

    let this_arg = args.get_raw(0);
    let array_arg = args.get_raw(1);

    if array_arg.is_undefined_or_null() && args.magic() != 2 {
        return Ok(unsafe { JS_Call(ctx, &this_val, &this_arg, &[]) });
    }

    let mut arg_list = build_arg_list(ctx, &array_arg)?;
    let ret = if args.magic() & 1 != 0 {
        JS_CallConstructor2(ctx, &this_val, this_arg, arg_list.as_ref())
    } else {
        JS_Call(ctx, &this_val, &this_arg, arg_list.as_ref())
    };
    Ok(ret)
}

// // magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
// pub unsafe fn js_function_apply(
//     ctx: &JSContext,
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
//             ctx,
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
//             ctx,
//             this_val,
//             this_arg,
//             args.len() as _,
//             args.as_mut_ptr(),
//         )
//     } else {
//         JS_Call(
//             ctx,
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
