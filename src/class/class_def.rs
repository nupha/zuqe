// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    Context, RuntimeImpl,
    atom::Atom,
    class::{ClassCallFn, ClassInitializeFn, FreeObjFn, GetPrototypeFn, SetPrototypeFn},
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
pub(crate) struct StdClassDef {
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
            let _ = Box::from(self.fields_init_fd);
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

/// user class definition
#[derive(Default)]
pub struct ClassDef {
    /// class name
    pub(crate) class_name: &'static str,

    /// callback handler when called
    pub(crate) call: Option<ClassCallFn>,
    /// exotic property method handlers
    pub(crate) exotic: Option<Box<ClassExoticMethods>>,
    /// free user class instance handler
    pub(crate) finalize_opaque: Option<FreeObjFn>,

    /// initialize class proto, constructor to context
    pub(crate) initialize: Option<ClassInitializeFn>,
    /// class instance finalize callback
    pub(crate) finalize: Option<ClassFinalizeFn>,
}

impl ClassDef {
    /// create a class def
    pub const fn new(class_name: &'static str) -> Self {
        debug_assert!(!class_name.is_empty());
        Self {
            class_name,
            call: None,
            exotic: None,
            finalize_opaque: None,

            initialize: None,
            finalize: Some(Self::user_class_finalize),
        }
    }

    /// set class initialize callback
    pub const fn initialize(mut self, f: ClassInitializeFn) -> Self {
        self.initialize.replace(f);
        self
    }

    /// Handler on class instance finalize to process opaque pointer.
    /// used for of non-builtin class instance only.
    pub const fn finalize_opaque(mut self, f: FreeObjFn) -> Self {
        self.finalize_opaque.replace(f);
        self
    }

    fn user_class_finalize(rt: &mut RuntimeImpl, isa: &ObjectPayload, id: usize) {
        if let Some(u) = isa.other() {
            if let Some(f) = rt.class_registry[u.class_id as usize].free_obj {
                f(rt, u.opaque, id)
            }
        } else {
            unreachable!();
        }
    }
}

fn js_arguments_define_own_property(
    mut ctx: Context,
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
            .to_array_index(ctx.rt_ref())
            .is_some_and(|i| i < obj.fast_array_count())
    {
        obj.destruct_fast_array(&mut ctx)?;
    }

    // run the default define own property
    obj.define_property(
        &mut ctx,
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
