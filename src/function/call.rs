use std::{cell::RefCell, ops::DerefMut, rc::Rc};

use crate::{
    atom::Atom,
    branch_hints::unlikely,
    context::JSContext,
    error::JSResult,
    function::build_arg_list,
    object::{JSObject, ObjectPayload},
    stack_frame::StackFrame,
    value::JSValue,
};

pub struct CallContext<'cc, 'o: 'cc> {
    ctx: &'cc JSContext<'o>,
    this_value: JSValue<'o>,
    args: &'cc [JSValue<'o>],
    magic: i32,

    /// additional binding values
    data: Option<Rc<RefCell<Box<[JSValue<'o>]>>>>,
}

impl<'cc, 'o: 'cc> CallContext<'cc, 'o> {
    pub(crate) const fn new(
        ctx: &'cc JSContext<'o>,
        this_value: JSValue<'o>,
        args: &'cc [JSValue<'o>],
        magic: i32,
        data: Option<Rc<RefCell<Box<[JSValue<'o>]>>>>,
    ) -> Self {
        Self {
            ctx,
            this_value,
            args,
            magic,
            data,
        }
    }

    #[inline(always)]
    pub(crate) const fn new_simple(
        ctx: &'cc JSContext<'o>,
        this: JSValue<'o>,
        args: &'cc [JSValue<'o>],
    ) -> Self {
        Self::new(ctx, this, args, 0, None)
    }

    pub const fn ctx(&self) -> &JSContext<'o> {
        self.ctx
    }

    pub const fn magic(&self) -> i32 {
        self.magic
    }

    /// get arguments count
    #[inline(always)]
    pub const fn length(&self) -> usize {
        self.args.len()
    }

    /// get `this` value ref
    #[inline(always)]
    pub const fn this(&self) -> &JSValue<'o> {
        &self.this_value
    }

    /// set new `this` value, drop old `this`.
    /// Note: `this_val` is consumed.
    pub fn set_this(&mut self, this_val: JSValue<'o>) {
        self.this_value = this_val;
    }

    /// get nth argument value ref
    #[inline(always)]
    pub fn arg(&self, nth: usize) -> &JSValue<'o> {
        self.args.get(nth).unwrap_or(&JSValue::UNDEFINED)
    }

    /// get argument values slice
    #[inline(always)]
    pub const fn arguments(&self) -> &[JSValue<'o>] {
        self.args
    }

    /// get nth extra data value.
    /// returned value is cloned, caller should free it after use.
    #[inline]
    pub fn data(&self, nth: usize) -> JSValue<'o> {
        self.data
            .as_ref()
            .and_then(|d| d.borrow().get(nth).cloned())
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// data length
    #[inline]
    pub fn data_len(&self) -> usize {
        self.data.as_ref().map(|d| d.borrow().len()).unwrap_or(0)
    }

    /// Replace nth data with new `val`, and returns old value.
    /// returns `None` if `nth` is out of range.
    pub fn replace_data(&mut self, nth: usize, val: JSValue<'o>) -> Option<JSValue<'o>> {
        if let Some(data) = self.data.as_ref() {
            let mut lst = data.borrow_mut();
            if nth < lst.len() {
                return Some(std::mem::replace(&mut lst[nth], val));
            }
        }

        None
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

impl<'a> JSObject<'a> {
    /// call `self` object. if object is not callable, returns not function error.
    #[inline(always)]
    pub fn call(
        &self,
        ctx: &JSContext<'a>,
        this_obj: &JSValue<'a>,
        args: &[JSValue<'a>],
    ) -> JSResult<'a> {
        JS_CallInternal(
            ctx,
            &self.clone().into(),
            this_obj,
            JSValue::UNDEFINED,
            args,
            JS_CALL_FLAG_COPY_ARGV,
        )
        .err()
    }

    /// call function `self.method`
    ///
    /// `method` is not consumed
    #[inline]
    pub fn invoke(&self, ctx: &JSContext<'a>, method: &Atom, args: &[JSValue<'a>]) -> JSResult<'a> {
        self.get_property(ctx, method)?
            .call(ctx, &self.clone().into(), args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None
    pub fn invoke_defined(
        &self,
        ctx: &JSContext<'a>,
        method: &Atom,
        args: &[JSValue<'a>],
    ) -> Result<Option<JSResult<'a>>, i32> {
        let m = self.get_property(ctx, method)?;
        if !m.is_undefined_or_null() {
            return Ok(Some(m.call(ctx, &self.clone().into(), args)));
        }
        Ok(None)
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    pub fn invoke_callable(
        &self,
        ctx: &JSContext<'a>,
        method: &Atom,
        args: &[JSValue<'a>],
    ) -> Result<Option<JSResult<'a>>, i32> {
        let m = self.get_property(ctx, method)?;
        if m.is_function() {
            Ok(Some(m.call(ctx, &self.clone().into(), args)))
        } else {
            Ok(None)
        }
    }
}

impl<'a> JSValue<'a> {
    /// call `self` value. if value is not callable, returns 'not function error'.
    #[inline(always)]
    pub fn call(
        &self,
        ctx: &JSContext<'a>,
        this_obj: &JSValue<'a>,
        args: &[JSValue<'a>],
    ) -> JSResult<'a> {
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
    pub fn invoke(&self, ctx: &JSContext<'a>, method: &Atom, args: &[JSValue<'a>]) -> JSResult<'a> {
        self.get_property(ctx, method)?.call(ctx, self, args)
    }

    /// call `self.method` function if property `method` is neither null nor undefined.
    /// otherwise, returns None.
    pub fn invoke_defined(
        &self,
        ctx: &JSContext<'a>,
        method: &Atom,
        args: &[JSValue<'a>],
    ) -> Result<Option<JSResult<'a>>, i32> {
        let m = self.get_property(ctx, method)?;
        if !m.is_undefined_or_null() {
            return Ok(Some(m.call(ctx, self, args)));
        }
        Ok(None)
    }

    /// call `self.method` if property `method` is callable.
    /// otherwise, returns None
    ///
    /// `method` is not consumed
    pub fn invoke_callable(
        &self,
        ctx: &JSContext<'a>,
        method: &Atom,
        args: &[JSValue<'a>],
    ) -> Result<Option<JSResult<'a>>, i32> {
        let m = self.get_property(ctx, method)?;
        if m.is_function() {
            let r = m.call(ctx, self, args);
            Ok(Some(r))
        } else {
            Ok(None)
        }
    }
}

/// argv[] is modified if (flags & JS_CALL_FLAG_COPY_ARGV) = 0.
/// both `this_obj` and `argv` values are not consumed.
pub fn JS_CallInternal<'a>(
    caller_ctx: &JSContext<'a>,
    func_obj: &JSValue<'a>,
    this_obj: &JSValue<'a>,
    new_target: JSValue<'a>,
    args: &[JSValue<'a>],
    flags: i32,
) -> JSValue<'a> {
    if caller_ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    match func_obj {
        JSValue::Object(o) => {
            if let Some(bf) = o.as_BytecodeFunction() {
                bf.call(caller_ctx, this_obj, new_target, args, flags)
            } else {
                // non bytecode function: bound function, native function, async function etc.
                // use callback defined on the class.
                if let Some(call_func) = caller_ctx.rt().class_registry[o.class_id() as usize].call
                {
                    call_func(caller_ctx, func_obj, this_obj, args, flags)
                        .unwrap_or(JSValue::EXCEPTION)
                } else {
                    caller_ctx.throw_type_error("not callable");
                    JSValue::EXCEPTION
                }
            }
        }
        JSValue::AsyncStateResume(state) => {
            // `func_obj` is a generator function
            debug_assert!(flags & JS_CALL_FLAG_GENERATOR != 0);

            let mut s = state.0.borrow_mut();
            let throw_flag = s.throw_flag;

            // state has a dedicated stack frame for async function
            let frame = unsafe { &mut s.as_mut().get_unchecked_mut().frame };
            let bcf = frame.cur_func.as_object_BCFunction().unwrap().clone();
            let isa = bcf.payload();

            // let p = unsafe { frame.cur_func.as_object_unchecked().clone() };
            // let isa = p.payload().func().unwrap();

            // let (bc, home_obj) = p
            //     .payload()
            //     .func()
            //     .map(|f| (f.bytecode(), f.home_object.as_ref()))
            //     .unwrap();

            let pc_offset = frame.cur_pc_offset;
            let sp_offset = frame.cur_sp_offset;
            debug_assert!(sp_offset >= 0);

            frame.cur_sp_offset = -1;

            let rt = caller_ctx.rt_mut();
            frame.prev_frame = rt.cur_stack_frame;
            rt.cur_stack_frame = (frame.deref_mut() as *mut StackFrame).cast();

            let ret_val = isa.bytecode().run(
                caller_ctx,
                isa.home_object().as_ref(),
                frame,
                this_obj,
                new_target,
                pc_offset,
                sp_offset as usize,
                isa.var_refs.borrow().as_slice(),
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

pub fn JS_CallConstructorInternal<'a>(
    ctx: &JSContext<'a>,
    func_obj: &JSValue<'a>,
    new_target: JSValue<'a>,
    args: &[JSValue<'a>],
    mut flags: i32,
) -> JSValue<'a> {
    if ctx.poll_interrupts().is_err() {
        return JSValue::EXCEPTION;
    }

    if let JSValue::Object(o) = func_obj {
        if !o.flags().is_constructor() {
            ctx.throw_type_error("not a constructor");
            return JSValue::EXCEPTION;
        }

        flags |= JS_CALL_FLAG_CONSTRUCTOR;

        if let Some(bcf) = o.as_BytecodeFunction() {
            // BYTECODE_FUNCTION
            if bcf.payload().bytecode().attrs.is_derived_class_constructor {
                JS_CallInternal(ctx, func_obj, &JSValue::UNDEFINED, new_target, args, flags)
            } else {
                // legacy constructor behavior
                let this: JSValue = if let Ok(o) =
                    ctx.create_object_from_ctor(&new_target, ObjectPayload::OBJECT)
                {
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
            // call class exotic handler
            if let Some(cb) = ctx.rt().class_registry[o.class_id() as usize].call {
                cb(ctx, func_obj, &new_target, args, flags).unwrap_or(JSValue::EXCEPTION)
            } else {
                ctx.throw_type_error("not a function");
                JSValue::EXCEPTION
            }
        }

        // original code
        // if unlikely(class_id != JS_CLASS_BYTECODE_FUNCTION) {
        //     if let Some(cb) = ctx.rt().class_defs[class_id as usize].call {
        //         cb(ctx, func_obj, &new_target, args, flags).unwrap_or(JSValue::EXCEPTION)
        //     } else {
        //         ctx.throw_type_error("not a function");
        //         JSValue::EXCEPTION
        //     }
        // } else {
        //     // BYTECODE_FUNCTION
        //     if o.as_BytecodeFunction()
        //         .unwrap()
        //         .payload()
        //         .bytecode()
        //         .attrs
        //         .is_derived_class_constructor
        //     {
        //         JS_CallInternal(ctx, func_obj, &JSValue::UNDEFINED, new_target, args, flags)
        //     } else {
        //         // legacy constructor behavior
        //         let this: JSValue = if let Ok(o) =
        //             ctx.create_object_from_ctor(&new_target, ObjectPayload::OBJECT)
        //         {
        //             o.into()
        //         } else {
        //             return JSValue::EXCEPTION;
        //         };

        //         let ret = JS_CallInternal(ctx, func_obj, &this, new_target, args, flags);
        //         if ret.is_object() || ret.is_exception() {
        //             ret
        //         } else {
        //             this
        //         }
        //     }
        // }
    } else {
        ctx.throw_type_error("not a function");
        JSValue::EXCEPTION
    }
}

pub fn JS_CallConstructor2<'a>(
    ctx: &JSContext<'a>,
    func_obj: &JSValue<'a>,
    new_target: JSValue<'a>,
    args: &[JSValue<'a>],
) -> JSValue<'a> {
    JS_CallConstructorInternal(ctx, func_obj, new_target, args, JS_CALL_FLAG_COPY_ARGV)
}

#[inline(always)]
pub fn JS_CallConstructor<'a>(
    ctx: &JSContext<'a>,
    func_obj: &JSValue<'a>,
    args: &[JSValue<'a>],
) -> JSValue<'a> {
    JS_CallConstructorInternal(
        ctx,
        func_obj,
        func_obj.clone(),
        args,
        JS_CALL_FLAG_COPY_ARGV,
    )
}

// magic value: 0 = normal apply, 1 = apply for constructor, 2 = Reflect.apply
pub fn js_function_apply<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    let this_val = cc.this();
    this_val.ensure_function(ctx)?;

    let this_arg = cc.arg(0);
    let array_arg = cc.arg(1);

    if unlikely(array_arg.is_undefined_or_null() && cc.magic() != 2) {
        this_val.call(ctx, this_arg, &[])
    } else {
        let arg_list = build_arg_list(ctx, array_arg)?;

        if cc.magic() & 1 != 0 {
            JS_CallConstructor2(ctx, this_val, this_arg.clone(), unsafe {
                std::mem::transmute(arg_list.as_ref())
            })
            .err()
        } else {
            this_val.call(ctx, this_arg, unsafe {
                std::mem::transmute(arg_list.as_ref())
            })
        }
    }
}
