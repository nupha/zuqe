use std::{
    ffi::{c_char, c_void, CStr},
    ptr::NonNull,
};

use crate::{
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_NewAtomString},
    context::{JSContext, JS_DupContext, JS_FreeContext},
    error::{JSResult, JS_ThrowStackOverflow},
    function::JS_CALL_FLAG_CONSTRUCTOR,
    gc::JS_MarkFunc,
    iterator::js_create_iterator_result,
    runtime::{js_check_stack_overflow, JSRuntime},
    stack_frame::JSStackFrame,
    types::list_head,
    value::{
        object::{
            JSAutoInitIDEnum, JSObject, JS_DefineAutoInitProperty, JS_DefinePropertyGetSet,
            JS_NewObject, JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE,
        },
        JSValue, JSValueConst, JSValueRc, JS_ToFloat64, JS_DEF_ALIAS, JS_DEF_CFUNC, JS_DEF_CGETSET,
        JS_DEF_CGETSET_MAGIC, JS_DEF_OBJECT, JS_DEF_PROP_DOUBLE, JS_DEF_PROP_INT32,
        JS_DEF_PROP_INT64, JS_DEF_PROP_STRING, JS_DEF_PROP_UNDEFINED, JS_TAG_OBJECT,
        JS_VALUE_GET_TAG,
    },
    JS_CLASS_ARRAY, JS_CLASS_C_FUNCTION,
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
}

pub struct CallArgs<'a> {
    ctx: NonNull<JSContext>,
    pub this: JSValue,
    pub args: &'a [JSValue],
    pub magic: i32,

    /// extra values
    pub data: Option<&'a mut [JSValue]>,
}

impl<'a> CallArgs<'a> {
    pub(crate) fn new(
        ctx: NonNull<JSContext>,
        this: JSValue,
        args: &'a [JSValue],
        magic: i32,
        data: Option<&'a mut [JSValue]>,
    ) -> Self {
        Self {
            ctx,
            this,
            args,
            magic,
            data,
        }
    }

    /// get arguments count
    #[inline(always)]
    pub fn length(&self) -> usize {
        self.args.len()
    }

    /// get nth argument raw value
    #[inline(always)]
    pub fn get_raw(&self, nth: usize) -> JSValue {
        self.args.get(nth).copied().unwrap_or(JSValue::UNDEFINED)
    }

    /// get argument raw values slice
    #[inline(always)]
    pub fn arguments_raw(&self) -> &[JSValue] {
        self.args
    }

    /// get nth argument, dup and wrap into value rc
    #[inline(always)]
    pub fn get(&self, nth: usize) -> JSValueRc {
        if let Some(v) = self.args.get(nth) {
            v.dup().into_rc(unsafe { self.ctx.as_ref().rt() })
        } else {
            JSValue::UNDEFINED.into_rc(unsafe { self.ctx.as_ref().rt() })
        }
    }

    /// get nth extra data value.
    /// if you are referencing the value, must dup it by yourself.
    #[inline(always)]
    pub fn data_raw(&self, nth: usize) -> JSValue {
        self.data
            .as_deref()
            .and_then(|d| d.get(nth))
            .copied()
            .unwrap_or(JSValue::UNDEFINED)
    }

    /// set nth data value. returns old value, or Err if nth is out of range.
    /// Note: `val` is consumed.
    pub fn replace_data(&mut self, nth: usize, val: JSValue) -> Result<JSValue, i32> {
        if let Some(lst) = self.data.as_deref_mut() {
            if nth < lst.len() {
                return Ok(unsafe { lst[nth].replace(val) });
            }
        }

        Err(-1)
    }

    #[inline(always)]
    pub fn data_slice(&self) -> Option<&[JSValue]> {
        self.data.as_deref()
    }
}

pub type JSNativeCallback = for<'a> fn(&'a mut JSContext, CallArgs<'a>) -> JSResult;
pub type JSNativeGetter = for<'a> fn(&'a mut JSContext, CallArgs<'a>) -> JSResult;
pub type JSNativeSetter = for<'a> fn(&'a mut JSContext, CallArgs<'a>) -> JSResult;

/* return the value associated to the autoinit property or an exception */
pub type JSAutoInitFunc =
    unsafe fn(ctx: *mut JSContext, p: *mut JSObject, atom: JSAtom, opaque: *mut c_void) -> JSValue;

