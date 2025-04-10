use std::ffi::{c_int, c_void, CStr};

use crate::{
    array::convert_fast_array_to_array,
    atom::{
        consts::{JS_ATOM_constructor, JS_ATOM_prototype},
        JSAtom, JSAtomTypeHint, JS_AtomIsArrayIndex, JS_FreeAtomRT, __JS_FindAtom, JS_ATOM_NULL,
    },
    context::{JSContext, JS_FreeContext},
    function::{
        JSCFunction, JSCFunctionEnum, JSFunctionDef, JS_GetFunctionRealm, JS_NewCFunction2,
    },
    gc::{set_cycle_flag, JS_MarkFunc},
    runtime::JSRuntime,
    value::{
        object::{
            JSObject, JSPropertyDescriptor, JS_NewObject, JS_NewObjectProtoClass, PropertyEnum,
            JS_PROP_CONFIGURABLE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE,
        },
        JSTag, JSValue, JSValueConst, JS_FreeValue, JS_TAG_NULL, JS_VALUE_GET_OBJ,
        JS_VALUE_GET_TAG,
    },
    *,
};

pub const JS_DEFINE_CLASS_HAS_HERITAGE: i32 = 1 << 0;

pub const OP_DEFINE_METHOD_METHOD: u8 = 0;
pub const OP_DEFINE_METHOD_GETTER: u8 = 1;
pub const OP_DEFINE_METHOD_SETTER: u8 = 2;
pub const OP_DEFINE_METHOD_ENUMERABLE: u8 = 4;

pub type JSClassID = u32;

pub type JSClassFinalizer = ::core::option::Option<unsafe fn(rt: *mut JSRuntime, val: JSValue)>;

pub type JSClassGCMark =
    ::core::option::Option<unsafe fn(rt: *mut JSRuntime, val: JSValue, mark_func: JS_MarkFunc)>;

pub type JSClassCall = unsafe fn(
    ctx: &mut JSContext,
    func_obj: JSValue,
    this_val: JSValue,
    argc: ::core::ffi::c_int,
    argv: *const JSValue,
    flags: ::core::ffi::c_int,
) -> JSValue;

#[derive(Copy, Clone, Default)]
#[repr(C)]
pub struct JSClassExoticMethods {
    pub get_own_property:
        Option<unsafe fn(*mut JSContext, *mut JSPropertyDescriptor, JSValueConst, JSAtom) -> c_int>,

    pub get_own_property_names:
        Option<unsafe fn(*mut JSContext, *mut *mut PropertyEnum, *mut u32, JSValueConst) -> c_int>,

    pub delete_property: Option<unsafe fn(*mut JSContext, JSValueConst, JSAtom) -> c_int>,

    pub define_own_property: Option<
        unsafe fn(
            *mut JSContext,
            JSValueConst,
            JSAtom,
            JSValueConst,
            JSValueConst,
            JSValueConst,
            c_int,
        ) -> c_int,
    >,

    pub has_property: Option<unsafe fn(*mut JSContext, JSValueConst, JSAtom) -> c_int>,

    pub get_property:
        Option<unsafe fn(*mut JSContext, JSValueConst, JSAtom, JSValueConst) -> JSValue>,

    /// returns 0,1,-1
    pub set_property: Option<
        unsafe fn(*mut JSContext, JSValueConst, JSAtom, JSValueConst, JSValueConst, c_int) -> c_int,
    >,
}

#[derive(Copy, Clone)]
#[repr(C)]
pub struct JSClassDef {
    pub class_name: *const i8,
    pub finalizer: JSClassFinalizer,
    pub gc_mark: JSClassGCMark,
    pub call: Option<JSClassCall>,
    pub exotic: *const JSClassExoticMethods,
}

impl Default for JSClassDef {
    fn default() -> Self {
        let mut s = core::mem::MaybeUninit::<Self>::uninit();
        unsafe {
            core::ptr::write_bytes(s.as_mut_ptr(), 0, 1);
            s.assume_init()
        }
    }
}

