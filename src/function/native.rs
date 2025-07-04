use std::{ops::Deref, ptr::NonNull};

use crate::{
    JS_CLASS_C_FUNCTION,
    atom::JS_FreeAtom,
    context::{JSContext, JSContextWeakref},
    defer,
    error::{JS_ThrowStackOverflow, JSResult},
    function::{CallContext, IteratorState, JS_CALL_FLAG_CONSTRUCTOR},
    iterator::js_create_iterator_result,
    object::JSObjectIsa,
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::JSStackFrame,
    value::{JS_ToFloat64, JSValue, JSValueConst},
};

use super::js_function_set_properties;

pub type NativeCallback = for<'a> fn(ctx: &'a JSContext, cc: CallContext<'a>) -> JSResult;
pub type NativeGetter = for<'a> fn(ctx: &'a JSContext, cc: CallContext<'a>) -> JSResult;
pub type NativeSetter = for<'a> fn(ctx: &'a JSContext, cc: CallContext<'a>) -> JSResult;
pub type NativeIteratorNext =
    fn(ctx: &JSContext, cc: CallContext, state: &mut IteratorState) -> JSResult;

pub type JSCFunction = unsafe fn(ctx: &JSContext, this_val: JSValue, args: &[JSValue]) -> JSValue;
pub type JSCFunctionMagic = unsafe fn(
    ctx: &JSContext,
    this_val: JSValue,
    args: &[JSValue],
    magic: ::core::ffi::c_int,
) -> JSValue;

pub type JSCFunctionGetter = unsafe fn(&JSContext, JSValue) -> JSValue;
pub type JSCFunctionGetterMagic =
    unsafe fn(ctx: &JSContext, this_val: JSValue, magic: i32) -> JSValue;

pub type JSCFunctionSetter = unsafe fn(&JSContext, JSValue, JSValue) -> JSValue;
pub type JSCFunctionSetterMagic = unsafe fn(&JSContext, JSValue, JSValue, i32) -> JSValue;

pub type JSCFunctionData = unsafe fn(
    ctx: &JSContext,
    this_val: JSValue,
    args: &[JSValue],
    magic: i32,
    func_data: &mut [JSValue],
) -> JSValue;

pub type JSCFunctionIteratorNext =
    fn(ctx: &JSContext, this: JSValue, args: &[JSValue], done: &mut i32, magic: i32) -> JSValue;

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
            (Self::Func(a), Self::Func(b)) => a == b,
            (Self::Getter(a), Self::Getter(b)) => a == b,
            (Self::Setter(a), Self::Setter(b)) => a == b,
            (Self::Constructor(a), Self::Constructor(b)) => a == b,
            (Self::ConstructorOrFunc(a), Self::ConstructorOrFunc(b)) => a == b,
            (Self::IteratorNext(a), Self::IteratorNext(b)) => a == b,
            (Self::CFunc(a), Self::CFunc(b)) => a == b,
            (Self::CFuncMagic(a), Self::CFuncMagic(b)) => a == b,
            (Self::CConstructor(a), Self::CConstructor(b)) => a == b,
            (Self::CConstructorOrFunc(a), Self::CConstructorOrFunc(b)) => a == b,
            (Self::CConstructorMagic(a), Self::CConstructorMagic(b)) => a == b,
            (Self::CConstructorOrFuncMagic(a), Self::CConstructorOrFuncMagic(b)) => a == b,
            (Self::CGetter(a), Self::CGetter(b)) => a == b,
            (Self::CGetterMagic(a), Self::CGetterMagic(b)) => a == b,
            (Self::CSetter(a), Self::CSetter(b)) => a == b,
            (Self::CSetterMagic(a), Self::CSetterMagic(b)) => a == b,
            (Self::CIteratorNext(a), Self::CIteratorNext(b)) => a == b,
            (Self::FF(a), Self::FF(b)) => a == b,
            (Self::FFF(a), Self::FFF(b)) => a == b,
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct JSObjectCFunc {
    pub c_function: NativeFunctionType,
    pub realm: JSContextWeakref,
    pub length: u8,
    pub magic: i16,
}

