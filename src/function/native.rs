use std::{ops::Deref, ptr::NonNull};

use crate::{
    JS_CLASS_C_FUNCTION,
    atom::JS_FreeAtom,
    context::{JSContext, JSContextWeakref},
    defer,
    error::{JS_ThrowStackOverflow, JSResult},
    function::JS_CALL_FLAG_CONSTRUCTOR,
    iterator::js_create_iterator_result,
    object::JSObjectIsa,
    runtime::{JSRuntime, js_check_stack_overflow},
    stack_frame::JSStackFrame,
    value::{JS_ToFloat64, JSValue, JSValueConst},
};

use super::js_function_set_properties;

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
    NativeIteratorNext,
}

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

pub type JSNativeCallback = for<'a> fn(ctx: &'a JSContext, cc: CallContext<'a>) -> JSResult;
pub type JSNativeGetter = for<'a> fn(ctx: &'a JSContext, cc: CallContext<'a>) -> JSResult;
pub type JSNativeSetter = for<'a> fn(ctx: &'a JSContext, cc: CallContext<'a>) -> JSResult;
pub type JSNativeIteratorNext =
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

#[derive(Clone, Copy)]
pub union JSCFunctionType {
    pub native_callback: JSNativeCallback,
    pub native_getter: JSNativeGetter,
    pub native_setter: JSNativeSetter,
    pub native_iterator_next: JSNativeIteratorNext,

    //
    pub generic: JSCFunction,
    pub generic_magic: JSCFunctionMagic,
    pub constructor: JSCFunction,
    pub constructor_magic: JSCFunctionMagic,
    pub constructor_or_func: JSCFunction,
    pub f_f: JSCFunctionFF,
    pub f_f_f: JSCFunctionFFF,
    pub getter: JSCFunctionGetter,
    pub setter: JSCFunctionSetter,
    pub getter_magic: JSCFunctionGetterMagic,
    pub setter_magic: JSCFunctionSetterMagic,
    pub iterator_next: JSCFunctionIteratorNext,
}

impl std::fmt::Debug for JSCFunctionType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "<JSCFunctionType>")
    }
}

#[derive(Debug)]
pub struct JSObjectCFunc {
    pub realm: JSContextWeakref,
    pub c_function: JSCFunctionType,
    pub length: u8,
    pub cproto: JSCFunctionEnum,
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

        let (callback, cproto, arg_length, magic, realm) = if let JSObjectIsa::C_FUNCTION(c) =
            unsafe { func_obj.object_unchecked() }
                .0
                .isa
                .borrow()
                .deref()
        {
            (c.c_function, c.cproto, c.length, c.magic, c.realm.upgrade())
        } else {
            unreachable!()
        };

        // let cfunc = if let JSObjectIsa::C_FUNCTION(c) =
        //     unsafe { func_obj.object_unchecked() }.isa().deref()
        // {
        //     c
        // } else {
        //     unreachable!()
        // };
        // let cproto = cfunc.cproto;
        // let arg_length = cfunc.length;
        // let func = &cfunc.c_function;
        // let realm = cfunc.realm.upgrade();

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

        macro_rules! fail {
            () => {
                // this is done in defer!()
                // rt.current_stack_frame = sf.prev_frame;
                return Err(-1);
            };
        }

        let ret_val: JSValue = match cproto {
            JSCFunctionEnum::NativeFunc
            | JSCFunctionEnum::NativeConstructor
            | JSCFunctionEnum::NativeConstructorOrFunc => {
                if matches!(
                    cproto,
                    JSCFunctionEnum::NativeConstructor | JSCFunctionEnum::NativeConstructorOrFunc
                ) {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        if cproto == JSCFunctionEnum::NativeConstructor {
                            realm.JS_ThrowTypeError("must be called with new");
                            fail!();
                        } else {
                            this_obj = &JSValue::UNDEFINED;
                        }
                    }
                }

                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                unsafe { (callback.native_callback)(realm, cc).unwrap_or(JSValue::EXCEPTION) }
            }
            JSCFunctionEnum::NativeGetter => {
                debug_assert_eq!(sf.arg_count, 0);
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                unsafe { (callback.native_getter)(realm, cc).unwrap_or(JSValue::EXCEPTION) }
            }
            JSCFunctionEnum::NativeSetter => {
                debug_assert_eq!(sf.arg_count, 1);
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                unsafe { (callback.native_setter)(realm, cc).unwrap_or(JSValue::EXCEPTION) }
            }
            JSCFunctionEnum::NativeIteratorNext => {
                let cc =
                    CallContext::new(realm, this_obj.clone(), sf.args_slice(), magic as _, None);
                let mut state = IteratorState::HasMore;
                let mut r = unsafe { (callback.native_iterator_next)(realm, cc, &mut state) };
                if !r.is_err() && state != IteratorState::Uninitialized {
                    r = js_create_iterator_result(realm, r.unwrap(), state == IteratorState::Done);
                }
                r.unwrap_or(JSValue::EXCEPTION)
            }

