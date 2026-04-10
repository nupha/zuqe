// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::GcNode;

use crate::{
    FieldDef, JS_CLASS_ARRAY, JS_CLASS_BYTECODE_FUNCTION, JS_CLASS_OBJECT, JS_CLASS_PROMISE,
    JS_CLASS_REGEXP, ScopedContext, USER_CLASS_ID_START,
    atom::{Atom, AtomKind},
    builtins::{generator::js_async_iterator_proto_funcs, iterator::js_iterator_proto_funcs},
    class::{ClassDef, ClassID, ShortClassDef},
    context::{ContextImpl, ProtoCtorPair},
    function::{Callback, CallbackFn, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object},
    runtime::RuntimeImpl,
};

impl RuntimeImpl {
    /// register a class def to runtime.
    pub(super) fn register_class_internal(
        &mut self,
        class_id: ClassID,
        class_def: ClassDef,
    ) -> Result<(), i32> {
        debug_assert!(!class_def.name_atom.is_null());

        #[cfg(debug_assertions)]
        if class_id >= USER_CLASS_ID_START {
            for m in self.class_registry[USER_CLASS_ID_START as usize..].iter() {
                if m.name_atom == class_def.name_atom {
                    eprintln!(
                        "WARN: class name {:?} ('{}') already exists",
                        class_def.name_atom, class_def.name
                    );
                }
            }
        }

        if (class_id as usize) < self.class_registry.len() {
            if !self.class_registry[class_id as usize].name_atom.is_null() {
                return Err(-1); // class_id slot is in use
            }
        } else if class_id as usize >= self.class_registry.len() {
            // grow slot
            let extra = class_id as usize - self.class_registry.len() + 1;
            // grow context class prototype array
            for ctx in self.context_list.iter_mut() {
                ctx.class_slots.try_reserve(extra).map_err(|_| -1)?;
                ctx.class_slots
                    .extend(std::iter::repeat_n(ProtoCtorPair::default(), extra));
            }

            // grow class array, fill slots with empty class
            self.class_registry.try_reserve(extra).map_err(|_| -1)?;
            self.class_registry
                .extend(std::iter::repeat_with(ClassDef::default).take(extra));
        }

        self.class_registry[class_id as usize] = class_def;

        Ok(())
    }

    /// register builtin class defs.
    pub(crate) fn register_builtin_classes(
        &mut self,
        tab: &[ShortClassDef],
        start_id: ClassID,
    ) -> Result<(), i32> {
        debug_assert!(start_id > 0);
        let mut class_id = start_id;

        for item in tab {
            self.register_class_internal(
                class_id,
                ClassDef {
                    name_atom: item.class_name,
                    initialize: item.initialize,
                    finalize: item.finalize,
                    ..Default::default()
                },
            )?;
            class_id += 1;
        }

        Ok(())
    }

    /// Register an user-defined (non-builtin) class defination.
    /// returns the assigned class_id.
    pub fn register_class(&mut self, mut class_def: ClassDef) -> Result<ClassID, i32> {
        debug_assert!(!class_def.name.is_empty());
        debug_assert!(class_def.finalize.is_some());

        // convert class name str to atom
        class_def.name_atom = {
            let a = Atom::from_str(self.into(), class_def.name, AtomKind::String)?;
            let s = a.descriptor(self).gc_head_ptr();
            self.gc_heap_mut().set_root_node(s); // should be root gc node
            a
        };

        thread_local! {
            static CLASS_ID_SERIAL: Cell<u16> = const { Cell::new(USER_CLASS_ID_START) };
        }
        let class_id = CLASS_ID_SERIAL.with(|ser| {
            let i = ser.get();
            ser.set(i + 1);
            i
        });
        debug_assert!(class_id >= USER_CLASS_ID_START);

        self.register_class_internal(class_id, class_def)?;
        Ok(class_id)
    }

    /// test if class_id exists.
    pub fn is_class_registered(&self, class_id: ClassID) -> bool {
        class_id < self.class_registry.len() as ClassID
            && !self.class_registry[class_id as usize].name_atom.is_null()
    }

    /// query class id by class name
    pub fn query_class_id(&self, class_name: Atom) -> Option<ClassID> {
        self.class_registry
            .iter()
            .enumerate()
            .find(|(_, x)| !x.name_atom.is_null() && x.name_atom == class_name)
            .map(|(i, _)| i as ClassID)
    }
}

impl ContextImpl {
    /// get class proto object by `class_id`.
    /// if class proto object is not initialized in this context, returns None.
    pub fn class_proto(&self, class_id: ClassID) -> Option<Object> {
        self.class_slots
            .get(class_id as usize)
            .and_then(|x| x.proto)
    }

    /// get class constructor by `class_id`.
    /// if class constructor is not initialized in this context, returns None.
    pub fn class_constructor(&self, class_id: ClassID) -> Option<Object> {
        self.class_slots.get(class_id as usize).and_then(|x| x.ctor)
    }