impl JSObjectCFunc {
    pub fn hook_call(
        ctx: &JSContext,
        func_obj: &JSValue,
        mut this_obj: &JSValue,
        args: &[JSValue],
        flags: i32,
    ) -> JSResult {
        let rt = ctx.rt_mut();

        let (callback, arg_length, magic, realm) = if let JSObjectIsa::C_FUNCTION(c) =
            unsafe { func_obj.object_unchecked() }
                .0
                .isa
                .borrow()
                .deref()
        {
            (c.c_function, c.length, c.magic, c.realm.upgrade())
        } else {
            unreachable!()
        };

        let realm = realm.as_ref().unwrap_or(ctx); // change the current realm

        // better to always check stack overflow
        if js_check_stack_overflow(rt, std::mem::size_of::<JSValue>() * arg_length as usize) {
            JS_ThrowStackOverflow(ctx);
            return Err(-1);
        }

        // build new stack frame, alloc on stack.
        let mut new_frame = JSStackFrame {
            prev_frame: rt.current_stack_frame,
            cur_func: func_obj.clone(),
            ..Default::default()
        };
        let sf = &mut new_frame;

        // switch current stack frame to new.
        // switch back to original when exit
        let orig_frame = rt.current_stack_frame;
        rt.current_stack_frame = (sf as *mut JSStackFrame).cast();

        // NOTE: switch back to original frame when exit.
        let mut rt_ptr = unsafe { NonNull::new_unchecked(rt) };
        defer!(unsafe {
            rt_ptr.as_mut().current_stack_frame = orig_frame;
        });

        if args.len() < arg_length as usize {
            // copy arguments to local buffer
            sf.local_buf
                .try_reserve_exact(arg_length as usize)
                .map_err(|_| {
                    ctx.JS_ThrowOutOfMemory();
                    -1
                })?;

            // copy pass in args, cloned
            sf.local_buf.extend(args.iter().map(|v| v.clone()));
            // padding with undefined
            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_length as usize - args.len(),
            ));
            debug_assert_eq!(sf.local_buf.len(), arg_length as usize);

