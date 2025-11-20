use std::ops::Deref;

use gc_lite::{Collect, GcWeak};

use crate::{
    atom::Atom,
    branch_hints::unlikely,
    context::{ContextImpl, ContextRef},
    defer,
    error::JSResult,
    function::{CallContext, IteratorState, JS_CALL_FLAG_CONSTRUCTOR},
    iterator::js_create_iterator_result,
    object::{ObjectImpl, ObjectPayload, ObjectRef},
    runtime::js_check_stack_overflow,
    stack_frame::StackFrame,
    value::JSValue,
};

use super::js_function_set_properties;

pub type NativeCallback = fn(ctx: ContextRef, cc: CallContext) -> JSResult;
pub type NativeGetter = fn(ctx: ContextRef, cc: CallContext) -> JSResult;
pub type NativeSetter = fn(ctx: ContextRef, cc: CallContext) -> JSResult;
pub type NativeIteratorNext =
    fn(ctx: ContextRef, cc: CallContext, iter_state: &mut IteratorState) -> JSResult;

pub type JSCFunction = unsafe fn(ctx: &ContextImpl, this_val: JSValue, args: &[JSValue]) -> JSValue;
pub type JSCFunctionMagic = unsafe fn(
    ctx: &ContextImpl,
    this_val: JSValue,
    args: &[JSValue],
    magic: ::core::ffi::c_int,
) -> JSValue;

pub type JSCFunctionGetter = unsafe fn(&ContextImpl, JSValue) -> JSValue;
pub type JSCFunctionGetterMagic =
    unsafe fn(ctx: &ContextImpl, this_val: JSValue, magic: i32) -> JSValue;

pub type JSCFunctionSetter = unsafe fn(&ContextImpl, JSValue, JSValue) -> JSValue;
pub type JSCFunctionSetterMagic = unsafe fn(&ContextImpl, JSValue, JSValue, i32) -> JSValue;

pub type JSCFunctionData = unsafe fn(
    ctx: &ContextImpl,
    this_val: JSValue,
    args: &[JSValue],
    magic: i32,
    func_data: &mut [JSValue],
) -> JSValue;

pub type JSCFunctionIteratorNext =
    fn(ctx: &ContextImpl, this: JSValue, args: &[JSValue], done: &mut i32, magic: i32) -> JSValue;

pub type JSCFunctionFF = unsafe fn(f64) -> f64;
pub type JSCFunctionFFF = unsafe fn(f64, f64) -> f64;

#[repr(u8)]
#[derive(Debug, Clone, Copy)]
pub enum NativeFunctionType {
    Func(NativeCallback),
    Getter(NativeGetter),
    Setter(NativeSetter),
    Constructor(NativeCallback),
    ConstructorOrFunc(NativeCallback),
    IteratorNext(NativeIteratorNext),

    CFunc(JSCFunction),
    CFuncMagic(JSCFunctionMagic),
    CConstructor(JSCFunction),
    CConstructorOrFunc(JSCFunction),
    CConstructorMagic(JSCFunctionMagic),
    CConstructorOrFuncMagic(JSCFunctionMagic),
    CGetter(JSCFunctionGetter),
    CGetterMagic(JSCFunctionGetterMagic),
    CSetter(JSCFunctionSetter),
    CSetterMagic(JSCFunctionSetterMagic),
    CIteratorNext(JSCFunctionIteratorNext),

    FF(JSCFunctionFF),
    FFF(JSCFunctionFFF),
}

