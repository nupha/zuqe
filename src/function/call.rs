use std::ops::{Deref, DerefMut};

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
    atom::JSAtom,
    bytecode::run_bytecodes,
    context::JSContext,
    error::JSResult,
    function::build_arg_list,
    object::JSObjectIsa,
    stack_frame::JSStackFrame,
    value::{JSValue, JSValueConst},
};

pub struct CallContext<'a> {
    ctx: &'a JSContext,

    this_value: JSValue,
    args: &'a [JSValue],
    magic: i32,

    /// additional binding values
    data: Option<&'a mut [JSValue]>,
}

impl<'a> CallContext<'a> {
    /// `this_value` is not consumed, `args` values are not consumed, `data` values are not consumed.
    pub(crate) fn new(
        ctx: &'a JSContext,
        this_value: JSValue,
        args: &'a [JSValue],
        magic: i32,
        data: Option<&'a mut [JSValue]>,
    ) -> Self {
        Self {
            ctx,
            this_value,
            args,
            magic,
            data,
        }
    }

    pub fn ctx(&self) -> &JSContext {
        self.ctx
    }

    pub fn magic(&self) -> i32 {
        self.magic
    }

    /// get arguments count
    #[inline(always)]
    pub fn length(&self) -> usize {
        self.args.len()
    }

    /// get `this` value ref
    pub fn this(&self) -> &JSValue {
        &self.this_value
    }

    /// get `this` value, cloned
    pub fn this_raw(&self) -> JSValue {
        self.this_value.clone()
    }

    /// set new `this` value, drop old `this`.
    /// Note: `this_val` is consumed.
    pub fn set_this(&mut self, this_val: JSValue) {
        self.this_value = this_val;
    }

    /// get nth argument value ref
    #[inline(always)]
    pub fn arg(&self, nth: usize) -> &JSValue {
        self.args.get(nth).unwrap_or(&JSValue::UNDEFINED)
    }

    /// get nth argument value, cloned
    #[inline(always)]
    pub fn get_raw(&self, nth: usize) -> JSValue {
        self.args.get(nth).cloned().unwrap_or(JSValue::UNDEFINED)
    }

    /// get argument values slice
    #[inline(always)]
    pub fn arguments(&self) -> &[JSValue] {
        self.args
    }

    /// get nth extra data value.
    /// if you are referencing the value, must dup it by yourself.
    /// returned value is cloned.
    #[inline(always)]
    pub fn data_raw(&self, nth: usize) -> JSValue {
        self.data
            .as_deref()
            .and_then(|d| d.get(nth))
            .cloned()
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// set nth data value. returns old value, or Err if nth is out of range.
    /// Note: `val` is consumed.
    pub fn replace_data(&mut self, nth: usize, val: JSValue) -> Result<JSValue, i32> {
        if let Some(lst) = self.data.as_deref_mut() {
            if nth < lst.len() {
                return Ok(lst[nth].replace(val));
            }
        }

        Err(-1)
    }

    #[inline(always)]
    pub fn data_slice(&self) -> Option<&[JSValue]> {
        self.data.as_deref()
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IteratorState {
    /// iterator is not done
    HasMore,
    /// iterator is done
    Done,
    /// iterator object is not parsed
    Uninitialized,
}

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
        return if let JSValue::AsyncStateResume(state) = func_obj {
            // `func_obj` is a generator function, get attached JSFuncAsyncState.
            // the JSFuncAsyncState provides a dedicated stack frame for the async function
            debug_assert!(flags & JS_CALL_FLAG_GENERATOR != 0);

            let mut s = state.0.borrow_mut();
            let throw_flag = s.throw_flag;
            let frame = unsafe { &mut s.as_mut().get_unchecked_mut().frame };

            let p = unsafe { frame.cur_func.object_unchecked().clone() };
            let (b, home_obj) =
                p.0.isa
                    .borrow()
                    .func()
                    .map(|f| (f.function_bytecode.clone().unwrap(), f.home_object.clone()))
                    .unwrap();

            let pc_offset = frame.cur_pc_offset;
            let sp_offset = std::mem::replace(&mut frame.cur_sp_offset, -1);
            debug_assert!(sp_offset >= 0);

            frame.prev_frame = rt.current_stack_frame;
            rt.current_stack_frame = (frame.deref_mut() as *mut JSStackFrame).cast();

            unsafe {
                run_bytecodes(
                    &b,
                    home_obj.as_ref(),
                    frame,
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

    let class_id = unsafe { func_obj.object_unchecked().class_id() };
    if class_id == JS_CLASS_BYTECODE_FUNCTION {
        // bytecode function
        unsafe {
            func_obj
                .clone()
                .as_bytecode_function_unchecked()
                .call_internal(caller_ctx, this_obj.clone(), new_target, args, flags)
        }
    } else {
        // non-bytecode function: bound function, native function, async function etc.
        // use callback defined on the class.
        if let Some(cb) = rt.class_defs[class_id as usize].call {
            cb(caller_ctx, func_obj, this_obj, args, flags).unwrap_or(JSValue::EXCEPTION)
        } else {
            caller_ctx.JS_ThrowTypeError("not a function");
            JSValue::EXCEPTION
        }
    }
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
        return if let Some(cb) = ctx.rt().class_defs[class_id as usize].call {
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
        return Ok(JS_Call(ctx, &this_val, &this_arg, &[]));
    }

    let arg_list = build_arg_list(ctx, &array_arg)?;
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
