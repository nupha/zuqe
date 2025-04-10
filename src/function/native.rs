use std::ffi::{c_char, c_void, CStr};

use crate::{
    atom::{consts::*, JSAtom, JS_FreeAtom, JS_NewAtomString},
    context::{JSContext, JSContextRef, JS_DupContext},
    error::JSResult,
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_free_rt,
    runtime::JSRuntime,
    value::{
        object::{
            JSAutoInitIDEnum, JSObject, JS_DefineAutoInitProperty, JS_DefinePropertyGetSet,
            JS_GetOpaque, JS_NewObject, JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE,
        },
        JSValue, JSValueConst, JS_FreeValueRT, JS_IsException, JS_DEF_ALIAS, JS_DEF_CFUNC,
        JS_DEF_CGETSET, JS_DEF_CGETSET_MAGIC, JS_DEF_OBJECT, JS_DEF_PROP_DOUBLE, JS_DEF_PROP_INT32,
        JS_DEF_PROP_INT64, JS_DEF_PROP_STRING, JS_DEF_PROP_UNDEFINED, JS_VALUE_GET_OBJ,
    },
    JS_CLASS_ARRAY, JS_CLASS_C_FUNCTION, JS_CLASS_C_FUNCTION_DATA,
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

    NativeCallback,
}

pub struct CallArgs<'a> {
    pub this: JSValue,
    pub args: &'a [JSValue],
    pub magic: i32,
}

impl CallArgs<'_> {
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
}

impl<'a> CallArgs<'a> {
    pub fn new(this: JSValue, args: &'a [JSValue], magic: i32) -> Self {
        Self { this, args, magic }
    }
}

pub type JSNativeCallback = for<'a> fn(ctx: &'a mut JSContext, args: CallArgs<'a>) -> JSResult;

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
#[repr(C)]
pub union JSCFunctionType {
    pub native_callback: JSNativeCallback,

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

#[repr(C)]
pub struct JSCFunctionDataRecord {
    pub func: JSCFunctionData,
    /// argument slot length
    pub length: u8,
    pub data_len: u8,
    pub magic: u16,
    pub data: [JSValue; 0],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct JSObjectCFunc {
    pub realm: *mut JSContext,
    pub c_function: JSCFunctionType,
    pub length: u8,
    pub cproto: JSCFunctionEnum,
    pub magic: i16,
}

// Note: at least 'length' arguments will be readable in 'argv'
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
    if JS_IsException(func_obj) {
        return func_obj;
    }

    let p = JS_VALUE_GET_OBJ(func_obj).as_mut().unwrap();
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

    // if name.is_null() {
    //     name = "\0".as_ptr().cast(); //   使用空字符串代替NULL
    // }

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
        _ => {
            unreachable!();
        }
    }

    val
}

pub unsafe fn js_c_function_data_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let s = JS_GetOpaque(val, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();
    if !s.is_null() {
        for i in 0..(*s).data_len {
            JS_FreeValueRT(rt, *(*s).data.as_ptr().add(i as usize));
        }
        js_free_rt(rt, s as *mut std::ffi::c_void);
    }
}

pub unsafe fn js_c_function_data_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    let s = JS_GetOpaque(val, JS_CLASS_C_FUNCTION_DATA).cast::<JSCFunctionDataRecord>();
    if !s.is_null() {
        for i in 0..(*s).data_len {
            JS_MarkValue(rt, *(*s).data.as_ptr().add(i as usize), mark_func);
        }
    }
}
