// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{ffi::c_void, ptr::NonNull};

use gc_lite::{GcNode, GcTrace, GcTraceCtx};

use crate::{
    FieldDef, USER_CLASS_ID_START,
    atom::{Atom, AtomKind},
    builtins::iterator::{js_async_iterator_proto_funcs, js_iterator_proto_funcs},
    class::{ClassDef, ClassID},
    context::{ContextImpl, ProtoCtorPair, ScopedContext},
    error::ERR_TOO_MANY_CLASSES,
    function::{Callback, CallbackFn, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, ObjectImpl, ObjectPayload},
    runtime::RuntimeImpl,
    value::JSValue,
};

#[derive(Debug)]
pub struct OtherPayload {
    pub(crate) class_id: ClassID,
    pub(crate) opaque: *mut c_void,
}

impl GcTrace for OtherPayload {
    #[inline(always)]
    fn trace(&self, _: &mut GcTraceCtx) {}
}

impl OtherPayload {
    pub(crate) fn new(class_id: ClassID, opaque: *mut c_void) -> Box<Self> {
        debug_assert!(class_id >= USER_CLASS_ID_START);
        Box::new(Self { class_id, opaque })
    }

    #[inline(always)]
    pub const fn opaque(&self) -> *mut c_void {
        self.opaque
    }

    #[inline(always)]
    pub const unsafe fn replace_opaque(&mut self, opaque: *mut c_void) -> *mut c_void {
        std::mem::replace(&mut self.opaque, opaque)
    }
}

impl ObjectImpl {
    /// if the object is a user class instance, returns the opaque pointer if available
    pub fn opaque(&self) -> Option<NonNull<c_void>> {
        if let ObjectPayload::OTHER(u) = self.payload() {
            NonNull::new(u.opaque)
        } else {
            None
        }
    }

    /// if the object is a user class instance, returns the opaque pointer reference if available
    pub fn opaque_ref<T>(&self) -> Option<&T> {
        if let ObjectPayload::OTHER(u) = self.payload() {
            unsafe { u.opaque.cast::<T>().as_ref() }
        } else {
            None
        }
    }
}

impl JSValue {
    /// if the value is a user class instance, returns the opaque pointer if available
    pub fn opaque(&self) -> Option<NonNull<c_void>> {
        self.as_object()?.opaque()
    }

    pub fn opaque_ref<T>(&self) -> Option<&T> {
        self.as_object()?.opaque_ref::<T>()
    }
}

impl RuntimeImpl {
    fn register_class_internal(
        &mut self,
        class_id: ClassID,
        class_def: ClassDef,
    ) -> Result<(), i32> {
        debug_assert!(class_id > 0);
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
                return Err(-1);
            }
        } else if class_id as usize >= self.class_registry.len() {
            let extra = class_id as usize - self.class_registry.len() + 1;
            self.class_registry.try_reserve(extra).map_err(|_| -1)?;
            self.class_registry
                .extend(std::iter::repeat_with(ClassDef::default).take(extra));
        }

        self.class_registry[class_id as usize] = class_def;
        Ok(())
    }

    pub(crate) fn register_builtin_classes(
        &mut self,
        tab: &[ClassDef],
        start_id: ClassID,
    ) -> Result<(), i32> {
        debug_assert!(start_id > 0);
        let mut class_id = start_id;

        for item in tab {
            self.register_class_internal(class_id, *item)?;
            class_id += 1;
        }

        Ok(())
    }

    /// Register a user-defined (non-builtin) class
    pub fn register_class(&mut self, mut class_def: ClassDef) -> Result<ClassID, i32> {
        debug_assert!(!class_def.name.is_empty());
        debug_assert!(class_def.finalize.is_some());

        class_def.name_atom = {
            let a = Atom::from_str(self.into(), class_def.name, AtomKind::String)?;
            let s = a.descriptor(self).gc_head_ptr();
            self.gc_heap_mut().set_root_node(s); // class name atom is root gc node
            a
        };

        // find next class serial
        let class_id = self.user_class_id_serial;
        debug_assert!(class_id >= USER_CLASS_ID_START);
        if class_id == u16::MAX {
            self.set_exception(JSValue::Int(ERR_TOO_MANY_CLASSES));
            eprintln!("internal error: too many classes registered");
            return Err(-1);
        }
        self.user_class_id_serial = class_id + 1;

        self.register_class_internal(class_id, class_def)?;
        Ok(class_id)
    }

    pub fn is_class_registered(&self, class_id: ClassID) -> bool {
        class_id < self.class_registry.len() as ClassID
            && !self.class_registry[class_id as usize].name_atom.is_null()
    }

    pub(crate) fn query_class_id(&self, class_name: Atom) -> Option<ClassID> {
        self.class_registry
            .iter()
            .enumerate()
            .find(|(_, x)| !x.name_atom.is_null() && x.name_atom == class_name)
            .map(|(i, _)| i as ClassID)
    }
}