pub type JSCFunction = unsafe fn(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: ::core::ffi::c_int,
    argv: *mut JSValue,
) -> JSValue;
pub type JSCFunctionMagic = unsafe fn(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: ::core::ffi::c_int,
    argv: *mut JSValue,
    magic: ::core::ffi::c_int,
) -> JSValue;

pub type JSCFunctionGetter = unsafe fn(*mut JSContext, JSValueConst) -> JSValue;
pub type JSCFunctionGetterMagic =
    unsafe fn(ctx: *mut JSContext, this_val: JSValue, magic: i32) -> JSValue;

pub type JSCFunctionSetter = unsafe fn(*mut JSContext, JSValueConst, JSValueConst) -> JSValue;
pub type JSCFunctionSetterMagic =
    unsafe fn(*mut JSContext, JSValueConst, JSValueConst, i32) -> JSValue;

pub type JSCFunctionData = unsafe fn(
    ctx: *mut JSContext,
    this_val: JSValue,
    argc: i32,
    argv: *mut JSValue,
    magic: i32,
    func_data: *mut JSValue,
) -> JSValue;

pub type JSCFunctionIteratorNext =
    unsafe fn(*mut JSContext, JSValueConst, i32, *const JSValueConst, *mut i32, i32) -> JSValue;

pub type JSCFunctionFF = unsafe fn(f64) -> f64;
pub type JSCFunctionFFF = unsafe fn(f64, f64) -> f64;