    /// get base Object prototype, must be  as core.
    ///
    /// equivelent to call `ctx.class_proto(JS_CLASS_OBJECT).unwrap()`
    #[inline]
    pub fn object_proto(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_OBJECT as usize);
        self.class_proto(JS_CLASS_OBJECT).unwrap()
    }

    /// get class Function prototype.
    ///
    /// equivelent to `ctx.class_proto(JS_CLASS_BYTECODE_FUNCTION).unwrap()`
    #[inline]
    pub fn function_proto(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_BYTECODE_FUNCTION as usize);
        self.class_proto(JS_CLASS_BYTECODE_FUNCTION).unwrap()
    }

    /// get class Array prototype.
    ///
    /// equivelent to `ctx.class_proto(JS_CLASS_ARRAY).unwrap()`
    #[inline]
    pub fn array_proto(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_ARRAY as usize);
        self.class_proto(JS_CLASS_ARRAY).unwrap()
    }

    /// get class Array constructor.
    ///
    /// equivelent to `ctx.class_ctor(JS_ARRAY_FUNCTION).unwrap()`
    #[inline]
    pub fn array_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_ARRAY as usize);
        self.class_constructor(JS_CLASS_ARRAY).unwrap()
    }

    /// get class RegExp constructor.
    ///
    /// equivelent to `ctx.class_ctor(JS_CLASS_REGEXP).unwrap()`
    #[cfg(feature = "builtin_regexp")]
    #[inline]
    pub fn regexp_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_REGEXP as usize);
        self.class_constructor(JS_CLASS_REGEXP).unwrap()
    }

    /// get class Promise constructor.
    ///
    /// equivelent to `ctx.class_ctor(JS_ARRAY_PROMISE).unwrap()`
    #[inline]
    pub fn promise_ctor(&self) -> Object {
        debug_assert!(self.class_slots.len() > JS_CLASS_PROMISE as usize);
        self.class_constructor(JS_CLASS_PROMISE).unwrap()
    }
}

impl<'c> ScopedContext<'c> {
    /// Create class prototype (using base object), register to class slot.
    /// returns the proto object.
    pub fn init_class_proto(
        &mut self,
        class_id: ClassID,
        entries: &[FieldDef],
    ) -> Result<Object, i32> {
        debug_assert!(self.class_slots.len() > class_id as usize);
        debug_assert!(
            self.class_slots[class_id as usize].proto.is_none(),
            "class proto has been created"
        );

        let mut p = self.create_base_object()?;
        p.install_fields(self, entries)?;
        self.class_slots[class_id as usize].proto = Some(p);
        Ok(p)
    }

    /// Create class constructor, register to class ctor slot.
    /// returns the constructor function.
    fn init_class_ctor_internal(
        &mut self,
        class_id: ClassID,
        handler: Callback,
        length: i32,
        magic: i16,
        proto: Object,
        entries: &[FieldDef],
    ) -> Result<Object, i32> {
        debug_assert!(self.class_slots.len() > class_id as usize);
        debug_assert!(
            self.class_slots[class_id as usize].ctor.is_none(),
            "class ctor has been created"
        );

        let class_name = self.rt().class_registry[class_id as usize].name_atom;

        let mut ctor: Object = NativeFunction::create_internal(
            self,
            handler,
            class_name,
            length as _,
            magic,
            Some(self.function_proto()),
            None,
        )?
        .into();

        if !entries.is_empty() {
            ctor.install_fields(self, entries)?;
        }
        self.bind_ctor_proto(ctor, proto)?;

        self.class_slots[class_id as usize].ctor = Some(ctor);
        Ok(ctor)
    }

    /// Create class constructor, register to class slot.
    /// returns the constructor function.
    pub fn init_class_constructor(
        &mut self,
        class_id: ClassID,
        callback: CallbackFn,
        length: i32,
        magic: i16,
        proto: Object,
        entries: &[FieldDef],
    ) -> Result<Object, i32> {
        self.init_class_ctor_internal(
            class_id,
            Callback::Constructor(callback),
            length,
            magic,
            proto,
            entries,
        )
    }

    /// Create class constructor, register to class slot.
    /// returns the constructor function.
    pub fn init_class_constructor_or_func(
        &mut self,
        class_id: ClassID,
        callback: CallbackFn,
        length: i32,
        magic: i16,
        proto: Object,
        entries: &[FieldDef],
    ) -> Result<Object, i32> {
        self.init_class_ctor_internal(
            class_id,
            Callback::ConstructorOrFunc(callback),
            length,
            magic,
            proto,
            entries,
        )
    }

    /// create native constructor, optinally add it to global namespace.
    /// returns new constructor function.
    pub(crate) fn create_constructor_internal(
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

    /// create native constructor, optinally add it to global namespace.
    /// returns new constructor function.
    pub fn create_constructor(
        &mut self,
        name: &str,
        callback: CallbackFn,
        length: i32,
        proto: Object,
        magic: i16,
        add_to_global: bool,
    ) -> Result<Object, i32> {
        self.create_constructor_internal(
            name,
            Callback::Constructor(callback),
            length,
            proto,
            magic,
            add_to_global,
        )
    }

    /// create native constructor, optinally add it to global namespace.
    /// returns new constructor function.
    pub fn create_constructor_or_func(
        &mut self,
        name: &str,
        callback: CallbackFn,
        length: i32,
        proto: Object,
        magic: i16,
        add_to_global: bool,
    ) -> Result<Object, i32> {
        self.create_constructor_internal(
            name,
            Callback::ConstructorOrFunc(callback),
            length,
            proto,
            magic,
            add_to_global,
        )
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

    /// initialize iterator proto object
    pub fn iterator_proto(&mut self) -> Result<Object, i32> {
        let obj = if let Some(o) = self.iterator_proto.as_ref().cloned() {
            o
        } else {
            let mut o = self.create_base_object()?;
            o.install_fields(self, js_iterator_proto_funcs)?;
            self.iterator_proto.replace(o);
            o
        };

        Ok(obj)
    }

    /// Initialize async iterator proto object which is used as proto of:
    /// AsyncFromSyncIterator, AsyncGenerator.
    /// if this proto is not initialized, will create it inside.
    pub(crate) fn async_iterator_proto(&mut self) -> Result<Object, i32> {
        if let Some(o) = self.async_iterator_proto {
            Ok(o)
        } else {
            let mut o = self.create_base_object()?;
            o.install_fields(self, js_async_iterator_proto_funcs)?;
            self.async_iterator_proto.replace(o);
            Ok(o)
        }
    }
}
