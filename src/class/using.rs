// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    FieldDef,
    atom::Atom,
    builtins::iterator::{js_async_iterator_proto_funcs, js_iterator_proto_funcs},
    class::{ClassDef, ClassID, static_class_count},
    context::{ProtoCtorPair, ScopedContext},
    function::{Callback, CallbackFn, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object},
    runtime::RuntimeImpl,
    value::JSValue,
};

impl RuntimeImpl {
    /// Look up a `ClassDef` by class ID (could be static or dynamic class def).
    #[inline(always)]
    pub(crate) fn class_def(&self, cid: ClassID) -> Option<&ClassDef> {
        if cid == 0 {
            return None;
        }

        let base = static_class_count();
        let cid_usize = cid as usize;

        #[cfg(feature = "zapi")]
        if cid_usize < base {
            crate::class::get_static_class_def(cid)
        } else {
            self.zapi_class_defs.get(cid_usize - base).map(|c| &c.def)
        }
        #[cfg(not(feature = "zapi"))]
        {
            debug_assert!(
                cid_usize < base,
                "class id {cid} is out of range (static count {base})"
            );
            crate::class::get_static_class_def(cid)
        }
    }
}

impl<'c> ScopedContext<'c> {
    /// The one way a class extends another one.
    ///
    /// Whatever kind a class is — defined in Rust (a builtin, or one an
    /// extension registered) or written as `class` in a script — its parent is
    /// established here and nowhere else: `child_proto` is re-prototyped onto
    /// `parent_proto`, and `child_ctor`, when both it and `parent_ctor` exist,
    /// onto `parent_ctor`. That is the whole of `class Child extends Parent`:
    /// instances reach the parent's prototype members through the first link,
    /// static members through the second.
    ///
    /// Every class is built as a *root* first (its prototype an ordinary object
    /// on `Object.prototype`, its constructor on `Function.prototype`) and then
    /// linked, so all three kinds link the same way. That is what lets a parent
    /// that only appears later — an extension class whose base registers after
    /// it — be linked when it does, and it is why the class builders take no
    /// parent: there is no second path that creates the relation.
    ///
    /// `parent_proto: None` is `class Child extends null`, whose prototype chain
    /// stops at the child. `parent_ctor: None` leaves the constructor on
    /// `Function.prototype`, which is what `extends null` wants; a child without
    /// a constructor of its own links nothing on that side. A class that extends
    /// nothing is simply never linked.
    pub(crate) fn link_class_extends(
        &mut self,
        child_proto: Object,
        child_ctor: Option<Object>,
        parent_proto: Option<Object>,
        parent_ctor: Option<Object>,
    ) -> Result<(), i32> {
        let mut child_proto = child_proto;
        // Re-linking costs nothing: a pass that runs again over classes that are
        // already linked — zapi re-runs its pending pass after every
        // registration — finds the chain already equal and `set_prototype`
        // returns early.
        child_proto.set_prototype(self, parent_proto.map_or(JSValue::NULL, JSValue::from))?;

        if let (Some(mut child_ctor), Some(parent_ctor)) = (child_ctor, parent_ctor) {
            child_ctor.set_prototype(self, JSValue::from(parent_ctor))?;
        }

        Ok(())
    }

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
        self.set_class_proto(class_id, proto);
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

    /// The class prototype of `class_id`, installed with `entries`, inheriting
    /// `parent_proto` — the shape the iterator classes (and every other class
    /// whose prototype is all it has) repeat. Built as a root and then linked,
    /// like every class in the engine: this is only the proto-side convenience,
    /// and `link_class_extends` below is still the one call that decides what
    /// inheriting means.
    pub(crate) fn init_class_proto_extends(
        &mut self,
        class_id: ClassID,
        parent_proto: Option<Object>,
        entries: &[FieldDef],
    ) -> Result<(), i32> {
        let proto = self.create_base_object()?;
        self.init_class_proto_internal(class_id, proto, entries)?;
        if let Some(parent_proto) = parent_proto {
            self.link_class_extends(proto, None, Some(parent_proto), None)?;
        }
        Ok(())
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

        let class_name = self
            .rt()
            .class_def(class_id)
            .map(|cd| cd.name_atom)
            .unwrap_or(Atom::NULL);

        // A root constructor: `link_class_extends` is what chains a derived one
        // onto its parent, so that no class is born with a parent.
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

    #[inline(always)]
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

    pub(crate) fn add_global_constructor(
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

    #[inline(always)]
    pub(crate) fn bind_ctor_proto(&mut self, ctor: Object, proto: Object) -> Result<(), i32> {
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
        let total = self.rt().class_count();
        if let Some(mx) = class_ids.iter().max().copied() {
            if (mx as usize) < total {
                let n = self.class_slots.len();
                if mx as usize >= n {
                    // grow slot
                    let extra = mx as usize - n + 1;
                    self.class_slots
                        .try_reserve_exact(extra)
                        .map_err(|_| self.throw_out_of_memory())?;
                    self.class_slots
                        .extend(std::iter::repeat_n(ProtoCtorPair::default(), extra));
                }

                for id in class_ids {
                    let class_id = *id;

                    if let Some(class_def) = self.rt().class_def(class_id).copied() {
                        class_def.initialize(self, class_id)?;
                    } else {
                        return Err(
                            self.throw_internal_error(&format!("unknown class id {class_id}"))
                        );
                    }
                }

                Ok(())
            } else {
                Err(self.throw_internal_error(&format!("unknown class id {mx}")))
            }
        } else {
            Ok(())
        }
    }

    #[inline(always)]
    pub fn use_class(&mut self, class_id: ClassID) -> Result<(), i32> {
        self.use_classes(&[class_id])
    }
}
