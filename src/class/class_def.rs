// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use crate::{
    Callback, ClassID, FieldDef,
    atom::Atom,
    class::{
        ClassCallFn, ClassGcTraceFn, ClassInitializeFn, ClassUnregisterFn, GetPrototypeFn,
        SetPrototypeFn,
    },
    context::ScopedContext,
    function::{CallbackFn, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object},
};

use super::{
    DefineOwnPropertyFn, DeletePropertyFn, GetOwnPropertyFn, GetOwnPropertyNamesFn, GetPropertyFn,
    HasPropertyFn, SetPropertyFn,
};

/// class has extends inherits
pub(crate) const JS_DEFINE_CLASS_HAS_HERITAGE: i32 = 1 << 0;

#[derive(Copy, Clone, Default)]
pub struct ClassExoticMethods {
    pub get_own_property: Option<GetOwnPropertyFn>,
    pub get_own_property_names: Option<GetOwnPropertyNamesFn>,
    pub delete_property: Option<DeletePropertyFn>,
    pub define_own_property: Option<DefineOwnPropertyFn>,
    pub has_property: Option<HasPropertyFn>,
    pub get_property: Option<GetPropertyFn>,
    pub set_property: Option<SetPropertyFn>,

    pub get_prototype: Option<GetPrototypeFn>,
    pub set_prototype: Option<SetPrototypeFn>,
}

#[derive(Clone, Copy)]
pub struct ClassCtorDef {
    pub(crate) callback: Callback,
    pub(crate) length: i32,
    pub(crate) magic: i16,
    pub(crate) entries: &'static [FieldDef],
    pub(crate) export_to_global: bool,
}

impl ClassCtorDef {
    pub const fn new(callback: CallbackFn, length: i32, call_as_func: bool) -> Self {
        Self {
            callback: if call_as_func {
                Callback::ConstructorOrFunc(callback)
            } else {
                Callback::Constructor(callback)
            },
            length,
            magic: 0,
            entries: &[],
            export_to_global: false,
        }
    }

    /// Callable-as-function constructor that keeps the receiver (`this`) on
    /// plain calls — ECMA-402 legacy constructors
    /// (e.g. `Intl.DateTimeFormat.call(obj)`).
    pub const fn new_legacy(callback: CallbackFn, length: i32) -> Self {
        Self {
            callback: Callback::LegacyConstructorOrFunc(callback),
            length,
            magic: 0,
            entries: &[],
            export_to_global: false,
        }
    }

    pub const fn magic(mut self, magic: i16) -> Self {
        self.magic = magic;
        self
    }
    pub const fn entries(mut self, entries: &'static [FieldDef]) -> Self {
        self.entries = entries;
        self
    }

    /// expose constructor to global namespace
    pub const fn expose_to_global(mut self) -> Self {
        self.export_to_global = true;
        self
    }
}

#[derive(Clone, Copy)]
enum ClassInitType {
    Normal {
        proto_entries: &'static [FieldDef],
        /// Parent class id resolver (lazily resolved at initialize time, so the
        /// parent does not need to be registered before the child).
        extends_class: Option<fn() -> ClassID>,
        ctor: Option<ClassCtorDef>,
    },
    InitFn(ClassInitializeFn),
}

/// class definition
#[derive(Clone, Copy)]
pub struct ClassDef {
    /// class name str. Could be empty str for builtin classes
    pub(crate) name: &'static str,
    /// class name atom. Atom::NULL indicates the class slot is not used
    pub(crate) name_atom: Atom,

    /// native handler when class is called (for function-like instances)
    pub(crate) call: Option<ClassCallFn>,
    /// exotic property method handlers
    pub(crate) exotic: Option<&'static ClassExoticMethods>,

    /// initialize class proto, constructor to context
    on_initialize: ClassInitType,
    /// class instance finalize callback, called before Object::drop()
    pub(crate) on_finalize: Option<super::ClassFinalizeFn>,
    /// class def unregister handler.
    pub(crate) on_unregister: Option<ClassUnregisterFn>,
    /// to trace JSValues stored in an instance's payload (opaque)
    pub(crate) on_gc_trace: Option<ClassGcTraceFn>,

