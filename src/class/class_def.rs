// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::ffi::c_void;

use crate::{
    Callback, ClassID, FieldDef, RuntimeImpl,
    atom::Atom,
    class::{
        ClassCallFn, ClassFinalizeOpaqueFn, ClassGcTraceFn, ClassInitializeFn, ClassUnregisterFn,
        GetPrototypeFn, SetPrototypeFn,
    },
    context::ScopedContext,
    function::{CallbackFn, NativeFunction},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object},
};

use super::{
    ClassFinalizeFn, DefineOwnPropertyFn, DeletePropertyFn, GetOwnPropertyFn,
    GetOwnPropertyNamesFn, GetPropertyFn, HasPropertyFn, SetPropertyFn,
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
    /// class instance finalize callback, callbed before Object::drop()
    pub(crate) on_finalize: Option<ClassFinalizeFn>,
    /// handler on class instance dispose
    pub(crate) on_finalize_opaque: Option<ClassFinalizeOpaqueFn>,
    /// class def unregister handler.
    pub(crate) on_unregister: Option<ClassUnregisterFn>,
    /// to trace JSValues stored in an instance's payload (opaque)
    pub(crate) on_gc_trace: Option<ClassGcTraceFn>,
    /// Drop function for the typed payload behind *mut c_void
    pub(crate) drop_payload: Option<unsafe fn(*mut c_void)>,
    /// GC trace function for the typed payload behind *mut c_void
    pub(crate) trace_payload: Option<fn(*const c_void, &mut gc_lite::GcTraceCtx)>,
}

impl Default for ClassDef {
    fn default() -> Self {
        Self {
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
            on_finalize_opaque: None,
            on_unregister: None,
            on_gc_trace: None,
            drop_payload: None,
            trace_payload: None,
        }
    }
}

impl ClassDef {
    /// Create user-defined class def with proto entries and optional ctor def
    pub fn new(
        name: &'static str,
        proto_entries: &'static [FieldDef],
        ctor: Option<ClassCtorDef>,
    ) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            name_atom: Atom::NULL,
            call: None,
            exotic: None,
            on_unregister: None,
            on_initialize: ClassInitType::Normal {
                proto_entries,
                ctor,
                extends_class: None,
            },
            on_finalize: Some(Self::default_fini_fn),
            on_finalize_opaque: None,
            on_gc_trace: None,
            drop_payload: None,
            trace_payload: None,
        }
    }

    /// Create user-defined class def with proto entries, ctor and extends inheritance.
    /// `extends_class` is a resolver invoked lazily at initialize time, so the
    /// parent class need not be registered before the child.
    pub fn new_extends(
        name: &'static str,
        proto_entries: &'static [FieldDef],
        ctor: Option<ClassCtorDef>,
        extends_class: fn() -> ClassID,
    ) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            name_atom: Atom::NULL,
            call: None,
            exotic: None,
            on_unregister: None,
            on_initialize: ClassInitType::Normal {
                proto_entries,
                extends_class: Some(extends_class),
                ctor,
            },
            on_finalize: Some(Self::default_fini_fn),
            on_finalize_opaque: None,
            on_gc_trace: None,
            drop_payload: None,
            trace_payload: None,
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
            name_atom: Atom::NULL,
            call: None,
            exotic: None,

            on_unregister: None,
            on_initialize: ClassInitType::InitFn(init_fn),
            on_finalize: Some(Self::default_fini_fn),
            on_finalize_opaque: None,
            on_gc_trace: None,
            drop_payload: None,
            trace_payload: None,
        }
    }

    /// Create builtin class def
    pub(crate) const fn builtin(
        name: Atom,
        proto_entries: &'static [FieldDef],
        ctor: Option<ClassCtorDef>,
    ) -> Self {
        Self {
            name: "",
            name_atom: name,
            call: None,
            exotic: None,
            on_unregister: None,
            on_initialize: ClassInitType::Normal {
                proto_entries,
                ctor,
                extends_class: None,
            },
            on_finalize: None,
            on_finalize_opaque: None,
            on_gc_trace: None,
            drop_payload: None,
            trace_payload: None,
        }
    }

    /// Create builtin class def with customized init func
    pub(crate) const fn builtin_with_init_func(name: Atom, init_fn: ClassInitializeFn) -> Self {
        Self {
            name: "",
            name_atom: name,
            call: None,
            exotic: None,
            on_unregister: None,
            on_initialize: ClassInitType::InitFn(init_fn),
            on_finalize: None,
            on_finalize_opaque: None,
            on_gc_trace: None,
            drop_payload: None,
            trace_payload: None,
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

    /// Set class finalize callback of builtin class def. Called from `ObjectImpl::drop()`.
    ///
    /// This method is for builtin class def only. User class def should use on_dispose (which is a public api) instead.
    pub(crate) const fn on_finalize(mut self, f: ClassFinalizeFn) -> Self {
        self.on_finalize = Some(f);
        self
    }

    /// Set class instance dispose handler.
    ///
    /// This callback could be use to process opaque pointer when class instance is disposed.
    pub const fn on_dispose(mut self, f: ClassFinalizeOpaqueFn) -> Self {
        self.on_finalize_opaque.replace(f);
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

    /// Set drop function for the typed payload behind *mut c_void
    pub(crate) const fn on_drop_payload(mut self, f: unsafe fn(*mut c_void)) -> Self {
        self.drop_payload = Some(f);
        self
    }

    /// Set GC trace function for the typed payload behind *mut c_void
    pub(crate) const fn on_trace_payload(mut self, f: fn(*const c_void, &mut gc_lite::GcTraceCtx)) -> Self {
        self.trace_payload = Some(f);
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
                        let class_name = ctx.rt().class_registry[class_id as usize].name_atom;
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

    fn default_fini_fn(rt: &mut RuntimeImpl, isa: *mut c_void, id: usize) {
        if !isa.is_null() {
            let other = unsafe { &*(isa as *const crate::class::OtherPayload) };
            let class_id = other.class_id;
            if let Some(f) = rt.class_registry[class_id as usize].on_finalize_opaque {
                f(rt, other.opaque, class_id as usize, id)
            }
        } else {
            unreachable!("builtin class: finalize handler is required");
        }
    }
}