impl ContextImpl {}

impl<'c> ScopedContext<'c> {
    pub(crate) fn init_class_proto_internal(
        &mut self,
        class_id: ClassID,
        mut proto: Object,
        entries: &[FieldDef],
    ) -> Result<(), i32> {
        debug_assert!(self.class_slots.len() > class_id as usize);
        debug_assert!(
            self.class_slots[class_id as usize].proto.is_none(),
            "class proto has been created"
        );
        proto.install_fields(self, entries)?;
        self.class_slots[class_id as usize].proto = Some(proto);
        Ok(())
    }

    pub(crate) fn init_class_proto(
        &mut self,
        class_id: ClassID,
        entries: &[FieldDef],
    ) -> Result<Object, i32> {
        let p = self.create_base_object()?;
        self.init_class_proto_internal(class_id, p, entries)?;
        Ok(p)
    }

    pub(super) fn init_class_ctor_internal(
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

        let mut ctor: Object = NativeFunction::create(
            self,
            handler,
            class_name,
            length as _,
            magic,
            self.function_proto(),
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

    #[inline]
    pub fn bind_ctor_proto(&mut self, ctor: Object, proto: Object) -> Result<(), i32> {
        self.bind_ctor_proto_ex(ctor, proto, 0, JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE)
    }

    pub(crate) fn iterator_proto(&mut self) -> Result<Object, i32> {
        if let Some(o) = self.iterator_proto {
            Ok(o)
        } else {
            let mut o = self.create_base_object()?;
            o.install_fields(self, js_iterator_proto_funcs)?;
            self.iterator_proto.replace(o);
            Ok(o)
        }
    }

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

    pub fn use_classes(&mut self, class_ids: &[ClassID]) -> Result<(), i32> {
        if let Some(mx) = class_ids.iter().max().copied() {
            if self.rt().class_registry.get(mx as usize).is_some() {
                let n = self.class_slots.len();
                if mx as usize >= n {
                    // grow slot
                    let extra = mx as usize - n + 1;
                    self.class_slots.try_reserve_exact(extra).map_err(|_| {
                        self.throw_out_of_memory();
                        -1
                    })?;
                    self.class_slots
                        .extend(std::iter::repeat_n(ProtoCtorPair::default(), extra));
                }

                for id in class_ids {
                    let class_id = *id;

                    if let Some(class_def) =
                        self.rt().class_registry.get(class_id as usize).copied()
                    {
                        class_def.initialize(self, class_id)?;
                    } else {
                        self.throw_internal_error(&format!("unknown class id {class_id}"));
                        return Err(-1);
                    }
                }

                Ok(())
            } else {
                self.throw_internal_error(&format!("unknown class id {mx}"));
                Err(-1)
            }
        } else {
            Ok(())
        }
    }

    pub fn use_class(&mut self, class_id: ClassID) -> Result<(), i32> {
        self.use_classes(&[class_id])
    }

    pub fn use_class_name(&mut self, name: &str) -> Result<(), i32> {
        let atom_name = Atom::from_str(self.rt_ptr(), name, AtomKind::String)?;

        if let Some(id) = self
            .rt_ptr()
            .class_registry
            .iter()
            .position(|x| !x.name_atom.is_null() && x.name_atom == atom_name)
        {
            self.use_class(id as ClassID)
        } else {
            self.throw_internal_error(&format!("class '{}' was not registererd", name));
            Err(-1)
        }
    }

    /// create a new class instance
    pub fn new_instance(
        &mut self,
        class_id: ClassID,
        ctor: JSValue,
        opaque: *mut c_void,
    ) -> Result<Object, i32> {
        self.create_object_from_ctor(
            ctor,
            ObjectPayload::OTHER(Box::new(OtherPayload { class_id, opaque })),
        )
        .map_err(|_| -1)
    }
}