#[repr(C)]
pub struct JSClass {
    /// 0 means free entry
    pub class_id: JSClassID,
    pub class_name: JSAtom,
    pub finalizer: JSClassFinalizer,
    pub gc_mark: JSClassGCMark,
    pub call: Option<JSClassCall>,

    /// pointers for exotic behavior, can be NULL if none are present
    pub exotic: *const JSClassExoticMethods,
}

#[repr(C)]
pub struct JSClassShortDef {
    pub class_name: JSAtom,
    pub finalizer: JSClassFinalizer,
    pub gc_mark: JSClassGCMark,
}

#[deprecated(note = "use JSRuntime::add_classes() instead")]
pub unsafe fn init_class_range(
    rt: &mut JSRuntime,
    tab: &[JSClassShortDef],
    start: JSClassID,
) -> Result<(), i32> {
    (*rt).add_classes(tab, start)
}

impl JSRuntime {
    /// create a new object internal class. The finalizer can be NULL if none is needed.
    unsafe fn add_class_internal(
        &mut self,
        class_id: JSClassID,
        class_def: &JSClassDef,
        name: JSAtom,
    ) -> Result<(), i32> {
        if class_id >= (1 << 16) {
            return Err(-1);
        }

        if (class_id as usize) < self.class_array.len() {
            if self.class_array[class_id as usize].class_id != 0 {
                return Err(-1); // class_id slot is in use
            }
        } else {
            if class_id as usize >= self.class_array.capacity() {
                self.class_array
                    .try_reserve(class_id as usize - self.class_array.capacity() + 1)
                    .map_err(|_| -1)?;
            }

            // fill slots with empty entries
            for _ in 0..=(class_id as usize - self.class_array.len()) {
                self.class_array.push(JSClass {
                    class_id: 0,
                    class_name: JS_ATOM_NULL,
                    finalizer: None,
                    gc_mark: None,
                    call: None,
                    exotic: std::ptr::null(),
                });
            }
        }

        self.class_array[class_id as usize] = JSClass {
            class_id,
            class_name: name.dup(self),
            finalizer: (*class_def).finalizer,
            gc_mark: (*class_def).gc_mark,
            call: (*class_def).call,
            exotic: (*class_def).exotic,
        };

        Ok(())
    }

    /// register a class def to runtime, with given class_id
    pub fn add_class(&mut self, class_id: JSClassID, class_def: &JSClassDef) -> Result<(), i32> {
        let cstr = unsafe { CStr::from_ptr(class_def.class_name) };
        let str = cstr.to_string_lossy();

        let name =
            if let Some(a) = unsafe { __JS_FindAtom(self, str.as_ref(), JSAtomTypeHint::String) } {
                a
            } else {
                self.add_atom_str(str.as_ref(), JSAtomTypeHint::String)?
            };

        unsafe {
            let ret = self.add_class_internal(class_id, class_def, name);
            JS_FreeAtomRT(self, name);
            ret
        }
    }

    pub fn add_classes(&mut self, tab: &[JSClassShortDef], start: JSClassID) -> Result<(), i32> {
        let mut class_id = start;
        for el in tab {
            let cd = JSClassDef {
                class_name: std::ptr::null(),
                exotic: std::ptr::null(),
                finalizer: (*el).finalizer,
                gc_mark: (*el).gc_mark,
                call: None,
            };
            unsafe {
                self.add_class_internal(class_id, &cd, (*el).class_name)?;
            }

            class_id += 1;
        }

        Ok(())
    }
}

impl JSContext {
    /// get class proto object by `class_id`.
    /// if class proto object is not initialized in this context, returns JS_NULL.
    pub fn get_class_proto(&self, class_id: JSClassID) -> JSValue {
        debug_assert!((class_id as usize) < self.class_proto.len());
        // debug_assert_eq!(self.class_proto.len(), unsafe {
        //     self.rt.as_ref().class_array.len()
        // });
        self.class_proto[class_id as usize].dup()
    }