#[derive(Clone, Copy)]
pub union JSCFunctionType {
    pub native_callback: JSNativeCallback,
    pub native_getter: JSNativeGetter,
    pub native_setter: JSNativeSetter,

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

#[repr(C)]
pub struct JSCFunctionListEntry {
    pub name: &'static CStr,
    pub prop_flags: u8,
    pub def_type: u8,
    pub magic: i16,
    pub u: JSCFunctionListEntryUnion,
}

#[repr(C)]
pub union JSCFunctionListEntryUnion {
    pub func: JSCFunctionListEntryFunc,
    pub getset: JSCFunctionListEntryGetSet,
    pub alias: JSCFunctionListEntryAlias,
    pub prop_list: JSCFunctionListEntryPropList,
    pub str: &'static CStr,
    pub i32: i32,
    pub i64: i64,
    pub f64: f64,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct JSCFunctionListEntryFunc {
    pub length: u8,
    pub cproto: JSCFunctionEnum,
    pub cfunc: JSCFunctionType,
}

#[derive(Clone, Copy)]
#[repr(C)]
pub struct JSCFunctionListEntryGetSet {
    pub get: Option<JSCFunctionType>,
    pub set: Option<JSCFunctionType>,
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct JSCFunctionListEntryAlias {
    pub name: &'static CStr,
    pub base: i32,
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct JSCFunctionListEntryPropList {
    pub tab: *const JSCFunctionListEntry,
    pub len: i32,
}

#[derive(Clone, Copy)]
pub struct JSObjectCFunc {
    pub realm: *mut JSContext,
    pub c_function: JSCFunctionType,
    pub length: u8,
    pub cproto: JSCFunctionEnum,
    pub magic: i16,
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct NativeFunctionValue(JSValue);

impl JSValue {
    #[inline(always)]
    pub unsafe fn as_native_function_unchecked(self) -> NativeFunctionValue {
        debug_assert_eq!(JS_VALUE_GET_TAG(self), JS_TAG_OBJECT);
        debug_assert_eq!(self.object().class_id(), JS_CLASS_C_FUNCTION);
        std::mem::transmute(self)
    }
}

impl NativeFunctionValue {
    pub unsafe fn hook_call(
        ctx: &mut JSContext,
        func_obj: JSValueConst,
        mut this_obj: JSValueConst,
        argc: i32,
        argv: *const JSValueConst,
        flags: i32,
    ) -> JSValue {
        let rt = ctx.rt.as_mut();

        let p = func_obj.object();
        let cproto = p.u.cfunc.cproto;
        let arg_length = p.u.cfunc.length;

        // better to always check stack overflow
        if js_check_stack_overflow(
            rt,
            std::mem::size_of::<JSValueConst>() * arg_length as usize,
        ) {
            return JS_ThrowStackOverflow(ctx);
        }

        let mut sf_s: JSStackFrame = JSStackFrame {
            prev_frame: rt.current_stack_frame,
            cur_func: func_obj,
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
        rt.current_stack_frame = sf;

        let realm = p.u.cfunc.realm.as_mut().unwrap(); // change the current realm

        if argc < arg_length as i32 {
            // copy arguments to local buffer
            if sf.local_buf.try_reserve_exact(arg_length as usize).is_err() {
                ctx.JS_ThrowOutOfMemory();
                return JSValue::EXCEPTION;
            };

            // copy argv values
            for i in 0..argc {
                sf.local_buf.push(*argv.offset(i as isize)); // no value dup
            }
            // padding with undefined
            sf.local_buf.extend(std::iter::repeat_n(
                JSValue::UNDEFINED,
                arg_length as usize - argc as usize,
            ));
            debug_assert_eq!(sf.local_buf.len(), arg_length as usize);

            sf.arg_count = arg_length as _;
            sf.arg_base_ptr = sf.local_buf.as_mut_ptr();
        } else {
            sf.arg_count = argc as _;
            sf.arg_base_ptr = argv as *mut JSValue;
        }

        macro_rules! exit {
            ($retval:expr) => {
                rt.current_stack_frame = sf.prev_frame;
                return $retval;
            };
        }

        let arg_buf = sf.arg_base_ptr;
        let func = &p.u.cfunc.c_function;
        let mut ret_val: JSValue;

        match cproto {
            JSCFunctionEnum::NativeFunc
            | JSCFunctionEnum::NativeConstructor
            | JSCFunctionEnum::NativeConstructorOrFunc => {
                if matches!(
                    cproto,
                    JSCFunctionEnum::NativeConstructor | JSCFunctionEnum::NativeConstructorOrFunc
                ) {
                    if flags & JS_CALL_FLAG_CONSTRUCTOR == 0 {
                        if cproto == JSCFunctionEnum::NativeConstructor {
                            exit!(realm.JS_ThrowTypeError(c"must be called with new"));
                        } else {
                            this_obj = JSValue::UNDEFINED;
                        }
                    }
                }

                let cc = CallArgs::new(
                    NonNull::new_unchecked(p.u.cfunc.realm),
                    this_obj,
                    sf.args_slice(),
                    p.u.cfunc.magic as _,
                    None,
                );
                ret_val = (func.native_callback)(realm, cc).unwrap_or(JSValue::EXCEPTION);
            }
            JSCFunctionEnum::NativeGetter => {
                debug_assert_eq!(sf.arg_count, 0);
                let cc = CallArgs::new(
                    NonNull::new_unchecked(realm),
                    this_obj,
                    sf.args_slice(),
                    p.u.cfunc.magic as _,
                    None,
                );
                ret_val = (func.native_getter)(realm, cc).unwrap_or(JSValue::EXCEPTION);
            }
            JSCFunctionEnum::NativeSetter => {
                debug_assert_eq!(sf.arg_count, 1);
                let cc = CallArgs::new(
                    NonNull::new_unchecked(realm),
                    this_obj,
                    sf.args_slice(),
                    p.u.cfunc.magic as _,
                    None,
                );
                ret_val = (func.native_setter)(realm, cc).unwrap_or(JSValue::EXCEPTION);
            }

            //
            //
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
                            exit!(realm.JS_ThrowTypeError(c"must be called with new"));
                        } else {
                            this_obj = JSValue::UNDEFINED;
                        }
                    }
                }
                ret_val = (func.generic)(realm, this_obj, sf.arg_count as _, arg_buf);
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
                            exit!(realm.JS_ThrowTypeError(c"must be called with new"));
                        } else {
                            this_obj = JSValue::UNDEFINED;
                        }
                    }
                }
                ret_val = (func.generic_magic)(
                    realm,
                    this_obj,
                    sf.arg_count as _,
                    arg_buf,
                    p.u.cfunc.magic as i32,
                );
            }
            JSCFunctionEnum::Getter => {
                ret_val = (func.getter)(realm, this_obj);
            }
            JSCFunctionEnum::Setter => {
                ret_val = (func.setter)(realm, this_obj, *arg_buf);
            }
            JSCFunctionEnum::GetterMagic => {
                ret_val = (func.getter_magic)(realm, this_obj, p.u.cfunc.magic as _);
            }
            JSCFunctionEnum::SetterMagic => {
                ret_val = (func.setter_magic)(realm, this_obj, *arg_buf, p.u.cfunc.magic as _);
            }
            JSCFunctionEnum::FF => {
                let mut d1: f64 = 0.0;
                ret_val = if JS_ToFloat64(realm, &mut d1, *arg_buf) != 0 {
                    JSValue::EXCEPTION
                } else {
                    JSValue::from_f64((func.f_f)(d1))
                };
            }
            JSCFunctionEnum::FFF => {
                let mut d1: f64 = 0.0;
                let mut d2: f64 = 0.0;
                ret_val = if JS_ToFloat64(realm, &mut d1, *arg_buf) != 0
                    || JS_ToFloat64(realm, &mut d2, *arg_buf.offset(1)) != 0
                {
                    JSValue::EXCEPTION
                } else {
                    JSValue::from_f64((func.f_f_f)(d1, d2))
                };
            }
            JSCFunctionEnum::IteratorNext => {
                let mut done = 0;
                ret_val = (func.iterator_next)(
                    realm,
                    this_obj,
                    sf.arg_count as _,
                    arg_buf,
                    &mut done,
                    (*p).u.cfunc.magic as _,
                );
                if !ret_val.is_exception() && done != 2 {
                    ret_val = js_create_iterator_result(realm, ret_val, done != 0);
                }
            }
        }