            sf.arg_count = arg_length as _;
            sf.arg_base_ptr = sf.local_buf.as_mut_ptr();
        } else {
            sf.arg_count = args.len() as _;
            sf.arg_base_ptr = args.as_ptr() as *mut JSValue;
        }

        let ret_val: JSValue = match callback {
            NativeFunctionType::Func(f) => {
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::Constructor(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    realm.JS_ThrowTypeError("must be called with new");
                    return Err(-1);
                }
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::ConstructorOrFunc(f) => {
                if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                    this_obj = &JSValue::UNDEFINED;
                }
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::Getter(f) => {
                debug_assert_eq!(sf.arg_count, 0);
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::Setter(f) => {
                debug_assert_eq!(sf.arg_count, 1);
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                f(realm, cc).unwrap_or(JSValue::EXCEPTION)
            }
            NativeFunctionType::IteratorNext(f) => {
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                let mut state = IteratorState::HasMore;
                let mut r = f(realm, cc, &mut state);
                if !r.is_err() && state != IteratorState::Uninitialized {
                    r = js_create_iterator_result(realm, r.unwrap(), state == IteratorState::Done);
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
                ) {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        if matches!(callback, NativeFunctionType::Constructor(_)) {
                            realm.JS_ThrowTypeError("must be called with new");
                            return Err(-1);
                        } else {
                            this_obj = &JSValue::UNDEFINED;
                        }
                    }
                }
                unsafe { f(realm, this_obj.clone(), sf.args_slice()) }
            }
            NativeFunctionType::CConstructorMagic(f)
            | NativeFunctionType::CConstructorOrFuncMagic(f)
            | NativeFunctionType::CFuncMagic(f) => {
                if matches!(
                    callback,
                    NativeFunctionType::CConstructorMagic(_)
                        | NativeFunctionType::CConstructorOrFuncMagic(_)
                ) {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        if matches!(callback, NativeFunctionType::CConstructorMagic(_)) {
                            realm.JS_ThrowTypeError("must be called with new");
                            return Err(-1);
                        } else {
                            this_obj = &JSValue::UNDEFINED;
                        }
                    }
                }
                unsafe { f(realm, this_obj.clone(), sf.args_slice(), magic as i32) }
            }
            NativeFunctionType::CGetter(f) => unsafe { f(realm, this_obj.clone()) },
            NativeFunctionType::CSetter(f) => unsafe {
                f(realm, this_obj.clone(), sf.args_slice()[0].clone())
            },
            NativeFunctionType::CGetterMagic(f) => unsafe {
                f(realm, this_obj.clone(), magic as _)
            },
            NativeFunctionType::CSetterMagic(f) => unsafe {
                f(
                    realm,
                    this_obj.clone(),
                    sf.args_slice().first().cloned().unwrap(),
                    magic as _,
                )
            },
            NativeFunctionType::CIteratorNext(f) => unsafe {
                let mut done = 0;
                let mut r = f(
                    realm,
                    this_obj.clone(),
                    sf.args_slice(),
                    &mut done,
                    magic as _,
                );
                if !r.is_exception() && done != 2 {
                    r = js_create_iterator_result(realm, r, done != 0)
                        .unwrap_or(JSValue::EXCEPTION);
                }
                r
            },

            NativeFunctionType::FF(f) => unsafe {
                let mut d1: f64 = 0.0;
                if JS_ToFloat64(realm, &mut d1, &sf.args_slice()[0]) != 0 {
                    JSValue::EXCEPTION
                } else {
                    JSValue::from(f(d1))
                }
            },
            NativeFunctionType::FFF(f) => unsafe {
                let mut d1: f64 = 0.0;
                let mut d2: f64 = 0.0;
                let args = sf.args_slice();
                if JS_ToFloat64(realm, &mut d1, &args[0]) != 0
                    || JS_ToFloat64(realm, &mut d2, &args[1]) != 0
                {
                    JSValue::EXCEPTION
                } else {
                    JSValue::from(f(d1, d2))
                }
            },
        };

        // THIS IS DONE in defer!()
        //rt.current_stack_frame = sf.prev_frame;

        Ok(ret_val)
    }

    pub fn hook_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {
        debug_assert!(matches!(isa, JSObjectIsa::C_FUNCTION(_)));
    }

    pub fn hook_mark(val: JSValue) {
        unsafe {
            if let JSObjectIsa::C_FUNCTION(cfunc) = val.object_unchecked().0.isa.borrow().deref() {
                // println!("[O.o] TODO cfunction mark");
                // mark_func(rt, &cfunc.realm);
            }
        }
    }
}

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct NativeFunctionValue(JSValue);

impl JSValue {
    #[inline(always)]
    pub unsafe fn as_native_function_unchecked(self) -> NativeFunctionValue {
        debug_assert!(self.is_object());
        debug_assert_eq!(self.object_unchecked().class_id(), JS_CLASS_C_FUNCTION);
        std::mem::transmute(self)
    }

    /// Checks if a JSValue is native function of specific type.
    /// Matches both the callback pointer and magic.
    pub fn match_native_callback(&self, callback: NativeFunctionType, magic: i32) -> bool {
        self.object().is_some_and(|o| {
            if let JSObjectIsa::C_FUNCTION(cfunc) = o.0.isa.borrow().deref() {
                cfunc.magic == magic as i16 && callback.eq(&cfunc.c_function)
            } else {
                false
            }
        })
    }
}