impl PartialEq for NativeFunctionType {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Func(a), Self::Func(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Getter(a), Self::Getter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Setter(a), Self::Setter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Constructor(a), Self::Constructor(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::ConstructorOrFunc(a), Self::ConstructorOrFunc(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::IteratorNext(a), Self::IteratorNext(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CFunc(a), Self::CFunc(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CFuncMagic(a), Self::CFuncMagic(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CConstructor(a), Self::CConstructor(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CConstructorOrFunc(a), Self::CConstructorOrFunc(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::CConstructorMagic(a), Self::CConstructorMagic(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::CConstructorOrFuncMagic(a), Self::CConstructorOrFuncMagic(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }
            (Self::CGetter(a), Self::CGetter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CGetterMagic(a), Self::CGetterMagic(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CSetter(a), Self::CSetter(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CSetterMagic(a), Self::CSetterMagic(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::CIteratorNext(a), Self::CIteratorNext(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::FF(a), Self::FF(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::FFF(a), Self::FFF(b)) => std::ptr::fn_addr_eq(*a, *b),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NFuncPayload {
    pub handler: NativeFunctionType,
    pub realm: GcWeak<ContextImpl>,
    pub length: u8,
    pub magic: i16,
}

unsafe impl Collect for NFuncPayload {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::Tracer) {}
}

impl NFuncPayload {
    pub fn class_call(
        ctx: ContextRef,
        func_obj: JSValue,
        mut this_obj: JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let mut rt = ctx.rt();

        let (callback, arg_length, magic, realm) = if let ObjectPayload::C_FUNCTION(c) =
            unsafe { func_obj.as_object_unchecked().payload() }
        {
            (
                c.handler,
                c.length,
                c.magic,
                ContextRef::from(c.realm.upgrade(ctx.rt().gc_heap()).unwrap()),
            )
        } else {
            unreachable!()
        };

        // better to always check stack overflow
        if js_check_stack_overflow(
            rt.deref(),
            std::mem::size_of::<JSValue>() * arg_length as usize,
        ) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // new stack frame, alloc on stack.
        let mut new_frame = StackFrame {
            prev_frame: rt.cur_stack_frame,
            cur_func: func_obj,
            ..Default::default()
        };
        let sf = &mut new_frame;

        // switch current stack frame to new.
        // switch back to original when exit
        let orig_frame = rt.cur_stack_frame;
        rt.cur_stack_frame = (sf as *mut StackFrame).cast();

        // NOTE: switch back to original frame when exit.
        defer!({
            rt.cur_stack_frame = orig_frame;
        });

        let realm_cx = &*realm;

        if args.len() < arg_length as usize {
            // copy arguments to local buffer
            sf.local_buf
                .try_reserve_exact(arg_length as usize)
                .map_err(|_| {
                    realm_cx.throw_out_of_memory();
                    -1
                })?;

            // copy pass in args, cloned
            sf.local_buf.extend(args.iter().cloned());
            // padding with undefined
            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_length as usize - args.len(),
            ));
            debug_assert_eq!(sf.local_buf.len(), arg_length as usize);

            sf.arg_count = arg_length as _;
            sf.arg_real_count = args.len() as _;
            sf.arg_base_ptr = sf.local_buf.as_mut_ptr();
        } else {
            sf.arg_count = args.len() as _;
            sf.arg_real_count = args.len() as _;
            sf.arg_base_ptr = args.as_ptr() as *mut JSValue;
        }

        let ret_val: JSValue = match callback {
            NativeFunctionType::Func(f) => {
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::Constructor(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    realm_cx.throw_type_error("must be called with new");
                    return Err(-1);
                }
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::ConstructorOrFunc(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    this_obj = JSValue::UNDEFINED;
                }
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::Getter(f) => {
                debug_assert_eq!(sf.arg_count, 0);
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::Setter(f) => {
                debug_assert_eq!(sf.arg_count, 1);
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::IteratorNext(f) => {
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                let mut iter_state = IteratorState::NotDone;
                let mut r = f(realm, cc, &mut iter_state);
                if r.is_ok() && iter_state != IteratorState::NotParsed {
                    r = js_create_iterator_result(
                        realm_cx,
                        r.unwrap(),
                        iter_state == IteratorState::Done,
                    );
                }
                r.unwrap_or(JSValue::EXCEPTION)
            }

            //
            // LEGACIES
            //
            NativeFunctionType::CConstructor(f)
            | NativeFunctionType::CConstructorOrFunc(f)
            | NativeFunctionType::CFunc(f) => {
                if matches!(
                    callback,
                    NativeFunctionType::CConstructorOrFunc(_) | NativeFunctionType::CConstructor(_)
                ) && flags & JS_CALL_FLAG_CONSTRUCTOR == 0
                {
                    if unlikely(matches!(callback, NativeFunctionType::Constructor(_))) {
                        realm_cx.throw_type_error("must be called with new");
                        return Err(-1);
                    } else {
                        this_obj = JSValue::UNDEFINED;
                    }
                }
                unsafe { f(realm_cx, this_obj, sf.args_slice()) }
            }
            NativeFunctionType::CConstructorMagic(f)
            | NativeFunctionType::CConstructorOrFuncMagic(f)
            | NativeFunctionType::CFuncMagic(f) => {
                if matches!(
                    callback,
                    NativeFunctionType::CConstructorMagic(_)
                        | NativeFunctionType::CConstructorOrFuncMagic(_)
                ) && flags & JS_CALL_FLAG_CONSTRUCTOR == 0
                {
                    if unlikely(matches!(callback, NativeFunctionType::CConstructorMagic(_))) {
                        realm_cx.throw_type_error("must be called with new");
                        return Err(-1);
                    } else {
                        this_obj = JSValue::UNDEFINED;
                    }
                }
                unsafe { f(realm_cx, this_obj, sf.args_slice(), magic as i32) }
            }
            NativeFunctionType::CGetter(f) => unsafe { f(realm_cx, this_obj) },
            NativeFunctionType::CSetter(f) => unsafe { f(realm_cx, this_obj, sf.args_slice()[0]) },
            NativeFunctionType::CGetterMagic(f) => unsafe { f(realm_cx, this_obj, magic as _) },
            NativeFunctionType::CSetterMagic(f) => unsafe {
                f(
                    realm_cx,
                    this_obj,
                    sf.args_slice().first().cloned().unwrap(),
                    magic as _,
                )
            },
            NativeFunctionType::CIteratorNext(f) => {
                let mut done = 0;
                let mut r = f(realm_cx, this_obj, sf.args_slice(), &mut done, magic as _);
                if !r.is_exception() && done != 2 {
                    r = js_create_iterator_result(realm_cx, r, done != 0)
                        .unwrap_or(JSValue::EXCEPTION);
                }
                r
            }

            NativeFunctionType::FF(f) => sf.args_slice()[0]
                .to_f64(realm_cx)
                .map(|n| JSValue::from(unsafe { f(n) }))
                .unwrap_or(JSValue::EXCEPTION),
            NativeFunctionType::FFF(f) => {
                let args = sf.args_slice();
                if let Ok(d1) = args[0].to_f64(realm_cx)
                    && let Ok(d2) = args[1].to_f64(realm_cx)
                {
                    JSValue::from(unsafe { f(d1, d2) })
                } else {
                    JSValue::EXCEPTION
                }
            }
        };

        // THIS IS DONE in defer!()
        //rt.current_stack_frame = sf.prev_frame;

        Ok(ret_val)
    }
}

// #[repr(transparent)]
// #[derive(Debug, Clone)]
// pub struct NativeFunction(JSValue);

impl ObjectImpl {
    /// Checks if Object is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: NativeFunctionType, magic: i32) -> bool {
        if let ObjectPayload::C_FUNCTION(cfunc) = self.payload() {
            cfunc.magic == magic as i16 && callback.eq(&cfunc.handler)
        } else {
            false
        }
    }
}

impl JSValue {
    /// Checks if a JSValue is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: NativeFunctionType, magic: i32) -> bool {
        if let Self::Object(o) = self {
            o.match_native_callback(callback, magic)
        } else {
            false
        }
    }
}

impl ContextImpl {
    /// Create new function object with native callback and shape of specified prototype.
    /// at least `length` arguments will be readable in arguments.
    pub fn create_native_function_with_proto(
        &self,
        call: NativeFunctionType,
        name: &str,
        length: u8,
        magic: i16,
        proto: Option<ObjectRef>,
    ) -> Result<ObjectRef, i32> {
        let mut func_obj = self
            .create_object_with_proto(
                proto,
                ObjectPayload::C_FUNCTION(NFuncPayload {
                    handler: call,
                    length,
                    magic,
                    realm: unsafe { ContextRef::from_ref_unchecked(self).downgrade() },
                }),
            )
            .map_err(|_| -1)?;

        func_obj.set_flag_constructor(matches!(
            call,
            NativeFunctionType::Constructor(_)
                | NativeFunctionType::ConstructorOrFunc(_)
                | NativeFunctionType::CConstructor(_)
                | NativeFunctionType::CConstructorMagic(_)
                | NativeFunctionType::CConstructorOrFunc(_)
                | NativeFunctionType::CConstructorOrFuncMagic(_)
        ));

        // define properties length and name on function.
        let name_atom = Atom::new(self.rt().as_ref(), name)?;
        js_function_set_properties(self, func_obj, name_atom, length as i32);

        Ok(func_obj)
    }

    /// Create new function object with native callback. using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function(
        &self,
        handler: NativeFunctionType,
        name: &str,
        length: u8,
        magic: i16,
    ) -> Result<ObjectRef, i32> {
        self.create_native_function_with_proto(
            handler,
            name,
            length,
            magic,
            Some(self.function_proto()),
        )
    }
}