        rt.current_stack_frame = sf.prev_frame;
        ret_val
    }

    pub unsafe fn hook_finalizer(rt: *mut JSRuntime, val: JSValue) {
        let p = val.object();
        if !p.u.cfunc.realm.is_null() {
            JS_FreeContext(p.u.cfunc.realm);
        }
    }

    pub unsafe fn hook_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
        let p = val.object();
        if !p.u.cfunc.realm.is_null() {
            mark_func(rt, &mut (*p.u.cfunc.realm).header);
        }
    }
}

impl JSContext {
    /// Create new function object with native callback and speficied proto object.
    /// at least `length` arguments will be readable in arguments.
    pub fn create_native_function_with_proto(
        &mut self,
        callback: JSNativeCallback,
        name: &str,
        length: u8,
        cproto: JSCFunctionEnum,
        magic: i16,
        proto_val: JSValue,
    ) -> JSResult {
        debug_assert!(matches!(
            cproto,
            JSCFunctionEnum::NativeFunc
                | JSCFunctionEnum::NativeConstructor
                | JSCFunctionEnum::NativeConstructorOrFunc
        ));

        let func_obj = unsafe {
            JS_NewObjectProtoClass(self, proto_val, JS_CLASS_C_FUNCTION).check_exception()?
        };

        let p = unsafe { func_obj.object() };

        p.detailed.set_constructor(matches!(
            cproto,
            JSCFunctionEnum::NativeConstructor | JSCFunctionEnum::NativeConstructorOrFunc
        ));

        unsafe {
            let cfunc = &mut p.u.cfunc;

            cfunc.realm = JS_DupContext(self);
            cfunc.c_function.native_callback = callback;
            cfunc.length = length;
            cfunc.cproto = cproto;
            cfunc.magic = magic;
        }

        // define properties length and name on function.
        let name_atom = self.create_atom(name).unwrap();

        unsafe {
            js_function_set_properties(self, func_obj, name_atom, length as i32);
            JS_FreeAtom(self, name_atom);
        }

        Ok(func_obj)
    }

    /// Create new function object with native callback. using function proto as proto object.
    /// at least `length` arguments will be readable in arguments.
    #[inline(always)]
    pub fn create_native_function(
        &mut self,
        callback: JSNativeCallback,
        name: &str,
        length: u8,
        cproto: JSCFunctionEnum,
        magic: i16,
    ) -> JSResult {
        self.create_native_function_with_proto(
            callback,
            name,
            length,
            cproto,
            magic,
            self.function_proto,
        )
    }
}