impl JSContext {
    /// Create new function object with native callback and shape of speficied prototype.
    /// at least `length` arguments will be readable in arguments.
    pub fn create_native_function_from_proto(
        &self,
        handler: NativeFunctionType,
        name: &str,
        length: u8,
        magic: i16,
        proto_val: &JSValue,
    ) -> JSResult {
        let func_obj = self
            .create_instance_from_proto(
                proto_val,
                JSObjectIsa::C_FUNCTION(JSObjectCFunc {
                    c_function: handler,
                    length,
                    magic,
                    realm: self.downgrade(),
                }),
            )
            .map_err(|_| -1)?;

        let ob = unsafe { func_obj.object_unchecked() };

        unsafe {
            (*ob.0.detailed.as_ptr()).set_constructor(matches!(
                handler,
                NativeFunctionType::Constructor(_)
                    | NativeFunctionType::ConstructorOrFunc(_)
                    | NativeFunctionType::CConstructor(_)
                    | NativeFunctionType::CConstructorMagic(_)
                    | NativeFunctionType::CConstructorOrFunc(_)
                    | NativeFunctionType::CConstructorOrFuncMagic(_)
            ));
        }

        // define properties length and name on function.
        let name_atom = self.create_atom(name).unwrap();
        unsafe {
            js_function_set_properties(self, &func_obj, name_atom, length as i32);
        }
        JS_FreeAtom(self, name_atom);

        Ok(func_obj)
    }

    /// Create new function object with native callback. using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline]
    pub fn create_native_function(
        &self,
        handler: NativeFunctionType,
        name: &str,
        length: u8,
        magic: i16,
    ) -> JSResult {
        self.create_native_function_from_proto(
            handler,
            name,
            length,
            magic,
            &self.0.borrow().function_proto,
        )
    }
}

#[deprecated]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum JSCFunctionEnum {
    Generic = 0,
    GenericMagic,
    Constructor,
    ConstructorMagic,
    ConstructorOrFunc,
    ConstructorOrFuncMagic,
    FF,
    FFF,
    Getter,
    Setter,
    GetterMagic,
    SetterMagic,
    IteratorNext,

    NativeFunc,
    NativeConstructor,
    NativeConstructorOrFunc,
    NativeSetter,
    NativeGetter,
}

// Note: at least 'length' arguments will be readable in 'argv'
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
pub unsafe fn JS_NewCFunction3(
    ctx: &JSContext,
    func: JSCFunction,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
    proto_val: JSValueConst,
) -> JSValue {
    match ctx.create_instance_from_proto(
        &proto_val,
        JSObjectIsa::C_FUNCTION(JSObjectCFunc {
            realm: ctx.downgrade(),
            c_function: NativeFunctionType::CFunc(func),
            length,
            magic,
        }),
    ) {
        Ok(func_obj) => {
            unsafe {
                (*func_obj.object_unchecked().0.detailed.as_ptr()).set_constructor(matches!(
                    cproto,
                    JSCFunctionEnum::Constructor
                        | JSCFunctionEnum::ConstructorMagic
                        | JSCFunctionEnum::ConstructorOrFunc
                        | JSCFunctionEnum::ConstructorOrFuncMagic
                ));
            }

            // define propperties on func_obj: length and name.
            let name_atom = ctx.create_atom(name).unwrap();

            js_function_set_properties(ctx, &func_obj, name_atom, length as i32);
            JS_FreeAtom(ctx, name_atom);

            func_obj
        }
        Err(_) => JSValue::EXCEPTION,
    }
}

/* Note: at least 'length' arguments will be readable in 'argv' */
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
#[inline(always)]
pub fn JS_NewCFunction2(
    ctx: &JSContext,
    func: JSCFunction,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    unsafe {
        JS_NewCFunction3(
            ctx,
            func,
            name,
            length,
            cproto,
            magic,
            ctx.0.borrow().function_proto.clone(),
        )
    }
}

#[inline(always)]
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
pub fn JS_NewCFunction(ctx: &JSContext, func: JSCFunction, name: &str, length: u8) -> JSValue {
    JS_NewCFunction2(ctx, func, name, length, JSCFunctionEnum::Generic, 0)
}

#[inline(always)]
#[deprecated(note = "use native callback instead. see JSContext::create_native_function()")]
pub fn JS_NewCFunctionMagic(
    ctx: &JSContext,
    func: JSCFunctionMagic,
    name: &str,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    unsafe { JS_NewCFunction2(ctx, std::mem::transmute(func), name, length, cproto, magic) }
}
