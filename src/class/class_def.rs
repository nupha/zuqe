use crate::{
    atom::Atom,
    class::{ClassID, ClassInitializeFn, FreeObjFn},
    context::ContextRef,
    function::JSFunctionDef,
    object::JS_PROP_NO_EXOTIC,
    value::JSValue,
};

use super::{
    ClassCallFn, ClassFinalizeFn, DefineOwnPropertyFn, DeletePropertyFn, GetOwnPropertyFn,
    GetOwnPropertyNamesFn, GetPropertyFn, HasPropertyFn, SetPropertyFn,
};

pub(crate) const JS_DEFINE_CLASS_HAS_HERITAGE: i32 = 1 << 0;

#[derive(Copy, Clone, Default)]
pub(crate) struct JSClassExoticMethods {
    pub get_own_property: Option<GetOwnPropertyFn>,
    pub get_own_property_names: Option<GetOwnPropertyNamesFn>,
    pub delete_property: Option<DeletePropertyFn>,
    pub define_own_property: Option<DefineOwnPropertyFn>,
    pub has_property: Option<HasPropertyFn>,
    pub get_property: Option<GetPropertyFn>,
    pub set_property: Option<SetPropertyFn>,
}

/// builtin class definition
pub struct StdClassDef {
    pub class_name: Atom,
    pub initialize: Option<ClassInitializeFn>,
    pub finalize: Option<ClassFinalizeFn>,
}

/// class definition
#[derive(Default)]
pub struct ClassDef {
    /// class name
    pub(crate) class_name: &'static str,
    pub(crate) class_id: ClassID,

    /// callback handler when called
    pub(crate) call: Option<ClassCallFn>,
    /// exotic property method handlers
    pub(crate) exotic: Option<Box<JSClassExoticMethods>>,
    /// free user class instance handler
    pub(crate) finalize_opaque: Option<FreeObjFn>,

    /// initialize class proto, constructor to context
    pub(crate) initialize: Option<ClassInitializeFn>,
    /// class instance finalize callback
    pub(crate) finalize: Option<ClassFinalizeFn>,
}

impl ClassDef {
    /// for non-builtin class defs only
    #[inline(always)]
    pub const fn class_id(&self) -> ClassID {
        self.class_id
    }
}

#[derive(Debug)]
pub struct ClassFieldsDef {
    pub fields_init_fd: *mut JSFunctionDef,
    pub computed_fields_count: i32,
    pub need_brand: bool,
    pub brand_push_pos: i32,
    pub is_static: bool,
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
            computed_fields_count: Default::default(),
            need_brand: Default::default(),
            brand_push_pos: Default::default(),
            is_static: Default::default(),
        }
    }
}

/// prop is not consumed
fn js_arguments_define_own_property(
    ctx: ContextRef,
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
            .to_array_index()
            .is_some_and(|i| i < obj.fast_array_count())
    {
        obj.destruct_fast_array(&ctx)?;
    }

    // run the default define own property
    obj.define_property(
        &ctx,
        prop,
        val,
        getter,
        setter,
        flags as u32 | JS_PROP_NO_EXOTIC,
    )
}

pub(crate) const js_arguments_exotic_methods: JSClassExoticMethods = JSClassExoticMethods {
    define_own_property: Some(js_arguments_define_own_property),
    delete_property: None,
    get_own_property: None,
    get_own_property_names: None,
    has_property: None,
    get_property: None,
    set_property: None,
};