    /// owned-payload free channel, uniform for ALL classes: builtin classes
    /// are wired by the generator (`wire_builtin_payload_drops`, called right
    /// after `register_builtin_classes`), user classes by `def_native_object!`
    /// (monomorphized `drop_payload_fn::<P>`). `None` only for classes whose
    /// payload the engine does not own (zapi opaque pointers) or that have no
    /// payload — their payloads are never freed here. `finalize` looks the
    /// callback up by class_id and frees through it.
    pub(crate) payload_drop: Option<unsafe fn(*mut c_void)>,

    /// Debug-only type ledger: `type_name::<P>()` of the payload type this
    /// class expects. Set at registration time — by `build_stdclass.rs` for
    /// builtins, by `def_native_object!` for user classes. Used by
    /// `PayloadPtr::new` and `create_class_instance` to catch
    /// type mismatches early (prevents silent UB from wrong-type drops).
    #[cfg(debug_assertions)]
    pub(crate) payload_type_name: Option<&'static str>,
}

impl Default for ClassDef {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl ClassDef {
    /// Const-compatible default, usable in `const fn` constructors where
    /// `Default::default()` is not available.
    const DEFAULT: Self = Self {
        name: "",
        name_atom: Atom::NULL,
        call: None,
        exotic: None,

        on_initialize: ClassInitType::Normal {
            proto_entries: &[],
            ctor: None,
            extends_class: None,
        },
        on_finalize: None,
        on_unregister: None,
        on_gc_trace: None,

        payload_drop: None,

        #[cfg(debug_assertions)]
        payload_type_name: None,
    };
}

impl ClassDef {
    /// Create user-defined class def with proto entries and optional ctor def.
    ///
    /// All hooks start as `None`; use the builder methods (`.on_call()`,
    /// `.on_finalize()`, `.payload_drop()`, etc.) to set them explicitly.
    pub const fn new(
        name: &'static str,
        proto_entries: &'static [FieldDef],
        ctor: Option<ClassCtorDef>,
    ) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            on_initialize: ClassInitType::Normal {
                proto_entries,
                ctor,
                extends_class: None,
            },
            ..Self::DEFAULT
        }
    }

    /// Create class def with a pre-allocated name atom.
    ///
    /// Like `ClassDef::new()` but uses the provided `atom` directly instead of
    /// deriving it from `name` at registration time. Intended for builtin classes
    /// whose atoms are compile-time constants (`Atom::CONST_*`).
    pub const fn new_with_atom(
        name: &'static str,
        atom: Atom,
        proto_entries: &'static [FieldDef],
        ctor: Option<ClassCtorDef>,
    ) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            name_atom: atom,
            on_initialize: ClassInitType::Normal {
                proto_entries,
                ctor,
                extends_class: None,
            },
            ..Self::DEFAULT
        }
    }

    /// Create user-defined class def with proto entries, ctor and extends inheritance.
    /// `extends_class` is a resolver invoked lazily at initialize time, so the
    /// parent class need not be registered before the child.
    pub const fn new_extends(
        name: &'static str,
        proto_entries: &'static [FieldDef],
        ctor: Option<ClassCtorDef>,
        extends_class: fn() -> ClassID,
    ) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            on_initialize: ClassInitType::Normal {
                proto_entries,
                extends_class: Some(extends_class),
                ctor,
            },
            ..Self::DEFAULT
        }
    }

    /// Create user-defined class def with a custom initialization function.
    ///
    /// `init_fn` is called **once per engine context** when the class is first used
    /// via `use_class()` (never at registration time). It receives the target context
    /// and the registered `class_id`.
    ///
    /// The init function **must** populate `ctx.class_slots[class_id].proto` with a
    /// prototype object before returning `Ok(())`. Failure to do so will trigger a
    /// runtime assertion. Optionally it may also set `.ctor` if the class has a
    /// constructor.
    ///
    /// This path is intended for classes whose prototype (or constructor) cannot be
    /// described by a static `&[FieldDef]` — e.g. classes registered dynamically by
    /// native extensions (zapi), where the prototype layout is determined at runtime.
    pub const fn new_with(name: &'static str, init_fn: ClassInitializeFn) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            on_initialize: ClassInitType::InitFn(init_fn),
            ..Self::DEFAULT
        }
    }

    pub const fn exotic(mut self, exotic: &'static ClassExoticMethods) -> Self {
        self.exotic = Some(exotic);
        self
    }

    pub const fn on_call(mut self, call: ClassCallFn) -> Self {
        self.call = Some(call);
        self
    }

    /// Set class instance finalize callback. Called before payload is freed.
    ///
    /// Unified for both builtin and user classes. The callback receives the
    /// payload pointer, class_id, and object id.
    pub const fn on_finalize(mut self, f: super::ClassFinalizeFn) -> Self {
        self.on_finalize.replace(f);
        self
    }

    /// Set the free channel for owned user-class payloads.
    ///
    /// Typically wired automatically by `def_native_object!` with the
    /// monomorphized `drop_payload_fn::<P>`. Leave `None` for classes whose
    /// payload the engine does not own (zapi opaque pointers) or that have no
    /// payload at all.
    pub const fn payload_drop(mut self, f: unsafe fn(*mut c_void)) -> Self {
        self.payload_drop.replace(f);
        self
    }

    /// set class unregister callback
    pub const fn on_unregister(mut self, f: ClassUnregisterFn) -> Self {
        self.on_unregister.replace(f);
        self
    }

    /// Set GC mark callback for tracing JSValues in opaque payload.
    pub const fn on_gc_trace(mut self, f: ClassGcTraceFn) -> Self {
        self.on_gc_trace = Some(f);
        self
    }

    /// Set the expected payload type name for debug-only type checking.
    ///
    /// Used by `PayloadPtr::new` and `create_class_instance` to
    /// catch type mismatches early. Builtin classes get this from the
    /// generated `payload_type_name` function; user classes from the
    /// `def_native_object!` macro.
    #[cfg(debug_assertions)]
    pub const fn payload_type_name(mut self, name: Option<&'static str>) -> Self {
        self.payload_type_name = name;
        self
    }

    /// Perform class initialize in context.
    pub(super) fn initialize(&self, ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        match self.on_initialize {
            ClassInitType::Normal {
                proto_entries,
                extends_class,
                ctor,
            } => {
                if let Some(parent_resolver) = extends_class {
                    // Extends inheritance: set up prototype chain. Resolve the
                    // parent id lazily so registration order does not matter.
                    let parent_id = parent_resolver();

                    if ctx.class_slots[class_id as usize].proto.is_none() {
                        let parent_proto = ctx.class_proto(parent_id).unwrap();
                        let proto = ctx.create_base_object_with_proto(Some(parent_proto))?;
                        ctx.init_class_proto_internal(class_id, proto, proto_entries)?;
                    }

                    if let Some(c) = ctor
                        && ctx.class_slots[class_id as usize].ctor.is_none()
                    {
                        let class_name = ctx.rt().class_def(class_id).unwrap().name_atom;
                        let parent_ctor = ctx.class_constructor(parent_id).unwrap();

                        let proto = ctx.class_proto(class_id).unwrap();
                        let mut ctor: Object = NativeFunction::create(
                            ctx,
                            c.callback,
                            class_name,
                            c.length as _,
                            c.magic,
                            parent_ctor,
                            None,
                        )?
                        .into();
                        if !c.entries.is_empty() {
                            ctor.install_fields(ctx, c.entries)?;
                        }
                        ctx.bind_ctor_proto(ctor, proto)?;
                        ctx.class_slots[class_id as usize].ctor = Some(ctor);

                        if c.export_to_global {
                            ctx.global_obj().define_property_value(
                                ctx,
                                class_name,
                                ctor.into(),
                                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                            )?;
                        }
                    }
                } else {
                    if ctx.class_slots[class_id as usize].proto.is_none() {
                        let proto = ctx.init_class_proto(class_id, proto_entries)?;
                        debug_assert!(
                            ctx.class_slots[class_id as usize]
                                .proto
                                .is_some_and(|p| p == proto)
                        );
                    }

                    if let Some(c) = ctor
                        && ctx.class_slots[class_id as usize].ctor.is_none()
                    {
                        let proto = ctx.class_proto(class_id).unwrap();
                        let ctor = ctx.init_class_ctor_internal(
                            class_id, c.callback, c.length, c.magic, proto, c.entries,
                        )?;
                        if c.export_to_global {
                            ctx.global_obj().define_property_value(
                                ctx,
                                self.name_atom,
                                ctor.into(),
                                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                            )?;
                        }
                    }
                }
            }

            ClassInitType::InitFn(init_fn) => {
                if ctx.class_proto(class_id).is_none() {
                    init_fn(ctx, class_id)?;

                    if ctx.class_proto(class_id).is_none() {
                        let _ = ctx.throw_internal_error(&format!(
                            "class ({class_id}) initialization must setup class slot for proto"
                        ));
                    }
                }
            }
        }

        Ok(())
    }
}
