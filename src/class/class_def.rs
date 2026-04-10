// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    FieldDef, RuntimeImpl,
    atom::Atom,
    class::{
        ClassCallFn, ClassFinalizeOpaqueFn, ClassInitializeFn, ClassUnregisterFn, GetPrototypeFn,
        SetPrototypeFn,
    },
    context::ScopedContext,
    function::FunctionDef,
    object::{JS_PROP_NO_EXOTIC, ObjectPayload},
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

/// builtin class definition
pub(crate) struct ShortClassDef {
    pub class_name: Atom,
    pub initialize: Option<ClassInitializeFn>,
    pub finalize: Option<ClassFinalizeFn>,
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

/// class definition
#[derive(Default)]
pub struct ClassDef {
    /// class name str. Could be empty str for builtin classes
    pub(crate) name: &'static str,
    /// class name atom. Atom::NULL indicates the class slot is not used
    pub(crate) name_atom: Atom,

    pub(crate) proto_entries: &'static [FieldDef],
    pub(crate) ctor_entries: &'static [FieldDef],

    /// callback handler when called
    pub(crate) call: Option<ClassCallFn>,
    /// exotic property method handlers
    pub(crate) exotic: Option<&'static ClassExoticMethods>,

    /// initialize class proto, constructor to context
    pub(crate) initialize: Option<ClassInitializeFn>,
    /// class instance finalize callback
    pub(crate) finalize: Option<ClassFinalizeFn>,
    /// handler on class instance dispose
    pub(crate) finalize_opaque: Option<ClassFinalizeOpaqueFn>,
    /// class def unregister handler.
    pub(crate) unregister: Option<ClassUnregisterFn>,
}

impl ClassDef {
    /// create a user-defined class def
    pub const fn new(name: &'static str) -> Self {
        debug_assert!(!name.is_empty());
        Self {
            name,
            name_atom: Atom::NULL,
            proto_entries: &[],
            ctor_entries: &[],

            call: None,
            exotic: None,

            unregister: None,
            initialize: None,
            finalize: Some(Self::default_fini_fn),
            finalize_opaque: None,
        }
    }

    /// set class initialize callback
    pub const fn on_init(mut self, f: ClassInitializeFn) -> Self {
        self.initialize.replace(f);
        self
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

    pub const fn proto_entries(mut self, entries: &'static [FieldDef]) -> Self {
        self.proto_entries = entries;
        self
    }

    pub const fn ctor_entries(mut self, entries: &'static [FieldDef]) -> Self {
        self.ctor_entries = entries;
        self
    }

    #[deprecated(note = "use ::on_init() instead")]
    pub const fn initialize(self, f: ClassInitializeFn) -> Self {
        self.on_init(f)
    }

    #[deprecated(note = "use ::on_disponse() instead")]
    pub const fn finalize_opaque(self, f: ClassFinalizeOpaqueFn) -> Self {
        self.on_dispose(f)
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
