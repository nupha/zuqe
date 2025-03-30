use std::ffi::{c_int, c_void, CStr};

use crate::{
    array::convert_fast_array_to_array,
    atom::{
        consts::{JS_ATOM_constructor, JS_ATOM_prototype},
        JSAtom, JSAtomType, JSAtomTypeHint, JS_AtomIsArrayIndex, JS_FreeAtomRT, __JS_FindAtom,
    },
    context::{JSContext, JS_FreeContext},
    function::{
        JSCFunction, JSCFunctionEnum, JSFunctionDef, JS_GetFunctionRealm, JS_NewCFunction2,
    },
    gc::{set_cycle_flag, JS_MarkFunc},
    mem::js_realloc_rt,
    runtime::JSRuntime,
    value::{
        object::{
            JSObject, JSPropertyDescriptor, JSPropertyEnum, JS_DefineProperty,
            JS_DefinePropertyValue, JS_DefinePropertyValueStr, JS_GetProperty, JS_IsConstructor,
            JS_NewObject, JS_NewObjectProtoClass, JS_PROP_CONFIGURABLE, JS_PROP_NO_EXOTIC,
            JS_PROP_WRITABLE,
        },
        JSTag, JSValue, JSValueConst, JS_DupValue, JS_FreeValue, JS_IsException, JS_IsNull,
        JS_IsObject, JS_IsUndefined, JS_TAG_NULL, JS_VALUE_GET_OBJ, JS_VALUE_GET_TAG,
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
    ctx: *mut JSContext,
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

    pub get_own_property_names: Option<
        unsafe fn(*mut JSContext, *mut *mut JSPropertyEnum, *mut u32, JSValueConst) -> c_int,
    >,

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
    pub class_id: JSClassID, /* 0 means free entry */
    pub class_name: JSAtom,
    pub finalizer: JSClassFinalizer,
    pub gc_mark: JSClassGCMark,
    pub call: Option<JSClassCall>,
    /* pointers for exotic behavior, can be NULL if none are present */
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
        class_def: *const JSClassDef,
        name: JSAtom,
    ) -> Result<(), i32> {
        if class_id >= (1 << 16) {
            return Err(-1);
        } else if class_id < self.class_count as u32
            && (*self.class_array.offset(class_id as _)).class_id != 0
        {
            return Err(-1); // class_id is used
        }

        if class_id >= self.class_count as u32 {
            let new_size = std::cmp::max(
                JS_CLASS_INIT_COUNT,
                std::cmp::max(class_id + 1, (self.class_count as u32 * 3) / 2),
            );

            // reallocate the context class prototype array, if any
            for ctx in self.context_list.iter_mut() {
                let sz = new_size as usize - (*ctx).class_proto.len();
                if ctx.class_proto.try_reserve_exact(sz).is_err() {
                    return Err(-1);
                } else {
                    ctx.class_proto
                        .extend(std::iter::repeat_n(JSValue::NULL, sz));
                }
            }

            // let r = list_foreach_with_ret(&mut self.context_list, |el| {
            //     let ctx = list_entry!(el, JSContext, link);

            //     let sz = new_size as usize - (*ctx).class_proto.len();
            //     if (*ctx).class_proto.try_reserve_exact(sz).is_err() {
            //         Some(-1) // abort err
            //     } else {
            //         (*ctx).class_proto.extend(std::iter::repeat_n(JS_NULL, sz));
            //         None
            //     }
            // });
            // if r.is_some() {
            //     return Err(-1);
            // }

            // reallocate the class array
            let new_class_array: *mut JSClass = js_realloc_rt(
                self,
                self.class_array.cast(),
                std::mem::size_of::<JSClass>() * new_size as usize,
            )
            .cast();

            if new_class_array.is_null() {
                return Err(-1);
            }

            std::ptr::write_bytes(
                new_class_array.offset(self.class_count as isize),
                0,
                (new_size as usize - self.class_count as usize) * std::mem::size_of::<JSClass>(),
            );

            self.class_array = new_class_array;
            self.class_count = new_size as _;
        }

        let cl = self.class_array.offset(class_id as _);
        (*cl).class_id = class_id;
        (*cl).class_name = name.dup(self);
        (*cl).finalizer = (*class_def).finalizer;
        (*cl).gc_mark = (*class_def).gc_mark;
        (*cl).call = (*class_def).call;
        (*cl).exotic = (*class_def).exotic;

        Ok(())
    }

    /// register a class def to runtime, with given class_id
    pub fn add_class(&mut self, class_id: JSClassID, class_def: &JSClassDef) -> Result<(), i32> {
        let cstr = unsafe { CStr::from_ptr(class_def.class_name) };
        let str = cstr.to_str().unwrap();

        let name = if let Some(a) = unsafe { __JS_FindAtom(self, str, JSAtomTypeHint::String) } {
            a
        } else {
            self.add_atom_str(str, JSAtomTypeHint::String)?
        };

        unsafe {
            let ret = self.add_class_internal(class_id, class_def, name);
            JS_FreeAtomRT(self, name);
            ret
        }
    }

    pub unsafe fn add_classes(
        &mut self,
        tab: &[JSClassShortDef],
        start: JSClassID,
    ) -> Result<(), i32> {
        let mut class_id = start;
        for elem in tab {
            let mut cm = std::mem::zeroed::<JSClassDef>();
            cm.finalizer = (*elem).finalizer;
            cm.gc_mark = (*elem).gc_mark;
            self.add_class_internal(class_id, &cm, (*elem).class_name)?;

            class_id += 1;
        }

        Ok(())
    }
}