// Note: at least 'length' arguments will be readable in 'argv'
#[deprecated(note = "use native callback instead")]
pub unsafe fn JS_NewCFunction3(
    ctx: *mut JSContext,
    func: JSCFunction,
    name: &CStr,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
    proto_val: JSValueConst,
) -> JSValue {
    let func_obj = JS_NewObjectProtoClass(ctx, proto_val, JS_CLASS_C_FUNCTION);
    if func_obj.is_exception() {
        return func_obj;
    }

    let p = func_obj.object();

    {
        let cf = &mut p.u.cfunc;
        cf.realm = JS_DupContext(ctx);
        cf.c_function.generic = func;
        cf.length = length;
        cf.cproto = cproto;
        cf.magic = magic;
    }

    p.detailed.set_constructor(matches!(
        cproto,
        JSCFunctionEnum::Constructor
            | JSCFunctionEnum::ConstructorMagic
            | JSCFunctionEnum::ConstructorOrFunc
            | JSCFunctionEnum::ConstructorOrFuncMagic
    ));

    // define propperties on func_obj: length and name.
    let name_atom = (*ctx).create_atom(name.to_string_lossy().as_ref()).unwrap();

    js_function_set_properties(ctx, func_obj, name_atom, length as i32);
    JS_FreeAtom(ctx, name_atom);

    func_obj
}

/* Note: at least 'length' arguments will be readable in 'argv' */
#[inline(always)]
pub unsafe fn JS_NewCFunction2(
    ctx: *mut JSContext,
    func: JSCFunction,
    name: &CStr,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    JS_NewCFunction3(
        ctx,
        func,
        name,
        length,
        cproto,
        magic,
        (*ctx).function_proto,
    )
}

#[inline(always)]
pub unsafe fn JS_NewCFunction(
    ctx: *mut JSContext,
    func: JSCFunction,
    name: &CStr,
    length: u8,
) -> JSValue {
    JS_NewCFunction2(ctx, func, name, length, JSCFunctionEnum::Generic, 0)
}

#[inline(always)]
pub unsafe fn JS_NewCFunctionMagic(
    ctx: *mut JSContext,
    func: JSCFunctionMagic,
    name: &CStr,
    length: u8,
    cproto: JSCFunctionEnum,
    magic: i16,
) -> JSValue {
    JS_NewCFunction2(ctx, std::mem::transmute(func), name, length, cproto, magic)
}

pub unsafe fn JS_SetPropertyFunctionList(
    ctx: *mut JSContext,
    obj: JSValueConst,
    tab: *const JSCFunctionListEntry,
    len: usize,
) {
    for i in 0..len {
        let e = tab.add(i);
        let atom = (*ctx)
            .create_atom((*e).name.to_string_lossy().as_ref())
            .expect("TODO");

        JS_InstantiateFunctionListItem(ctx, obj, atom, e);
        JS_FreeAtom(ctx, atom);
    }
}

