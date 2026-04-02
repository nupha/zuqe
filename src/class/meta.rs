// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_OBJECT, ScopedContext,
    atom::Atom,
    builtins::iterator::js_iterator_proto_funcs,
    class::{
        ClassCallFn, ClassDef, ClassExoticMethods, ClassFinalizeFn, ClassID, ClassInitializeFn,
        FreeObjFn, StdClassDef,
    },
    context::{ContextImpl, ProtoCtorPair},
    error::JSResult,
    function::{Callback, CallbackFn},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object},
    runtime::RuntimeImpl,
    value::JSValue,
};

/// class meta info for registry.
#[derive(Default)]
pub(crate) struct ClassMeta {
    /// Atom::NULL indicates the class slot is not used
    pub(crate) class_name: Atom,

    /// callback handler when called
    pub(crate) call: Option<ClassCallFn>,

    /// exotic property method handlers
    pub(crate) exotic: Option<Box<ClassExoticMethods>>,

    /// handler to free instance of class
    pub(crate) free_obj: Option<FreeObjFn>,

    /// initialize class proto, constructor to context
    pub(crate) initialize: Option<ClassInitializeFn>,
    /// instance finalize handler
    pub(crate) finalize: Option<ClassFinalizeFn>,
}

impl RuntimeImpl {
    /// register a class def to runtime.
    pub(super) fn add_class_internal(
        &mut self,
        class_id: ClassID,
        class_name: Atom,
        class_def: &ClassDef,
    ) -> Result<(), i32> {
        if (class_id as usize) < self.class_registry.len() {
            if !self.class_registry[class_id as usize].class_name.is_null() {
                return Err(-1); // class_id slot is in use
            }
        } else if class_id as usize >= self.class_registry.len() {
            // grow slot
            let extra = class_id as usize - self.class_registry.len() + 1;

            // grow context class prototype array
            for ctx in self.context_list.iter_mut() {
                ctx.class_protos_ctors.try_reserve(extra).map_err(|_| -1)?;
                ctx.class_protos_ctors
                    .extend(std::iter::repeat_n(ProtoCtorPair::default(), extra));
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
            free_obj: class_def.finalize_opaque,

            initialize: class_def.initialize,
            finalize: class_def.finalize,
        };

        Ok(())
    }

    /// register standard class defs.
    pub(crate) fn register_builtin_classes(
        &mut self,
        tab: &[StdClassDef],
        start_id: ClassID,
    ) -> Result<(), i32> {
        debug_assert!(start_id > 0);

        let mut class_id = start_id;
        for item in tab {
            let cd = ClassDef {
                initialize: item.initialize,
                finalize: item.finalize,
                ..Default::default()
            };
            self.add_class_internal(class_id, item.class_name, &cd)?;
            class_id += 1;
        }

        Ok(())
    }

    /// test if class_id exists.
    pub fn is_class_registered(&self, class_id: ClassID) -> bool {
        class_id < self.class_registry.len() as ClassID
            && !self.class_registry[class_id as usize].class_name.is_null()
    }
}

impl ContextImpl {
    /// get class proto object by `class_id`.
    /// if class proto object is not initialized in this context, returns None.
    pub fn class_proto(&self, class_id: ClassID) -> Option<Object> {
        self.class_protos_ctors
            .get(class_id as usize)
            .and_then(|x| x.proto)
    }

    /// get base Object prototype, must be  as core.
    ///
    /// equivelent to call `ctx.class_proto(JS_CLASS_OBJECT).unwrap()`
    #[inline]
    pub fn object_proto(&self) -> Object {
        debug_assert!(self.class_protos_ctors.len() > JS_CLASS_OBJECT as usize);
        self.class_proto(JS_CLASS_OBJECT).unwrap()
    }

    /// get Function prototype.
    ///
    /// equivelent to call `ctx.class_proto(JS_CLASS_BYTECODE_FUNCTION).unwrap()`
    #[inline]
    pub fn function_proto(&self) -> Object {
        debug_assert!(self.class_protos_ctors.len() > JS_CLASS_BYTECODE_FUNCTION as usize);
        self.class_proto(JS_CLASS_BYTECODE_FUNCTION).unwrap()
    }