impl JSContext {
    /// get class proto object by `class_id`.
    /// if class proto object is not initialized in this context, returns JS_NULL.
    pub fn get_class_proto(&self, class_id: JSClassID) -> JSValue {
        debug_assert_eq!(self.class_proto.len(), unsafe {
            self.rt.as_ref().class_count as usize
        });
        debug_assert!((class_id as usize) < self.class_proto.len());
        self.class_proto[class_id as usize].dup()
    }

    pub fn set_class_proto(&mut self, class_id: JSClassID, proto_obj: JSValue) {
        debug_assert_eq!(self.class_proto.len(), self.rt().class_count as usize);
        debug_assert!(class_id < self.class_proto.len() as JSClassID);
        debug_assert_eq!(
            JS_VALUE_GET_TAG(self.class_proto[class_id as usize]),
            JS_TAG_NULL
        );

        unsafe { self.class_proto[class_id as usize].replace(proto_obj) };
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
    (*rt).add_class_internal(class_id, class_def, name)
}

// Note: 'func_obj' is not necessarily a constructor
pub unsafe fn JS_SetConstructor2(
    ctx: *mut JSContext,
    func_obj: JSValueConst,
    proto: JSValueConst,
    proto_flags: u32,
    ctor_flags: u32,
) {
    JS_DefinePropertyValue(
        ctx,
        func_obj,
        JS_ATOM_prototype,
        JS_DupValue(ctx, proto),
        proto_flags,
    );
    JS_DefinePropertyValue(
        ctx,
        proto,
        JS_ATOM_constructor,
        JS_DupValue(ctx, func_obj),
        ctor_flags,
    );
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

pub unsafe fn JS_NewGlobalCConstructor2(
    ctx: *mut JSContext,
    func_obj: JSValue,
    name: &CStr,
    proto: JSValueConst,
) {
    JS_DefinePropertyValueStr(
        ctx,
        (*ctx).global_obj,
        name,
        JS_DupValue(ctx, func_obj),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
    JS_SetConstructor(ctx, func_obj, proto);
    JS_FreeValue(ctx, func_obj);
}

pub unsafe fn JS_NewGlobalCConstructor(
    ctx: *mut JSContext,
    name: &CStr,
    func: JSCFunction,
    length: i32,
    proto: JSValueConst,
) -> JSValueConst {
    let func_obj = JS_NewCFunction2(
        ctx,
        func,
        name,
        length as _,
        JSCFunctionEnum::ConstructorOrFunc,
        0,
    );
    JS_NewGlobalCConstructor2(ctx, func_obj, name, proto);

    func_obj
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
    if JS_IsException(obj) {
        return JSValue::EXCEPTION;
    }
    set_cycle_flag(ctx, obj);
    set_cycle_flag(ctx, this_val);
    let ret = JS_DefinePropertyValue(
        ctx,
        obj,
        JS_ATOM_constructor,
        JS_DupValue(ctx, this_val),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
    if ret < 0 {
        JS_FreeValue(ctx, obj);
        return JSValue::EXCEPTION;
    }
    return obj;
}

pub unsafe fn js_create_from_ctor(
    ctx: *mut JSContext,
    ctor: JSValueConst,
    class_id: JSClassID,
) -> JSValue {
    let proto = if JS_IsUndefined(ctor) {
        JS_DupValue(ctx, (*ctx).class_proto[class_id as usize])
    } else {
        let pr = JS_GetProperty(ctx, ctor, JS_ATOM_prototype);
        if JS_IsException(pr) {
            return pr;
        }
        if !JS_IsObject(pr) {
            JS_FreeValue(ctx, pr);
            let realm = JS_GetFunctionRealm(ctx, ctor);
            if realm.is_null() {
                return JSValue::EXCEPTION;
            }
            JS_DupValue(ctx, (*realm).class_proto[class_id as usize])
        } else {
            pr
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
        && idx < (*p).u.array.count
    {
        if convert_fast_array_to_array(ctx, p) != 0 {
            return -1;
        }
    }

    /* run the default define own property */
    JS_DefineProperty(
        ctx,
        this_obj,
        prop,
        val,
        getter,
        setter,
        flags as u32 | JS_PROP_NO_EXOTIC,
    )
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
    let p: *mut JSObject = JS_VALUE_GET_OBJ(val);

    if !(*p).u.cfunc.realm.is_null() {
        JS_FreeContext((*p).u.cfunc.realm);
    }
}

pub unsafe fn js_c_function_mark(rt: *mut JSRuntime, val: JSValueConst, mark_func: JS_MarkFunc) {
    let p: *mut JSObject = JS_VALUE_GET_OBJ(val);

    if !(*p).u.cfunc.realm.is_null() {
        mark_func(rt, &mut (*(*p).u.cfunc.realm).header);
    }
}
