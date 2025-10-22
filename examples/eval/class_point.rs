use std::os::raw::c_void;

use zhuque::{
    JS_CGETSET_NATIVE_DEF,
    class::{JS_SetConstructor, JSClassDef, JSClassID},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunctionType},
    object::{JS_PROP_C_W_E, ObjectPayload},
    obsoletes::JS_NewClass,
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug)]
pub struct PointData {
    pub x: i32,
    pub y: i32,
}

fn point_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as JSClassID;

    let x = cc.arg(0).to_i32(ctx)?;
    let y = cc.arg(1).to_i32(ctx)?;
    let p = PointData { x, y };
    println!("[O.o] Point: {p:?}, class_id={class_id}");

    let obj = ctx
        .create_object_from_ctor(
            cc.this(),
            ObjectPayload::OTHER(class_id, std::ptr::null_mut()),
        )
        .map_err(|_| -1)?;

    Ok(obj.into())
}

fn point_finalize(rt: &mut JSRuntime, payload: &ObjectPayload, id: usize) {
    println!("[O.o] Point finalize: {payload:?}, id={id}");
}

fn point_getset(ctx: &JSContext, cc: CallContext) -> JSResult {
    println!("[O.o] Point getset {:?}", cc.magic());
    Ok(JSValue::UNDEFINED)
}

static class_point_proto: [FieldDef; 2] = [
    JS_CGETSET_NATIVE_DEF!("x", point_getset, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("y", point_getset, NONE, 1),
];

pub fn init_class_point(ctx: &JSContext) -> Result<(), i32> {
    // register class
    let class_id = ctx.rt_mut().add_class(&JSClassDef {
        class_name: "Point",
        finalizer: Some(point_finalize),
        ..Default::default()
    })?;

    // create prototype
    let proto: JSValue = ctx.create_base_object().unwrap().into();
    proto.install_field_defs(ctx, &class_point_proto)?;

    // create constructor
    let ctor = ctx.create_native_function(
        NativeFunctionType::ConstructorOrFunc(point_constructor),
        "Point",
        2,
        class_id as i16,
    )?;
    JS_SetConstructor(ctx, &ctor, &proto);
    ctor.install_field_defs(ctx, &class_point_proto)?;

    // expose ctor to global
    ctx.global_obj()
        .define_property_value_str(ctx, "Point", ctor, JS_PROP_C_W_E)?;

    Ok(())
}
