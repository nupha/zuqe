use zhuque::{
    JS_CGETSET_NATIVE_DEF,
    class::{ClassDef, JSClassID},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunctionType},
    object::{JS_PROP_C_W_E, JSObject, ObjectPayload},
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
            ObjectPayload::USER(class_id, std::ptr::null_mut()),
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

pub fn register_class_point(rt: &mut JSRuntime) -> Result<JSClassID, i32> {
    // register class
    let class_id = rt.register_class(&ClassDef {
        class_name: "Point",
        initialize: Some(
            |ctx: &JSContext, class_id: JSClassID| -> Result<JSObject, i32> {
                // create prototype
                let proto = ctx.create_base_object().unwrap();
                proto.install_field_defs(ctx, &class_point_proto)?;

                // create constructor
                let ctor = ctx.create_native_function(
                    NativeFunctionType::ConstructorOrFunc(point_constructor),
                    "Point",
                    2,
                    class_id as i16,
                )?;
                ctor.install_field_defs(ctx, &class_point_proto)?;

                ctx.bind_ctor_proto(&ctor, &proto.clone().into())?;

                // expose ctor to global
                ctx.global_obj()
                    .define_property_value_str(ctx, "Point", ctor, JS_PROP_C_W_E)?;

                Ok(proto)
            },
        ),
        finalize: Some(point_finalize),
        ..Default::default()
    })?;

    Ok(class_id)
}
