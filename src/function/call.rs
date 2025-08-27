use std::ops::DerefMut;

use crate::{
    JS_CLASS_BYTECODE_FUNCTION,
    atom::JSAtom,
    context::JSContext,
    error::JSResult,
    function::{BytecodeFunction, FuncObjPayload, build_arg_list},
    object::{JSObject, ObjectPayload},
    stack_frame::StackFrame,
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

    /// get argument values slice
    #[inline(always)]
    pub fn arguments(&self) -> &[JSValue] {
        self.args
    }

    /// get nth extra data value.
    /// returned value is cloned, caller should free it after use.
    #[inline(always)]
    pub fn data(&self, nth: usize) -> JSValue {
        self.data
            .as_deref()
            .and_then(|d| d.get(nth))
            .cloned()
            .unwrap_or(JSValue::UNDEFINED)
    }

    #[inline(always)]
    pub fn data_slice(&self) -> Option<&[JSValue]> {
        self.data.as_deref()
    }

    /// set nth data value. returns old value, or Err if nth is out of range.
    /// Note: `val` is consumed.
    pub fn replace_data(&mut self, nth: usize, val: JSValue) -> JSResult {
        if let Some(lst) = self.data.as_deref_mut() {
            if nth < lst.len() {
                return Ok(std::mem::replace(lst.get_mut(nth).unwrap(), val));
            }
        }

        Err(-1)
    }
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IteratorState {
    /// iterator is not done
    NotDone,
    /// iterator is done
    Done,
    /// iterator object is not parsed
    NotParsed,
}

pub const JS_CALL_FLAG_CONSTRUCTOR: i32 = 1 << 0;
pub const JS_CALL_FLAG_COPY_ARGV: i32 = 1 << 1;
pub const JS_CALL_FLAG_GENERATOR: i32 = 1 << 2;

impl JSObject {
    /// call function `self.method`
    #[inline]
    pub fn invoke(&self, ctx: &JSContext, method: JSAtom, args: &[JSValue]) -> JSResult {
        self.get_property(ctx, method)?
            .call(ctx, &self.clone().into(), args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None
    pub fn invoke_defined(
        &self,
        ctx: &JSContext,
        method: JSAtom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if !m.is_undefined_or_null() {
            return Ok(Some(m.call(ctx, &self.clone().into(), args)));
        }
        Ok(None)
    }
}

impl JSValue {
    /// call `self`. if `self` value is not callable, returns not function error.
    #[inline(always)]
    pub fn call(&self, ctx: &JSContext, this_obj: &Self, args: &[JSValue]) -> JSResult {
        JS_CallInternal(
            ctx,
            self,
            this_obj,
            JSValue::UNDEFINED,
            args,
            JS_CALL_FLAG_COPY_ARGV,
        )
        .err()
    }

    /// call function `self.method`
    #[inline]
    pub fn invoke(&self, ctx: &JSContext, method: JSAtom, args: &[JSValue]) -> JSResult {
        self.get_property(ctx, method)?.call(ctx, self, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None
    pub fn invoke_defined(
        &self,
        ctx: &JSContext,
        method: JSAtom,
        args: &[JSValue],
    ) -> Result<Option<JSResult>, i32> {
        let m = self.get_property(ctx, method)?;
        if !m.is_undefined_or_null() {
            return Ok(Some(m.call(ctx, self, args)));
        }
        Ok(None)
    }
}

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

    match func_obj {
        JSValue::Object(o) => {
            if let Ok(bf) = BytecodeFunction::try_from(o.clone()) {
                bf.call(caller_ctx, this_obj, new_target, args, flags)
            } else {
                // non bytecode function: bound function, native function, async function etc.
                // use callback defined on the class.
                if let Some(cb) = caller_ctx.rt().class_defs[o.class_id() as usize].call {
                    cb(caller_ctx, func_obj, this_obj, args, flags).unwrap_or(JSValue::EXCEPTION)
                } else {
                    caller_ctx.throw_type_error("not callable");
                    JSValue::EXCEPTION
                }
            }
        }
        JSValue::AsyncStateResume(state) => {
            // `func_obj` is a generator function, get attached JSFuncAsyncState.
            // the JSFuncAsyncState provides a dedicated stack frame for the async function
            debug_assert!(flags & JS_CALL_FLAG_GENERATOR != 0);

            let mut s = state.0.borrow_mut();
            let throw_flag = s.throw_flag;
            let frame = unsafe { &mut s.as_mut().get_unchecked_mut().frame };

            let p = unsafe { frame.cur_func.object_unchecked().clone() };
            let (bc, home_obj) =
                p.0.payload
                    .borrow()
                    .func()
                    .map(|f| (f.bytecode().unwrap(), f.home_object.clone()))
                    .unwrap();

            let pc_offset = frame.cur_pc_offset;
            let sp_offset = std::mem::replace(&mut frame.cur_sp_offset, -1);
            debug_assert!(sp_offset >= 0);

            let rt = caller_ctx.rt_mut();
            frame.prev_frame = rt.cur_stack_frame;
            rt.cur_stack_frame = (frame.deref_mut() as *mut StackFrame).cast();

            let ret_val = bc.run(
                caller_ctx,
                home_obj.as_ref(),
                frame,
                this_obj,
                new_target,
                pc_offset,
                sp_offset as usize,
                p.0.payload.borrow().func().unwrap().var_refs.as_slice(),
                args,
                throw_flag,
            );

            debug_assert_eq!(
                caller_ctx.rt().cur_stack_frame,
                frame.prev_frame,
                "runtime current stack frame should be restored after running bytecodes"
            );

            ret_val
        }
        _ => {
            caller_ctx.throw_type_error("not a function");
            JSValue::EXCEPTION
        }
    }
}

pub fn JS_CallConstructorInternal(
    ctx: &JSContext,
    func_obj: &JSValue,
    new_target: JSValue,
    args: &[JSValue],
    mut flags: i32,
) -> JSValue {
    if ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    if let JSValue::Object(p) = func_obj {
        if !p.flags().is_constructor() {
            ctx.throw_type_error("not a constructor");
            return JSValue::EXCEPTION;
        }

        flags |= JS_CALL_FLAG_CONSTRUCTOR;
        let class_id = p.class_id();

        if class_id != JS_CLASS_BYTECODE_FUNCTION {
            return if let Some(cb) = ctx.rt().class_defs[class_id as usize].call {
                cb(ctx, func_obj, &new_target, args, flags).unwrap_or(JSValue::EXCEPTION)
            } else {
                ctx.throw_type_error("not a function");
                JSValue::EXCEPTION
            };
        }

        let bc =
            p.0.payload
                .borrow()
                .func()
                .and_then(FuncObjPayload::bytecode)
                .unwrap();

        if bc.0.borrow().is_derived_class_constructor {
            JS_CallInternal(ctx, func_obj, &JSValue::UNDEFINED, new_target, args, flags)
        } else {
            // legacy constructor behavior
            let this: JSValue =
                if let Ok(o) = ctx.create_object_apply_ctor(&new_target, ObjectPayload::OBJECT) {
                    o.into()
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
    } else {
        ctx.throw_type_error("not a function");
        JSValue::EXCEPTION
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

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub fn js_function_apply(ctx: &JSContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    this_val.ensure_callable(ctx)?;

    let this_arg = cc.arg(0);
    let array_arg = cc.arg(1);

    if array_arg.is_undefined_or_null() && cc.magic() != 2 {
        return this_val.call(ctx, this_arg, &[]);
    }

    let arg_list = build_arg_list(ctx, &array_arg)?;
    if cc.magic() & 1 != 0 {
        JS_CallConstructor2(ctx, this_val, this_arg.clone(), arg_list.as_ref()).err()
    } else {
        this_val.call(ctx, &this_arg, arg_list.as_ref())
    }
}