    pub fn set_class_proto(&mut self, class_id: JSClassID, proto_obj: JSValue) {
        // debug_assert_eq!(self.class_proto.len(), self.rt().class_array.len());
        debug_assert!(class_id < self.class_proto.len() as JSClassID);
        debug_assert_eq!(
            JS_VALUE_GET_TAG(self.class_proto[class_id as usize]),
            JS_TAG_NULL
        );

        unsafe {
            let old = self.class_proto[class_id as usize].replace(proto_obj);
            JS_FreeValue(self, old);
        }
    }

    /// make function `func_obj` to constructor, adopt `proto` as prototype,
    /// then add it to global namespace as `name`.
    /// Note:: both `func_obj` and `proto` are  **NOT CONSUMED**.
    pub fn add_global_constructor(&mut self, name: &str, func_obj: JSValue, proto: JSValue) {
        self.global_obj
            .define_property_value_str(
                self,
                name,
                func_obj.dup(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        unsafe {
            JS_SetConstructor(self, func_obj, proto);
            JS_FreeValue(self, func_obj);
        }
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    pub fn create_global_constructor(
        &mut self,
        name: &CStr,
        func: JSCFunction,
        length: i32,
        proto: JSValue,
    ) -> JSValue {
        let func_obj = unsafe {
            JS_NewCFunction2(
                self,
                func,
                name,
                length as _,
                JSCFunctionEnum::ConstructorOrFunc,
                0,
            )
        };
        self.add_global_constructor(name.to_str().unwrap(), func_obj, proto);
        func_obj
    }
}

#[deprecated(note = "use JSContext::get_class_proto() instead")]
pub unsafe fn JS_GetClassProto(ctx: *mut JSContext, class_id: JSClassID) -> JSValue {
    (*ctx).get_class_proto(class_id)
}

#[deprecated(note = "use JSContext::set_class_proto() instead")]
pub unsafe fn JS_SetClassProto(ctx: *mut JSContext, class_id: JSClassID, obj: JSValue) {
    (*ctx).set_class_proto(class_id, obj);
}

#[deprecated(note = "use JSRuntime::add_class() instead")]
pub unsafe fn JS_NewClass(
    rt: *mut JSRuntime,
    class_id: JSClassID,
    class_def: *const JSClassDef,
) -> Result<(), i32> {
    (*rt).add_class(class_id, class_def.as_ref().unwrap())
}

/* create a new object internal class. Return -1 if error, 0 if OK. The finalizer can be NULL if none is needed. */
#[deprecated(note = "use JSRuntime::add_class_internal() instead")]
pub unsafe fn JS_NewClass1(
    rt: *mut JSRuntime,
    class_id: JSClassID,
    class_def: *const JSClassDef,
    name: JSAtom,
) -> Result<(), i32> {
    (*rt).add_class_internal(class_id, class_def.as_ref().unwrap(), name)
}

/// Note: 'func_obj' is not necessarily a constructor
/// Note: both `func_obj` and `proto` keep ref count unchanged after calling.
pub unsafe fn JS_SetConstructor2(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    proto: JSValueConst,
    proto_flags: u32,
    ctor_flags: u32,
) {
    func_obj
        .define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_prototype,
            proto.dup(),
            proto_flags,
        )
        .unwrap();

    proto
        .define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_constructor,
            func_obj.dup(),
            ctor_flags,
        )
        .unwrap();

    set_cycle_flag(ctx, func_obj);
    set_cycle_flag(ctx, proto);
}

pub unsafe fn JS_SetConstructor(ctx: *mut JSContext, func_obj: JSValueConst, proto: JSValueConst) {
    JS_SetConstructor2(
        ctx,
        func_obj,
        proto,
        0,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
}

#[deprecated(note = "use JSContext::add_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor2(
    ctx: *mut JSContext,
    func_obj: JSValue,
    name: &CStr,
    proto: JSValueConst,
) {
    (*ctx).add_global_constructor(name.to_string_lossy().as_ref(), func_obj, proto);
}

#[deprecated(note = "use JSContext::create_global_constructor() instead")]
pub unsafe fn JS_NewGlobalCConstructor(
    ctx: *mut JSContext,
    name: &CStr,
    func: JSCFunction,
    length: i32,
    proto: JSValueConst,
) -> JSValueConst {
    (*ctx).create_global_constructor(name, func, length, proto)
}

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ClassFieldsDef {
    pub fields_init_fd: *mut JSFunctionDef,
    pub computed_fields_count: i32,
    pub need_brand: bool,
    pub brand_push_pos: i32,
    pub is_static: bool,
}

impl Default for ClassFieldsDef {
    fn default() -> Self {
        Self {
            fields_init_fd: std::ptr::null_mut(),
            computed_fields_count: Default::default(),
            need_brand: Default::default(),
            brand_push_pos: Default::default(),
            is_static: Default::default(),
        }
    }
}

pub unsafe fn js_instantiate_prototype(
    ctx: *mut JSContext,
    p: *mut JSObject,
    atom: JSAtom,
    opaque: *mut c_void,
) -> JSValue {
    let this_val = JS_MKPTR!(JSTag::Object, p.cast());
    let obj = JS_NewObject(ctx);
    if obj.is_exception() {
        return JSValue::EXCEPTION;
    }

    set_cycle_flag(ctx, obj);
    set_cycle_flag(ctx, this_val);

    if obj
        .define_property_value(
            ctx.as_mut().unwrap(),
            JS_ATOM_constructor,
            this_val.dup(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )
        .is_ok()
    {
        obj
    } else {
        JS_FreeValue(ctx, obj);
        JSValue::EXCEPTION
    }
}

pub unsafe fn js_create_from_ctor(
    ctx: *mut JSContext,
    ctor: JSValueConst,
    class_id: JSClassID,
) -> JSValue {
    let proto = if ctor.is_undefined() {
        (*ctx).class_proto[class_id as usize].dup()
    } else {
        let p = ctor.get_property(ctx.as_mut().unwrap(), JS_ATOM_prototype);
        if p.is_exception() {
            return p;
        }
        if !p.is_object() {
            JS_FreeValue(ctx, p);
            let realm = JS_GetFunctionRealm(ctx, ctor);
            if realm.is_null() {
                return JSValue::EXCEPTION;
            }
            (*realm).class_proto[class_id as usize].dup()
        } else {
            p
        }
    };

    let obj = JS_NewObjectProtoClass(ctx, proto, class_id);
    JS_FreeValue(ctx, proto);
    return obj;
}

unsafe fn js_arguments_define_own_property(
    ctx: *mut JSContext,
    this_obj: JSValueConst,
    prop: JSAtom,
    val: JSValueConst,
    getter: JSValueConst,
    setter: JSValueConst,
    flags: c_int,
) -> c_int {
    let p = JS_VALUE_GET_OBJ(this_obj);
    let mut idx: u32 = 0;

    /* convert to normal array when redefining an existing numeric field */
    if (*p).detailed.fast_array()
        && JS_AtomIsArrayIndex(ctx, &mut idx, prop)
        && idx < (*p).array_length_unchecked() as u32
    {
        if convert_fast_array_to_array(ctx, p) != 0 {
            return -1;
        }
    }

    /* run the default define own property */
    this_obj
        .define_property(
            ctx.as_mut().unwrap(),
            prop,
            val,
            getter,
            setter,
            flags as u32 | JS_PROP_NO_EXOTIC,
        )
        .map_or(-1, |b| b as i32)
}

pub static js_arguments_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    define_own_property: Some(js_arguments_define_own_property),
    delete_property: None,
    get_own_property: None,
    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
};

pub unsafe fn js_c_function_finalizer(rt: *mut JSRuntime, val: JSValue) {
    let p = val.object();

    if !p.u.cfunc.realm.is_null() {
        JS_FreeContext(p.u.cfunc.realm);
    }
}

pub unsafe fn js_c_function_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p = val.object();

    if !p.u.cfunc.realm.is_null() {
        mark_func(rt, &mut (*p.u.cfunc.realm).header);
    }
}
