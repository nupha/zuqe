use crate::{
    atom::{
        AtomTypeHint, JS_ATOM_NULL, JSAtom,
        consts::{JS_ATOM_constructor, JS_ATOM_prototype},
    },
    context::JSContext,
    error::JSResult,
    function::{JSFunctionDef, NativeCallback, NativeFunctionType},
    gc::set_cycle_flag,
    object::{
        JS_PROP_CONFIGURABLE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE, JSObject, JSPropertyDescriptor,
        ObjectPayload, PropertyKey,
    },
    runtime::JSRuntime,
    value::JSValue,
};

pub const JS_DEFINE_CLASS_HAS_HERITAGE: i32 = 1 << 0;

pub const OP_DEFINE_METHOD_METHOD: u8 = 0;
pub const OP_DEFINE_METHOD_GETTER: u8 = 1;
pub const OP_DEFINE_METHOD_SETTER: u8 = 2;
pub const OP_DEFINE_METHOD_ENUMERABLE: u8 = 4;

pub type JSClassID = u32;

/// instance finalizer callback, obj is the object payload.
pub type JSClassFinalizer = fn(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize);

pub type JSClassGCMark = fn(val: JSValue);

pub type ClassCallFn = fn(
    ctx: &JSContext,
    func_obj: &JSValue,
    this_val: &JSValue,
    args: &[JSValue],
    flags: i32,
) -> JSResult;

