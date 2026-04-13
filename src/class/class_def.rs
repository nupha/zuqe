// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Callback, ClassID, FieldDef, RuntimeImpl,
    atom::Atom,
    class::{
        ClassCallFn, ClassFinalizeOpaqueFn, ClassInitializeFn, ClassUnregisterFn, GetPrototypeFn,
        SetPrototypeFn,
    },
    context::ScopedContext,
    function::{CallbackFn, FunctionDef},
    object::{JS_PROP_CONFIGURABLE, JS_PROP_NO_EXOTIC, JS_PROP_WRITABLE, ObjectPayload},
    value::JSValue,
};

use super::{
    ClassFinalizeFn, DefineOwnPropertyFn, DeletePropertyFn, GetOwnPropertyFn,
    GetOwnPropertyNamesFn, GetPropertyFn, HasPropertyFn, SetPropertyFn,
};

/// class has extends inherits
pub(crate) const JS_DEFINE_CLASS_HAS_HERITAGE: i32 = 1 << 0;

#[derive(Copy, Clone, Default)]
pub(crate) struct ClassExoticMethods {
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

#[derive(Debug)]
pub(crate) struct ClassFieldsDef {
    pub(crate) fields_init_fd: *mut FunctionDef,
    pub(crate) computed_fields_count: i32,
    pub(crate) is_static: bool,

    /// class private brand requires Symbol support
    #[cfg(feature = "builtin_symbol")]
    pub(crate) need_brand: bool,
    #[cfg(feature = "builtin_symbol")]
    pub(crate) brand_push_pos: i32,
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
            computed_fields_count: 0,
            is_static: false,

            #[cfg(feature = "builtin_symbol")]
            need_brand: false,
            #[cfg(feature = "builtin_symbol")]
            brand_push_pos: 0,
        }
    }
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

    pub const fn magic(mut self, magic: i16) -> Self {
        self.magic = magic;
        self
    }
    pub const fn entries(mut self, entries: &'static [FieldDef]) -> Self {
        self.entries = entries;
        self
    }

    pub const fn export_to_global(mut self) -> Self {
        self.export_to_global = true;
        self
    }
}

#[derive(Clone, Copy)]
pub enum ClassInitType {
    Normal {
        proto_entries: &'static [FieldDef],
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
    pub(crate) initialize: ClassInitType,
    /// class instance finalize callback
    pub(crate) finalize: Option<ClassFinalizeFn>,
    /// handler on class instance dispose
    pub(crate) finalize_opaque: Option<ClassFinalizeOpaqueFn>,
    /// class def unregister handler.
    pub(crate) unregister: Option<ClassUnregisterFn>,
}

impl Default for ClassDef {
    fn default() -> Self {
        Self {
            name: "",
            name_atom: Atom::NULL,

            call: None,
            exotic: None,
            initialize: ClassInitType::Normal {
                proto_entries: &[],
                ctor: None,
            },
            finalize: None,
            finalize_opaque: None,
            unregister: None,
        }
    }
}

impl ClassDef {
    /// Create user-defined class def with proto entries and optional ctor def
    pub const fn new(
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

            unregister: None,
            initialize: ClassInitType::Normal {
                proto_entries,
                ctor,
            },
            finalize: Some(Self::default_fini_fn),
            finalize_opaque: None,
        }
    }

    /// Create user-defined class def with init fn
    pub const fn new_with_init_func(name: &'static str, init_fn: ClassInitializeFn) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            name_atom: Atom::NULL,
            call: None,
            exotic: None,

            unregister: None,
            initialize: ClassInitType::InitFn(init_fn),
            finalize: Some(Self::default_fini_fn),
            finalize_opaque: None,
        }
    }

    /// Set class instance dispose handler.
    ///
    /// This callback could be use to process opaque pointer when class instance is disposed.
    pub const fn on_dispose(mut self, f: ClassFinalizeOpaqueFn) -> Self {
        self.finalize_opaque.replace(f);
        self
    }

    /// set class unregister callback
    pub const fn on_unregister(mut self, f: ClassUnregisterFn) -> Self {
        self.unregister.replace(f);
        self
    }

    /// Perform class initialize in context.
    pub(super) fn initialize(&self, ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        match self.initialize {
            ClassInitType::Normal {
                proto_entries,
                ctor,
            } => {
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
                        debug_assert!(self.name_atom != Atom::NULL);
                        ctx.global_obj().define_property_value(
                            ctx,
                            self.name_atom,
                            ctor.into(),
                            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
                        )?;
                    }
                }
            }

            ClassInitType::InitFn(init_fn) => {
                if ctx.class_proto(class_id).is_none() {
                    init_fn(ctx, class_id)?;

                    // if let Some(proto) = init_fn(ctx, class_id)? {
                    //     debug_assert!(
                    //         ctx.class_slots[class_id as usize]
                    //             .proto
                    //             .is_none_or(|p| p == proto)
                    //     );
                    //     ctx.class_slots[class_id as usize].proto = Some(proto);
                    // }

                    if ctx.class_proto(class_id).is_none() {
                        ctx.throw_internal_error(&format!(
                            "class ({class_id}) initialization must setup class slot for proto"
                        ));
                    }
                }
            }
        }

        Ok(())
    }

    fn default_fini_fn(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {
        if let Some(u) = isa.other() {
            if let Some(f) = rt.class_registry[u.class_id as usize].finalize_opaque {
                f(rt, u.opaque, id)
            }
        } else {
            unreachable!("builtin class: finalize handler is required");
        }
    }
}

fn js_arguments_define_own_property(
    ctx: &mut ScopedContext,
    this_obj: JSValue,
    prop: Atom,
    val: JSValue,
    getter: JSValue,
    setter: JSValue,
    flags: i32,
) -> Result<bool, i32> {
    let mut obj = this_obj.as_object().copied().unwrap();

    // convert to normal array when redefining an existing numeric field
    if obj.flags().is_fast_array()
        && prop
            .to_array_index(ctx.rt())
            .is_some_and(|i| i < obj.fast_array_count())
    {
        obj.destruct_fast_array(ctx)?;
    }

    // run the default define own property
    obj.define_property(
        ctx,
        prop,
        val,
        getter,
        setter,
        flags as u32 | JS_PROP_NO_EXOTIC,
    )
}

pub(crate) const js_arguments_exotic_methods: ClassExoticMethods = ClassExoticMethods {
    define_own_property: Some(js_arguments_define_own_property),
    delete_property: None,
    get_own_property: None,
    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
    get_prototype: None,
    set_prototype: None,
};