    /// get Array prototype, must be  as core.
    ///
    /// equivelent to call `ctx.class_proto(JS_ARRAY_FUNCTION).unwrap()`
    #[inline]
    pub fn array_proto(&self) -> Object {
        debug_assert!(self.class_protos_ctors.len() > JS_CLASS_ARRAY as usize);
        self.class_proto(JS_CLASS_ARRAY).unwrap()
    }
}

impl<'c> ScopedContext<'c> {
    /// initialize iterator proto object
    pub fn iterator_proto(&mut self) -> Result<Object, i32> {
        let obj = if let Some(o) = self.iterator_proto.as_ref().cloned() {
            o
        } else {
            let mut obj = self.create_base_object()?;
            obj.install_fields(self, js_iterator_proto_funcs)?;
            self.iterator_proto.replace(obj);
            obj
        };

        Ok(obj)
    }

    /// mark function `func_obj` to be a constructor, set `proto` as its prototype,
    /// then add it to global namespace as `name`.
    pub fn add_global_constructor(
        &mut self,
        name: &str,
        func_obj: Object,
        proto: Object,
    ) -> Result<(), i32> {
        self.bind_ctor_proto(func_obj, proto)?;
        self.global_obj().define_property_value_str(
            self,
            name,
            func_obj.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
        Ok(())
    }

    /// create native constructor, optinally add it to global namespace.
    /// returns new constructor function.
    pub(crate) fn create_constructor_ex(
        &mut self,
        name: &str,
        handler: Callback,
        length: i32,
        proto: Object,
        magic: i16,
        add_to_global: bool,
    ) -> Result<Object, i32> {
        debug_assert!(matches!(
            handler,
            Callback::Constructor(_) | Callback::ConstructorOrFunc(_)
        ));

        let func_obj = self.create_native_function(handler, name, length as _, magic)?;
        self.bind_ctor_proto(func_obj.into(), proto)?;

        if add_to_global {
            self.global_obj().define_property_value_str(
                self,
                name,
                func_obj.into(),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )?;
        }

        Ok(func_obj.into())
    }

    /// create a new native constructor, add it to global namespace.
    /// returns new constructor function.
    #[inline(always)]
    pub fn create_global_constructor_or_func(
        &mut self,
        name: &str,
        callback: CallbackFn,
        length: i32,
        proto: Object,
        magic: i16,
    ) -> JSResult {
        self.create_constructor_ex(
            name,
            Callback::ConstructorOrFunc(callback),
            length,
            proto,
            magic,
            true,
        )
        .map(JSValue::Object)
    }

    /// create a new native constructor, add it to global namespace.
    /// returns new constructor function.
    #[inline(always)]
    pub fn create_global_constructor(
        &mut self,
        name: &str,
        callback: CallbackFn,
        length: i32,
        proto: Object,
        magic: i16,
    ) -> JSResult {
        self.create_constructor_ex(
            name,
            Callback::Constructor(callback),
            length,
            proto,
            magic,
            true,
        )
        .map(JSValue::Object)
    }

    #[deprecated(note = "use ::create_global_constructor() instead")]
    pub fn create_global_constructor_only(
        &mut self,
        name: &str,
        callback: CallbackFn,
        length: i32,
        proto: Object,
        magic: i16,
    ) -> JSResult {
        self.create_global_constructor(name, callback, length, proto, magic)
    }

    /// Mutually define properties on both `ctor` and `proto` side:
    /// - Define property `ctor.prototype = proto`
    /// - Define property `proto.constructor = ctor`.
    ///
    /// Note: The `ctor` may not necessarily be a constructor
    pub(crate) fn bind_ctor_proto_ex(
        &mut self,
        mut ctor: Object,
        mut proto: Object,
        proto_flags: u32,
        ctor_flags: u32,
    ) -> Result<(), i32> {
        ctor.define_property_value(self, Atom::CONST_prototype, proto.into(), proto_flags)?;
        proto.define_property_value(self, Atom::CONST_constructor, ctor.into(), ctor_flags)?;
        Ok(())
    }

    /// Mutually define properties on both `ctor` and `proto` side:
    /// - Define property `ctor.prototype = proto`
    /// - Define property `proto.constructor = ctor`.
    ///
    /// Note: The `ctor` may not necessarily be a constructor
    #[inline]
    pub fn bind_ctor_proto(&mut self, ctor: Object, proto: Object) -> Result<(), i32> {
        self.bind_ctor_proto_ex(ctor, proto, 0, JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
    }
}