pub type DefineOwnPropertyFn = fn(
    ctx: &JSContext,
    target: &JSValue,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32>;

pub type HasPropertyFn = fn(ctx: &JSContext, target: &JSObject, prop: JSAtom) -> Result<bool, i32>;

/// TODO: change target type to &JSObject
pub type GetOwnPropertyFn =
    fn(ctx: &JSContext, target: JSValue, prop: JSAtom) -> Result<Option<JSPropertyDescriptor>, i32>;

pub type GetOwnPropertyNamesFn =
    fn(ctx: &JSContext, target: &JSObject) -> Result<Box<[PropertyKey]>, i32>;

pub type GetPropertyFn =
    fn(ctx: &JSContext, target: JSValue, prop: JSAtom, this_obj: JSValue) -> JSResult;

pub type SetPropertyFn = fn(
    ctx: &JSContext,
    target: JSValue,
    prop: JSAtom,
    val: JSValue,
    this_obj: JSValue,
    flags: i32,
) -> Result<bool, i32>;

/// TODO: change target type to &JSObject
pub type DeletePropertyFn = fn(ctx: &JSContext, target: JSValue, prop: JSAtom) -> Result<bool, i32>;

#[derive(Copy, Clone, Default)]
pub struct JSClassExoticMethods {
    pub get_own_property: Option<GetOwnPropertyFn>,
    pub get_own_property_names: Option<GetOwnPropertyNamesFn>,
    pub delete_property: Option<DeletePropertyFn>,
    pub define_own_property: Option<DefineOwnPropertyFn>,
    pub has_property: Option<HasPropertyFn>,
    pub get_property: Option<GetPropertyFn>,
    pub set_property: Option<SetPropertyFn>,
}

pub struct JSClassShortDef {
    pub class_name: JSAtom,
    pub finalizer: Option<JSClassFinalizer>,
    pub gc_mark: Option<JSClassGCMark>,
}

#[derive(Copy, Clone)]
pub struct JSClassDef {
    pub finalizer: Option<JSClassFinalizer>,
    pub gc_mark: Option<JSClassGCMark>,

    /// callback handler when called
    pub call: Option<ClassCallFn>,
    /// exotic method handlers
    pub exotic: *const JSClassExoticMethods,
}

impl Default for JSClassDef {
    fn default() -> Self {
        Self {
            finalizer: None,
            gc_mark: None,
            call: None,
            exotic: std::ptr::null(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct JSClass {
    /// 0 means free entry
    pub class_id: JSClassID,

    pub class_name: JSAtom,
    pub finalizer: Option<JSClassFinalizer>,
    pub gc_mark: Option<JSClassGCMark>,

    /// native callback when called
    pub call: Option<ClassCallFn>,

    /// pointers for exotic behavior, can be NULL if none are present
    pub exotic: *const JSClassExoticMethods,
}

impl JSRuntime {
    /// create a new object internal class. The finalizer can be NULL if none is needed.
    /// `name` is not consumed.
    pub(crate) fn add_class_internal(
        &mut self,
        class_id: JSClassID,
        class_def: &JSClassDef,
        name: JSAtom,
    ) -> Result<(), i32> {
        if class_id >= (1 << 16) {
            return Err(-1);
        }

        if (class_id as usize) < self.class_defs.len() {
            if self.class_defs[class_id as usize].class_id != 0 {
                return Err(-1); // class_id slot is in use
            }
        } else {
            if class_id as usize >= self.class_defs.len() {
                // number of extra slots to grow
                let grow = class_id as usize - self.class_defs.len() + 1;

                // grow context class prototype array
                for ctx in self.context_list.iter() {
                    let mut cx = ctx.0.borrow_mut();
                    cx.class_proto.try_reserve(grow).map_err(|_| -1)?;
                    cx.class_proto
                        .extend(std::iter::repeat_n(JSValue::NULL, grow));
                }

                // grow class array, fill slots with empty class entries
                self.class_defs.try_reserve(grow).map_err(|_| -1)?;
                for _ in 0..grow {
                    self.class_defs.push(JSClass {
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

        self.class_defs[class_id as usize] = JSClass {
            class_id,
            class_name: name.dup(self),
            finalizer: class_def.finalizer,
            gc_mark: class_def.gc_mark,
            call: class_def.call,
            exotic: class_def.exotic,
        };

        Ok(())
    }

    /// register a class, with given class_id and class name str.
    pub(crate) fn add_class(
        &mut self,
        class_id: JSClassID,
        class_def: &JSClassDef,
        name: &str,
    ) -> Result<(), i32> {
        let name = if let Some(a) = JSAtom::find(self, name, AtomTypeHint::String) {
            a
        } else {
            JSAtom::from_str(self, name, AtomTypeHint::String)?
        }
        .into_rc(self);
        self.add_class_internal(class_id, class_def, *name)
    }

    /// add multiple class defs.
    pub(crate) fn add_classes(
        &mut self,
        tab: &[JSClassShortDef],
        start: JSClassID,
    ) -> Result<(), i32> {
        let mut class_id = start;

        for item in tab {
            let cd = JSClassDef {
                exotic: std::ptr::null(),
                finalizer: item.finalizer,
                gc_mark: item.gc_mark,
                call: None,
            };
            self.add_class_internal(class_id, &cd, item.class_name)?;
            class_id += 1;
        }

        Ok(())
    }

    #[inline(always)]
    pub fn is_class_registered(&self, class_id: JSClassID) -> bool {
        class_id < self.class_defs.len() as JSClassID
            && self.class_defs[class_id as usize].class_id != 0
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

        self.0.borrow_mut().class_proto[class_id as usize] = proto_obj;
    }

    /// mark function `func_obj` to be a constructor, set `proto` as its prototype,
    /// then add it to global namespace as `name`.
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
    /// returns new constructor function.
    fn create_global_constructor_internal(
        &self,
        name: &str,
        handler: NativeFunctionType,
        length: i32,
        proto: &JSValue,
        magic: i16,
    ) -> JSResult {
        debug_assert!(matches!(
            handler,
            NativeFunctionType::Constructor(_) | NativeFunctionType::ConstructorOrFunc(_)
        ));

        let func_obj = self.create_native_function(handler, name, length as _, magic)?;
        self.add_global_constructor(name, &func_obj, proto);

        Ok(func_obj)
    }

    /// create a new native constructor, add it to global namespace.
    /// returns new constructor function.
    #[inline(always)]
    pub fn create_global_constructor_or_func(
        &self,
        name: &str,
        callback: NativeCallback,
        length: i32,
        proto: &JSValue,
        magic: i16,
    ) -> JSResult {
        self.create_global_constructor_internal(
            name,
            NativeFunctionType::ConstructorOrFunc(callback),
            length,
            proto,
            magic,
        )
    }

    /// create a new native constructor, add it to global namespace.
    /// returns new constructor function.
    #[inline(always)]
    pub fn create_global_constructor_only(
        &self,
        name: &str,
        callback: NativeCallback,
        length: i32,
        proto: &JSValue,
        magic: i16,
    ) -> JSResult {
        self.create_global_constructor_internal(
            name,
            NativeFunctionType::Constructor(callback),
            length,
            proto,
            magic,
        )
    }
}

/// Mutually define properties on both `func_obj` and `proto` side:
/// - Define property `func_obj.prototype = proto`
/// - Define property `proto.constructor = func_obj`.
///
/// Note: The `func_obj` may not necessarily be  a constructor
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

#[derive(Debug)]
pub struct ClassFieldsDef {
    pub fields_init_fd: *mut JSFunctionDef,
    pub computed_fields_count: i32,
    pub need_brand: bool,
    pub brand_push_pos: i32,
    pub is_static: bool,
}

impl Drop for ClassFieldsDef {
    fn drop(&mut self) {
        if !self.fields_init_fd.is_null() {
            drop(Box::from(self.fields_init_fd));
        }
    }
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

#[deprecated(note = "use JSContext::create_instance_with_ctor() instead")]
pub unsafe fn js_create_from_ctor(ctx: &JSContext, ctor: JSValue, class_id: JSClassID) -> JSValue {
    unreachable!("unused");
}

/// prop is not consumed
fn js_arguments_define_own_property(
    ctx: &JSContext,
    this_obj: &JSValue,
    prop: JSAtom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let p = this_obj.as_object().unwrap();

    // convert to normal array when redefining an existing numeric field
    if p.flags().is_fast_array()
        && prop
            .to_array_index(ctx.rt())
            .map_or(false, |i| i < p.fast_array_count())
    {
        p.destruct_fast_array(ctx)?;
    }

    // run the default define own property
    this_obj.define_property(
        ctx,
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