            //
            // LEGACIES
            //
            JSCFunctionEnum::Constructor
            | JSCFunctionEnum::ConstructorOrFunc
            | JSCFunctionEnum::Generic => {
                if matches!(
                    cproto,
                    JSCFunctionEnum::ConstructorOrFunc | JSCFunctionEnum::Constructor
                ) {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        if cproto == JSCFunctionEnum::Constructor {
                            realm.JS_ThrowTypeError("must be called with new");
                            fail!();
                        } else {
                            this_obj = &JSValue::UNDEFINED;
                        }
                    }
                }
                unsafe { (callback.generic)(realm, this_obj.clone(), sf.args_slice()) }
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
                            realm.JS_ThrowTypeError("must be called with new");
                            fail!();
                        } else {
                            this_obj = &JSValue::UNDEFINED;
                        }
                    }
                }
                unsafe {
                    (callback.generic_magic)(realm, this_obj.clone(), sf.args_slice(), magic as i32)
                }
            }
            JSCFunctionEnum::Getter => unsafe { (callback.getter)(realm, this_obj.clone()) },
            JSCFunctionEnum::Setter => unsafe {
                (callback.setter)(realm, this_obj.clone(), sf.args_slice()[0].clone())
            },
            JSCFunctionEnum::GetterMagic => unsafe {
                (callback.getter_magic)(realm, this_obj.clone(), magic as _)
            },
            JSCFunctionEnum::SetterMagic => unsafe {
                (callback.setter_magic)(
                    realm,
                    this_obj.clone(),
                    sf.args_slice().first().cloned().unwrap(),
                    magic as _,
                )
            },
            JSCFunctionEnum::FF => unsafe {
                let mut d1: f64 = 0.0;
                if JS_ToFloat64(realm, &mut d1, &sf.args_slice()[0]) != 0 {
                    JSValue::EXCEPTION
                } else {
                    JSValue::from((callback.f_f)(d1))
                }
            },
            JSCFunctionEnum::FFF => unsafe {
                let mut d1: f64 = 0.0;
                let mut d2: f64 = 0.0;
                let args = sf.args_slice();
                if JS_ToFloat64(realm, &mut d1, &args[0]) != 0
                    || JS_ToFloat64(realm, &mut d2, &args[1]) != 0
                {
                    JSValue::EXCEPTION
                } else {
                    JSValue::from((callback.f_f_f)(d1, d2))
                }
            },
            JSCFunctionEnum::IteratorNext => unsafe {
                let mut done = 0;
                let mut r = (callback.iterator_next)(
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
    pub fn match_native_callback(&self, callback: fn(), magic: i32) -> bool {
        self.object().is_some_and(|o| {
            if let JSObjectIsa::C_FUNCTION(cfunc) = o.0.isa.borrow().deref() {
                unsafe {
                    // let ptr1 = std::mem::transmute::<_, *const c_void>(cfunc.c_function.generic);
                    // let ptr2 = std::mem::transmute::<_, *const c_void>(callback);
                    // ptr1 == ptr2 && cfunc.magic == magic as i16

                    std::ptr::fn_addr_eq(cfunc.c_function.generic, callback)
                        && cfunc.magic == magic as i16
                }
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
        callback: JSNativeCallback,
        name: &str,
        length: u8,
        cproto: JSCFunctionEnum,
        magic: i16,
        proto_val: &JSValue,
    ) -> JSResult {
        debug_assert!(matches!(
            cproto,
            JSCFunctionEnum::NativeFunc
                | JSCFunctionEnum::NativeConstructor
                | JSCFunctionEnum::NativeConstructorOrFunc
        ));

        let func_obj = self
            .create_instance_from_proto(
                proto_val,
                JSObjectIsa::C_FUNCTION(JSObjectCFunc {
                    realm: self.downgrade(),
                    c_function: JSCFunctionType {
                        native_callback: callback,
                    },
                    length,
                    cproto,
                    magic,
                }),
            )
            .map_err(|_| -1)?;

        let ob = unsafe { func_obj.object_unchecked() };

        unsafe {
            (*ob.0.detailed.as_ptr()).set_constructor(matches!(
                cproto,
                JSCFunctionEnum::NativeConstructor | JSCFunctionEnum::NativeConstructorOrFunc
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
    #[inline(always)]
    pub fn create_native_function(
        &self,
        callback: JSNativeCallback,
        name: &str,
        length: u8,
        cproto: JSCFunctionEnum,
        magic: i16,
    ) -> JSResult {
        self.create_native_function_from_proto(
            callback,
            name,
            length,
            cproto,
            magic,
            &self.0.borrow().function_proto,
        )
    }
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
            c_function: JSCFunctionType { generic: func },
            length,
            cproto,
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
    unsafe { JS_NewCFunction2(ctx, func, name, length, JSCFunctionEnum::Generic, 0) }
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