pub unsafe fn JS_InstantiateFunctionListItem(
    ctx: *mut JSContext,
    obj: JSValueConst,
    atom: JSAtom,
    e: *const JSCFunctionListEntry,
) -> i32 {
    let val: JSValue;
    let mut prop_flags = (*e).prop_flags;

    match (*e).def_type {
        JS_DEF_ALIAS => {
            // using autoinit for aliases is not safe
            let atom1 = (*ctx)
                .create_atom((*e).u.alias.name.to_string_lossy().as_ref())
                .expect("TODO");

            val = match (*e).u.alias.base {
                -1 => obj.get_property(ctx.as_mut().unwrap(), atom1),
                0 => (*ctx).global_obj.get_property(ctx.as_mut().unwrap(), atom1),
                1 => (*ctx).class_proto[JS_CLASS_ARRAY as usize]
                    .get_property(ctx.as_mut().unwrap(), atom1),
                _ => {
                    unreachable!(); // abort();
                }
            };
            JS_FreeAtom(ctx, atom1);

            if atom == JS_ATOM_Symbol_toPrimitive {
                // Symbol.toPrimitive functions are not writable
                prop_flags = JS_PROP_CONFIGURABLE as u8;
            } else if atom == JS_ATOM_Symbol_hasInstance {
                // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                prop_flags = 0;
            }
        }
        JS_DEF_CFUNC => {
            if atom == JS_ATOM_Symbol_toPrimitive {
                // Symbol.toPrimitive functions are not writable
                prop_flags = JS_PROP_CONFIGURABLE as u8;
            } else if atom == JS_ATOM_Symbol_hasInstance {
                // Function.prototype[Symbol.hasInstance] is not writable nor configurable
                prop_flags = 0;
            }
            JS_DefineAutoInitProperty(
                ctx,
                obj,
                atom,
                JSAutoInitIDEnum::JS_AUTOINIT_ID_PROP,
                e as *mut c_void,
                prop_flags as _,
            )
            .unwrap();
            return 0;
        }
        JS_DEF_CGETSET | JS_DEF_CGETSET_MAGIC => {
            // XXX: use autoinit again ?
            let mut buf: [c_char; 64] = [0; 64];

            let getter = if let Some(f) = (*e).u.getset.get {
                libc::snprintf(
                    buf.as_mut_ptr(),
                    std::mem::size_of::<[c_char; 64]>(),
                    c"get %s".as_ptr(),
                    (*e).name,
                );
                JS_NewCFunction2(
                    ctx,
                    f.generic,
                    CStr::from_ptr(buf.as_ptr()),
                    0,
                    if (*e).def_type == JS_DEF_CGETSET_MAGIC {
                        JSCFunctionEnum::GetterMagic
                    } else {
                        JSCFunctionEnum::Getter
                    },
                    (*e).magic,
                )
            } else {
                JSValue::UNDEFINED
            };

            let setter = if let Some(t) = (*e).u.getset.set {
                libc::snprintf(
                    buf.as_mut_ptr(),
                    std::mem::size_of::<[c_char; 64]>(),
                    c"set %s".as_ptr(),
                    (*e).name,
                );
                JS_NewCFunction2(
                    ctx,
                    t.generic,
                    CStr::from_ptr(buf.as_ptr()),
                    1,
                    if (*e).def_type == JS_DEF_CGETSET_MAGIC {
                        JSCFunctionEnum::SetterMagic
                    } else {
                        JSCFunctionEnum::Setter
                    },
                    (*e).magic,
                )
            } else {
                JSValue::UNDEFINED
            };

            JS_DefinePropertyGetSet(ctx, obj, atom, getter, setter, prop_flags as _);
            return 0;
        }
        JS_DEF_PROP_INT32 => {
            val = JSValue::from_i32((*e).u.i32);
        }
        JS_DEF_PROP_INT64 => {
            val = JSValue::from_i64((*e).u.i64);
        }
        JS_DEF_PROP_DOUBLE => {
            val = JSValue::from_f64_as_is((*e).u.f64);
        }
        JS_DEF_PROP_UNDEFINED => {
            val = JSValue::UNDEFINED;
        }
        JS_DEF_PROP_STRING | JS_DEF_OBJECT => {
            JS_DefineAutoInitProperty(
                ctx,
                obj,
                atom,
                JSAutoInitIDEnum::JS_AUTOINIT_ID_PROP,
                e as *mut c_void,
                prop_flags as _,
            )
            .unwrap();
            return 0;
        }
        _ => {
            unreachable!();
        }
    }

    let _ = obj.define_property_value(ctx.as_mut().unwrap(), atom, val, prop_flags as _);

    0
}

pub unsafe fn JS_InstantiateFunctionListItem2(
    ctx: *mut JSContext,
    p: *mut JSObject,
    atom: JSAtom,
    opaque: *mut c_void,
) -> JSValue {
    let e: *const JSCFunctionListEntry = opaque as *const JSCFunctionListEntry;
    let val: JSValue;

    match (*e).def_type {
        0 => {
            // Assuming JS_DEF_CFUNC is 0
            val = JS_NewCFunction2(
                ctx,
                (*e).u.func.cfunc.generic,
                (*e).name,
                (*e).u.func.length,
                (*e).u.func.cproto,
                (*e).magic,
            );
        }
        1 => {
            // Assuming JS_DEF_PROP_STRING is 1
            val = JS_NewAtomString(ctx, (*e).u.str);
        }
        2 => {
            // Assuming JS_DEF_OBJECT is 2
            val = JS_NewObject(ctx);
            JS_SetPropertyFunctionList(ctx, val, (*e).u.prop_list.tab, (*e).u.prop_list.len as _);
        }
        t => {
            unreachable!("invalid def_type {t}");
        }
    }

    val
}
