use std::ffi::CStr;

use crate::{
    array::convert_fast_array_to_array,
    atom::{
        __JS_FindAtom, JS_ATOM_NULL, JSAtom, JSAtomTypeHint,
        consts::{JS_ATOM_constructor, JS_ATOM_prototype},
    },
    context::JSContext,
    error::JSResult,
    function::{JS_GetFunctionRealm, JSCFunctionEnum, JSFunctionDef, JSNativeCallback},
    gc::set_cycle_flag,
    runtime::JSRuntime,
    value::{
        JSValue,
        object::{
            JS_PROP_CONFIGURABLE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE, JSObject, JSObjectIsa,
            JSPropertyDescriptor, PropertyEnum,
        },
    },
    *,
};

pub const JS_DEFINE_CLASS_HAS_HERITAGE: i32 = 1 << 0;

pub const OP_DEFINE_METHOD_METHOD: u8 = 0;
pub const OP_DEFINE_METHOD_GETTER: u8 = 1;
pub const OP_DEFINE_METHOD_SETTER: u8 = 2;
pub const OP_DEFINE_METHOD_ENUMERABLE: u8 = 4;

pub type JSClassID = u32;

/// instance finalizer callback, obj is the object payload.
pub type JSClassFinalizer = fn(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize);

pub type JSClassGCMark = fn(val: JSValue);

