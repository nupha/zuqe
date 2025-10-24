use crate::{
    atom::{
        Atom,
        consts::{JS_ATOM_constructor, JS_ATOM_prototype},
    },
    class::{
        ClassCallFn, ClassDef, ClassFinalizeFn, ClassGCMarkFn, ClassID, ClassInitializeFn,
        FreeObjFn, JSClassExoticMethods, StdClassDef,
    },
    context::JSContext,
    error::JSResult,
    function::{NativeCallback, NativeFunctionType},
    gc::set_cycle_flag,
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE},
    runtime::JSRuntime,
    value::JSValue,
};

/// class meta info for registry.
#[derive(Default)]
pub(crate) struct ClassMeta {
    /// NULL atom indicates this class is not initialized
    pub class_name: Atom,

    /// callback handler when called
    pub call: Option<ClassCallFn>,

    /// exotic property method handlers
    pub exotic: Option<Box<JSClassExoticMethods>>,

    /// free user class instance handler
    pub free_obj: Option<FreeObjFn>,

    /// initialize class proto, constructor to context
    pub initialize: Option<ClassInitializeFn>,
    pub finalize: Option<ClassFinalizeFn>,

    pub gc_mark: Option<ClassGCMarkFn>,
}

impl JSRuntime {
    /// create a new object internal class. The finalize can be NULL if none is needed.
    pub(super) fn add_class_internal(
        &mut self,
        class_id: ClassID,
        class_def: &ClassDef,
        class_name: Atom,
    ) -> Result<(), i32> {
        // builtin class name allow duplication, don't check.
        // if self
        //     .class_registry
        //     .iter()
        //     .any(|c| c.class_name.eq(&class_name))
        // {
        //     println!("class name already exists: '{}'", class_name.to_utf8());
        //     return Err(-1);
        // }

        if (class_id as usize) < self.class_registry.len() {
            if !self.class_registry[class_id as usize].class_name.is_null() {
                return Err(-1); // class_id slot is in use
            }
        } else if class_id as usize >= self.class_registry.len() {
            // grow slot
            let extra = class_id as usize - self.class_registry.len() + 1;

            // grow context class prototype array
            for ctx in self.context_list.iter() {
                let mut cx = ctx.0.borrow_mut();
                cx.class_proto.try_reserve(extra).map_err(|_| -1)?;
                cx.class_proto
                    .extend(std::iter::repeat_n(JSValue::NULL, extra));
            }

            // grow class array, fill slots with empty class
            self.class_registry.try_reserve(extra).map_err(|_| -1)?;
            self.class_registry
                .extend(std::iter::repeat_with(ClassMeta::default).take(extra));
        }

        self.class_registry[class_id as usize] = ClassMeta {
            class_name,

            call: class_def.call,
            exotic: class_def.exotic.as_ref().cloned(),
            free_obj: class_def.free_obj,

            initialize: class_def.initialize,
            finalize: class_def.finalize,
            gc_mark: class_def.gc_mark,
        };

        Ok(())
    }

    /// initialize multiple class short defs. used for builtin class defs only
    pub(crate) fn init_class_range(
        &mut self,
        tab: &[StdClassDef],
        start_id: ClassID,
    ) -> Result<(), i32> {
        debug_assert!(start_id > 0);

        let mut class_id = start_id;
        for item in tab {
            let cd = ClassDef {
                finalize: item.finalize,
                gc_mark: item.gc_mark,
                ..Default::default()
            };
            self.add_class_internal(class_id, &cd, Atom::new_const(item.class_name))?;
            class_id += 1;
        }

        Ok(())
    }

    pub(crate) fn is_class_registered(&self, class_id: ClassID) -> bool {
        class_id < self.class_registry.len() as ClassID
            && !self.class_registry[class_id as usize].class_name.is_null()
    }
}

impl JSContext {
    /// get class proto object by `class_id`.
    /// if class proto object is not initialized in this context, returns JS_NULL.
    /// returned proto value is duplicated. caller should free it.
    pub fn get_class_proto(&self, class_id: ClassID) -> JSValue {
        debug_assert!((class_id as usize) < self.0.borrow().class_proto.len());
        self.0.borrow().class_proto[class_id as usize].clone()
    }

    pub fn set_class_proto(&self, class_id: ClassID, proto_obj: JSValue) {
        debug_assert!(class_id < self.0.borrow().class_proto.len() as ClassID);
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

        self.bind_ctor_proto(func_obj, proto).unwrap();
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

    /// Mutually define properties on both `ctor` and `proto` side:
    /// - Define property `ctor.prototype = proto`
    /// - Define property `proto.constructor = ctor`.
    ///
    /// Note: The `ctor` may not necessarily be a constructor
    pub(crate) fn bind_ctor_proto_ex(
        &self,
        ctor: &JSValue,
        proto: &JSValue,
        proto_flags: u32,
        ctor_flags: u32,
    ) -> Result<(), i32> {
        ctor.define_property_value(self, &JS_ATOM_prototype, proto.clone(), proto_flags)?;
        proto.define_property_value(self, &JS_ATOM_constructor, ctor.clone(), ctor_flags)?;
        set_cycle_flag(self, ctor);
        set_cycle_flag(self, proto);
        Ok(())
    }

    /// Mutually define properties on both `ctor` and `proto` side:
    /// - Define property `ctor.prototype = proto`
    /// - Define property `proto.constructor = ctor`.
    ///
    /// Note: The `ctor` may not necessarily be a constructor
    #[inline(always)]
    pub fn bind_ctor_proto(&self, ctor: &JSValue, proto: &JSValue) -> Result<(), i32> {
        self.bind_ctor_proto_ex(ctor, proto, 0, JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
    }
}
