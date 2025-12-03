use std::ptr::NonNull;

use gc_lite::{Collect, GcWeak};

use crate::{
    atom::Atom,
    builtins::iterator::js_create_iterator_result,
    context::{ContextImpl, ContextRef},
    defer,
    error::JSResult,
    function::{CallContext, IteratorState, JS_CALL_FLAG_CONSTRUCTOR},
    object::{ObjectImpl, ObjectPayload, ObjectRef},
    runtime::js_check_stack_overflow,
    stack_frame::StackFrame,
    utils::unlikely,
    value::JSValue,
};

use super::js_function_set_properties;

pub type NativeCallbackFn = fn(cc: CallContext) -> JSResult;
pub type NativeGetterFn = fn(ctx: ContextRef, cc: CallContext) -> JSResult;
pub type NativeSetterFn = fn(ctx: ContextRef, cc: CallContext) -> JSResult;
pub type NativeIteratorNextFn =
    fn(ctx: ContextRef, cc: CallContext, iter_state: &mut IteratorState) -> JSResult;

#[deprecated(note = "should migrate to NativeCallback")]
pub(crate) type NativeCallback_v1 = fn(ctx: ContextRef, cc: CallContext) -> JSResult;

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
pub enum CallbackType {
    Func(NativeCallbackFn),
    Getter(NativeGetterFn),
    Setter(NativeSetterFn),
    Constructor(NativeCallbackFn),
    ConstructorOrFunc(NativeCallbackFn),
    IteratorNext(NativeIteratorNextFn),

    #[deprecated]
    Func_v1(NativeCallback_v1),
    #[deprecated]
    Constructor_v1(NativeCallback_v1),
    #[deprecated]
    ConstructorOrFunc_v1(NativeCallback_v1),

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

impl PartialEq for CallbackType {
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

            (Self::Func_v1(a), Self::Func_v1(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::Constructor_v1(a), Self::Constructor_v1(b)) => std::ptr::fn_addr_eq(*a, *b),
            (Self::ConstructorOrFunc_v1(a), Self::ConstructorOrFunc_v1(b)) => {
                std::ptr::fn_addr_eq(*a, *b)
            }

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
    pub handler: CallbackType,
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
                ContextRef::from(c.realm.upgrade(rt.gc_heap()).unwrap()),
            )
        } else {
            unreachable!()
        };

        // better to always check stack overflow
        if js_check_stack_overflow(&rt, std::mem::size_of::<JSValue>() * arg_length as usize) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // new stack frame
        let mut stack_frame = StackFrame::new(func_obj, 0);
        let sf = &mut stack_frame;