/// class native constructor callback func
pub type JSClassCall = fn(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_val: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult;

#[derive(Copy, Clone, Default)]
#[repr(C)]
pub struct JSClassExoticMethods {
    pub get_own_property:
        Option<fn(&JSContext, JSValue, JSAtom) -> Result<Option<JSPropertyDescriptor>, i32>>,

    pub get_own_property_names:
        Option<fn(&JSContext, *mut *mut PropertyEnum, *mut u32, JSValue) -> i32>,

    pub delete_property: Option<fn(&JSContext, JSValue, JSAtom) -> Result<bool, i32>>,

    /// both JSValue(s), JSAtom should not be consumed.
    pub define_own_property:
        Option<fn(&JSContext, &JSValue, JSAtom, &JSValue, &JSValue, &JSValue, i32) -> i32>,

    pub has_property: Option<fn(ctx: &JSContext, this_obj: &JSObject, prop: JSAtom) -> i32>,

    pub get_property: Option<fn(&JSContext, JSValue, JSAtom, JSValue) -> JSValue>,

    pub set_property:
        Option<fn(&JSContext, JSValue, JSAtom, JSValue, JSValue, i32) -> Result<bool, i32>>,
}

#[repr(C)]
pub struct JSClassShortDef {
    pub class_name: JSAtom,
    pub finalizer: Option<JSClassFinalizer>,
    pub gc_mark: Option<JSClassGCMark>,
}

#[derive(Copy, Clone)]
pub struct JSClassDef {
    pub class_name: *const i8,
    pub finalizer: Option<JSClassFinalizer>,
    pub gc_mark: Option<JSClassGCMark>,

    /// native callback when called
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
    pub finalizer: Option<JSClassFinalizer>,
    pub gc_mark: Option<JSClassGCMark>,

    /// native callback when called
    pub call: Option<JSClassCall>,

    /// pointers for exotic behavior, can be NULL if none are present
    pub exotic: *const JSClassExoticMethods,
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
            if class_id as usize >= self.class_array.len() {
                // number of extra slots to grow
                let grow = class_id as usize - self.class_array.len() + 1;

                // grow context class prototype array
                for mut ctx in self.context_list.iter() {
                    let mut cx = ctx.0.borrow_mut();
                    cx.class_proto.try_reserve(grow).map_err(|_| -1)?;
                    cx.class_proto
                        .extend(std::iter::repeat_n(JSValue::NULL, grow));
                }

                // grow class array, fill slots with empty class entries
                self.class_array.try_reserve(grow).map_err(|_| -1)?;
                for _ in 0..grow {
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
        }

        self.class_array[class_id as usize] = JSClass {
            class_id,
            class_name: name.dup(self),
            finalizer: class_def.finalizer,
            gc_mark: class_def.gc_mark,
            call: class_def.call,
            exotic: class_def.exotic,
        };

        Ok(())
    }

    /// register a class def to runtime, with given class_id
    pub fn add_class(&mut self, class_id: JSClassID, class_def: &JSClassDef) -> Result<(), i32> {
        let cstr = unsafe { CStr::from_ptr(class_def.class_name) };
        let str = cstr.to_string_lossy();

        let name = if let Some(a) = __JS_FindAtom(self, str.as_ref(), JSAtomTypeHint::String) {
            a
        } else {
            self.register_atom_str(str.as_ref(), JSAtomTypeHint::String)?
        }
        .into_rc(self);

        unsafe { self.add_class_internal(class_id, class_def, *name) }
    }

    pub fn add_classes(&mut self, tab: &[JSClassShortDef], start: JSClassID) -> Result<(), i32> {
        let mut class_id = start;
        for el in tab {
            let cd = JSClassDef {
                class_name: std::ptr::null(),
                exotic: std::ptr::null(),
                finalizer: el.finalizer,
                gc_mark: el.gc_mark,
                call: None,
            };
            unsafe {
                self.add_class_internal(class_id, &cd, el.class_name)?;
            }

            class_id += 1;
        }

        Ok(())
    }

    #[inline(always)]
    pub fn is_class_registered(&self, class_id: JSClassID) -> bool {
        class_id < self.class_array.len() as JSClassID
            && self.class_array[class_id as usize].class_id != 0
    }
}

impl JSContext {
    /// get class proto object by `class_id`.
    /// if class proto object is not initialized in this context, returns JS_NULL.
    /// returned proto value is duplicated. caller should free it.
    pub fn get_class_proto(&self, class_id: JSClassID) -> JSValue {
        debug_assert!((class_id as usize) < self.0.borrow().class_proto.len());
        self.0.borrow().class_proto[class_id as usize].clone()
    }

    /// Note: `proto_obj` is consumed, old proto object will be consumed.
    pub fn set_class_proto(&self, class_id: JSClassID, proto_obj: JSValue) {
        debug_assert!(class_id < self.0.borrow().class_proto.len() as JSClassID);
        debug_assert!(self.0.borrow().class_proto[class_id as usize].is_null());

        self.0.borrow_mut().class_proto[class_id as usize].replace(proto_obj);
    }

    /// make function `func_obj` to be a constructor, set `proto` as its prototype,
    /// then add it to global namespace as `name`.
    /// Note:: both `func_obj` and `proto` are  **NOT CONSUMED**.
    pub fn add_global_constructor(&self, name: &str, func_obj: &JSValue, proto: &JSValue) {
        self.0
            .borrow()
            .global_obj
            .define_property_value_str(
                self,
                name,
                func_obj.clone(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .unwrap();

        JS_SetConstructor(self, func_obj, proto);
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    pub(crate) fn create_global_native_constructor_ex(
        &self,
        name: &CStr,
        callback: JSNativeCallback,
        length: i32,
        proto: &JSValue,
        cproto: JSCFunctionEnum,
        magic: i32,
    ) -> JSResult {
        debug_assert!(matches!(
            cproto,
            JSCFunctionEnum::NativeConstructorOrFunc | JSCFunctionEnum::NativeConstructor
        ));

        let name = name.to_str().unwrap();
        let func_obj =
            self.create_native_function(callback, name, length as _, cproto, magic as _)?;
        self.add_global_constructor(name, &func_obj, proto);
        Ok(func_obj)
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    pub fn create_global_native_constructor(
        &self,
        name: &CStr,
        callback: JSNativeCallback,
        length: i32,
        proto: &JSValue,
    ) -> JSResult {
        self.create_global_native_constructor_ex(
            name,
            callback,
            length,
            proto,
            JSCFunctionEnum::NativeConstructorOrFunc,
            0,
        )
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    pub fn create_global_native_constructor_only(
        &self,
        name: &CStr,
        callback: JSNativeCallback,
        length: i32,
        proto: &JSValue,
    ) -> JSResult {
        self.create_global_native_constructor_ex(
            name,
            callback,
            length,
            proto,
            JSCFunctionEnum::NativeConstructor,
            0,
        )
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    fn do_create_global_constructor(
        &self,
        name: &str,
        callback: JSNativeCallback,
        length: i32,
        proto: &JSValue,
        cproto: JSCFunctionEnum,
        magic: i16,
    ) -> JSResult {
        debug_assert!(matches!(
            cproto,
            JSCFunctionEnum::NativeConstructor | JSCFunctionEnum::NativeConstructorOrFunc
        ));

        let func_obj = self.create_native_function(callback, name, length as _, cproto, magic)?;
        self.add_global_constructor(name, &func_obj, proto);

        Ok(func_obj)
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    #[inline(always)]
    pub fn create_global_constructor(
        &self,
        name: &str,
        callback: JSNativeCallback,
        length: i32,
        proto: &JSValue,
        magic: i16,
    ) -> JSResult {
        self.do_create_global_constructor(
            name,
            callback,
            length,
            proto,
            JSCFunctionEnum::NativeConstructorOrFunc,
            magic,
        )
    }

    /// create a new native constructor, add it to global namespace.
    /// `proto` is the prototype object, it is **NOT CONSUMED**.
    /// returns new constructor function.
    #[inline(always)]
    pub fn create_global_constructor_only(
        &self,
        name: &str,
        callback: JSNativeCallback,
        length: i32,
        proto: &JSValue,
        magic: i16,
    ) -> JSResult {
        self.do_create_global_constructor(
            name,
            callback,
            length,
            proto,
            JSCFunctionEnum::NativeConstructor,
            magic,
        )
    }
}

/// set `proto` to func_obj.prototype property; set `func_obj`  to proto.constructor property.
/// Note: 'func_obj' is not necessarily a constructor
/// Note: both `func_obj` and `proto` are not consumed.
pub fn JS_SetConstructor2(
    ctx: &JSContext,
    func_obj: &JSValue,
    proto: &JSValue,
    proto_flags: u32,
    ctor_flags: u32,
) {
    func_obj
        .define_property_value(ctx, JS_ATOM_prototype, proto.clone(), proto_flags)
        .unwrap();

    proto
        .define_property_value(ctx, JS_ATOM_constructor, func_obj.clone(), ctor_flags)
        .unwrap();

    set_cycle_flag(ctx, func_obj);
    set_cycle_flag(ctx, proto);
}

pub fn JS_SetConstructor(ctx: &JSContext, func_obj: &JSValue, proto: &JSValue) {
    JS_SetConstructor2(
        ctx,
        func_obj,
        proto,
        0,
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    );
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

impl JSContext {
    /// create new instance based on class constructor `ctor` and isa payload data.
    /// `ctor` is not consumed.
    pub fn create_instance_from_ctor(
        &self,
        ctor: &JSValue,
        isa: JSObjectIsa,
    ) -> Result<JSValue, JSObjectIsa> {
        let class_id = isa.class_id();

        let proto = if ctor.is_undefined() {
            self.0.borrow().class_proto[class_id as usize].clone()
        } else {
            if let Ok(p) = ctor.get_property(self, JS_ATOM_prototype).check_exception() {
                if !p.is_object() {
                    if let Ok(realm) = JS_GetFunctionRealm(self, &ctor) {
                        realm.0.borrow().class_proto[class_id as usize].clone()
                    } else {
                        return Err(isa);
                    }
                } else {
                    p
                }
            } else {
                return Err(isa);
            }
        };

        self.create_instance_from_proto(&proto, isa)
    }
}

#[deprecated(note = "use JSContext::create_instance_with_ctor() instead")]
pub unsafe fn js_create_from_ctor(ctx: &JSContext, ctor: JSValue, class_id: JSClassID) -> JSValue {
    unreachable!("unused");
}

fn js_arguments_define_own_property(
    ctx: &JSContext,
    this_obj: &JSValue,
    prop: JSAtom,
    val: &JSValue,
    getter: &JSValue,
    setter: &JSValue,
    flags: i32,
) -> i32 {
    let p = unsafe { this_obj.object_unchecked() };

    /* convert to normal array when redefining an existing numeric field */
    if p.flags().fast_array()
        && prop
            .to_array_index(ctx.rt())
            .map_or(false, |i| i < p.array_length_unchecked())
    {
        if unsafe { convert_fast_array_to_array(ctx, p) } != 0 {
            return -1;
        }
    }

    /* run the default define own property */
    this_obj
        .define_property(
            ctx,
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
