use std::ffi::c_void;

use zhuque::{
    JS_CGETSET_NATIVE_DEF,
    class::{ClassDef, ClassID},
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, NativeFunctionType},
    object::{JS_PROP_C_W_E, JSObject},
    runtime::JSRuntime,
    value::JSValue,
};

#[derive(Debug)]
pub struct PointData {
    pub x: i32,
    pub y: i32,
}

fn point_constructor(ctx: &JSContext, cc: CallContext) -> JSResult {
    let class_id = cc.magic() as ClassID;

    let x = cc.arg(0).to_i32(ctx)?;
    let y = cc.arg(1).to_i32(ctx)?;
    let p = Box::new(PointData { x, y });

    let obj = ctx.new_instance(class_id, cc.this(), Box::into_raw(p).cast())?;
    Ok(obj.into())
}

fn point_getset(_: &JSContext, cc: CallContext) -> JSResult {
    if let Some(p) = cc.this().opaque_ref::<PointData>() {
        match cc.magic() {
            0 => Ok(p.x.into()),
            1 => Ok(p.y.into()),
            _ => unreachable!(),
        }
    } else {
        unreachable!()
    }
}

static class_point_proto: [FieldDef; 2] = [
    JS_CGETSET_NATIVE_DEF!("x", point_getset, NONE, 0),
    JS_CGETSET_NATIVE_DEF!("y", point_getset, NONE, 1),
];

pub fn register_class_point(rt: &mut JSRuntime) -> Result<ClassID, i32> {
    // register class
    let class_id = rt.register_class(
        ClassDef::new("Point")
            .initialize(
                |ctx: &JSContext, class_id: ClassID| -> Result<Option<JSObject>, i32> {
                    // create prototype
                    let proto = ctx.create_base_object().unwrap();
                    proto.install_field_defs(ctx, &class_point_proto)?;

                    // create constructor
                    let ctor: JSValue = ctx
                        .create_native_function(
                            NativeFunctionType::ConstructorOrFunc(point_constructor),
                            "Point",
                            2,
                            class_id as i16,
                        )?
                        .into();
                    ctor.install_field_defs(ctx, &class_point_proto)?;

                    ctx.bind_ctor_proto(&ctor, &proto.clone().into())?;

                    // expose ctor to global
                    ctx.global_obj().define_property_value_str(
                        ctx,
                        "Point",
                        ctor,
                        JS_PROP_C_W_E,
                    )?;

                    Ok(Some(proto))
                },
            )
            .free(|_: &mut JSRuntime, opaque: *mut c_void, id: usize| {
                debug_assert!(!opaque.is_null());
                unsafe {
                    let p = Box::from_raw(opaque.cast::<PointData>());
                    println!("--- Point instance free: {opaque:?}, obj_id: {id}, data: {p:?}");
                }
            }),
    )?;

    Ok(class_id)
}