        // set new stack frame to be the 'current-stack-frame',
        // setup to perform pop stack frame on function exit.
        rt.push_stack_frame(NonNull::from_ref(sf));
        defer!({
            rt.pop_stack_frame();
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
            sf.local_buf.extend(args.iter().copied());
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

        let ret_val: JSResult = match callback {
            CallbackType::Func(f) => {
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(cc)
            }
            CallbackType::Constructor(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    realm_cx.throw_type_error("must be called with new");
                    return Err(-1);
                }
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(cc)
            }
            CallbackType::ConstructorOrFunc(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    this_obj = JSValue::UNDEFINED;
                }
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(cc)
            }
            CallbackType::Getter(f) => {
                debug_assert_eq!(sf.arg_count, 0);
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc)
            }
            CallbackType::Setter(f) => {
                debug_assert_eq!(sf.arg_count, 1);
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc)
            }
            CallbackType::IteratorNext(f) => {
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
                r
            }

            CallbackType::Func_v1(f) => {
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc)
            }
            CallbackType::Constructor_v1(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    realm_cx.throw_type_error("must be called with new");
                    return Err(-1);
                }
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc)
            }
            CallbackType::ConstructorOrFunc_v1(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    this_obj = JSValue::UNDEFINED;
                }
                let cc = CallContext::new(realm, this_obj, sf.args_slice(), magic as _, None);
                f(realm, cc)
            }

            //
            // LEGACIES
            //
            CallbackType::CConstructor(f)
            | CallbackType::CConstructorOrFunc(f)
            | CallbackType::CFunc(f) => {
                if matches!(
                    callback,
                    CallbackType::CConstructorOrFunc(_) | CallbackType::CConstructor(_)
                ) && flags & JS_CALL_FLAG_CONSTRUCTOR == 0
                {
                    if unlikely(matches!(callback, CallbackType::Constructor_v1(_))) {
                        realm_cx.throw_type_error("must be called with new");
                        return Err(-1);
                    } else {
                        this_obj = JSValue::UNDEFINED;
                    }
                }
                unsafe { f(realm_cx, this_obj, sf.args_slice()).err() }
            }
            CallbackType::CConstructorMagic(f)
            | CallbackType::CConstructorOrFuncMagic(f)
            | CallbackType::CFuncMagic(f) => {
                if matches!(
                    callback,
                    CallbackType::CConstructorMagic(_) | CallbackType::CConstructorOrFuncMagic(_)
                ) && flags & JS_CALL_FLAG_CONSTRUCTOR == 0
                {
                    if unlikely(matches!(callback, CallbackType::CConstructorMagic(_))) {
                        realm_cx.throw_type_error("must be called with new");
                        return Err(-1);
                    } else {
                        this_obj = JSValue::UNDEFINED;
                    }
                }
                unsafe { f(realm_cx, this_obj, sf.args_slice(), magic as i32).err() }
            }
            CallbackType::CGetter(f) => unsafe { f(realm_cx, this_obj).err() },
            CallbackType::CSetter(f) => unsafe { f(realm_cx, this_obj, sf.args_slice()[0]).err() },
            CallbackType::CGetterMagic(f) => unsafe { f(realm_cx, this_obj, magic as _).err() },
            CallbackType::CSetterMagic(f) => unsafe {
                f(
                    realm_cx,
                    this_obj,
                    sf.args_slice().first().cloned().unwrap(),
                    magic as _,
                )
                .err()
            },
            CallbackType::CIteratorNext(f) => {
                let mut done = 0;
                let mut r = f(realm_cx, this_obj, sf.args_slice(), &mut done, magic as _);
                if !r.is_exception() && done != 2 {
                    r = js_create_iterator_result(realm_cx, r, done != 0)
                        .unwrap_or(JSValue::EXCEPTION);
                }
                r.err()
            }

            CallbackType::FF(f) => sf.args_slice()[0]
                .to_f64(realm_cx)
                .map(|n| JSValue::from(unsafe { f(n) })),
            CallbackType::FFF(f) => {
                let args = sf.args_slice();
                let d1 = args[0].to_f64(realm_cx)?;
                let d2 = args[1].to_f64(realm_cx)?;

                Ok(JSValue::from(unsafe { f(d1, d2) }))
            }
        };

        // THIS IS DONE in defer!()
        // rt.current_stack_frame = sf.prev;

        ret_val
    }
}

// #[repr(transparent)]
// #[derive(Debug, Clone)]
// pub struct NativeFunction(JSValue);

impl ObjectImpl {
    /// Checks if Object is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: CallbackType, magic: i32) -> bool {
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
    pub fn match_native_callback(&self, callback: CallbackType, magic: i32) -> bool {
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
        call: CallbackType,
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
            CallbackType::Constructor(_)
                | CallbackType::ConstructorOrFunc(_)
                | CallbackType::Constructor_v1(_)
                | CallbackType::ConstructorOrFunc_v1(_)
                | CallbackType::CConstructor(_)
                | CallbackType::CConstructorMagic(_)
                | CallbackType::CConstructorOrFunc(_)
                | CallbackType::CConstructorOrFuncMagic(_)
        ));

        // define properties length and name on function.
        let name_atom = Atom::new(&self.rt(), name)?;
        js_function_set_properties(self, func_obj, name_atom, length as i32);

        Ok(func_obj)
    }

    /// Create new function object with native callback. using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function(
        &self,
        handler: CallbackType,
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
