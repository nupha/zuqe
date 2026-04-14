use crate::{
    Atom, ClassDef, JSValue, ScopedContext,
    class::{ClassExoticMethods, ClassInitType},
    object::JS_PROP_NO_EXOTIC,
};

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

pub const CLASS_DEF_ARGUMENTS: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Arguments,
    call: None,
    exotic: Some(&js_arguments_exotic_methods),
    initialize: ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_MAPPED_ARGUMENTS: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Arguments,
    call: None,
    exotic: None,
    initialize: ClassInitType::Normal {
        proto_entries: &[],
        ctor: None,
    },
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};
