use std::ffi::c_void;

use once_cell::unsync::Lazy;
use zhuque::{
    class::{ClassDef, ClassID},
    context::ContextRef,
    error::JSResult,
    function::{CallContext, Callback, FieldDef},
    object::{JS_PROP_C_W_E, ObjectRef},
    runtime::RuntimeImpl,
    value::JSValue,
};

#[derive(Debug)]
pub struct PointData {
    pub x: i32,
    pub y: i32,
}

fn point_constructor(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let class_id = cc.magic() as ClassID;

    let x = cc.arg(0).to_i32(&ctx)?;
    let y = cc.arg(1).to_i32(&ctx)?;
    let p = Box::new(PointData { x, y });

    let obj = ctx.new_instance(class_id, *cc.this(), Box::into_raw(p).cast())?;
    Ok(obj.into())
}

fn point_getset(cc: CallContext) -> JSResult {
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

static CLASS_POINTER_PROTO: [FieldDef; 2] = [
    FieldDef::GETSET("x", Some(point_getset), None, 0),
    FieldDef::GETSET("y", Some(point_getset), None, 1),
];

thread_local! {
    static CLASS_DEF_POINTER: Lazy<ClassDef> = Lazy::new(|| {
        ClassDef::define("Point")
            .initialize(
                |mut ctx: ContextRef, class_id: ClassID| -> Result<Option<ObjectRef>, i32> {
                    // create prototype
                    let mut proto = ctx.create_base_object().unwrap();
                    proto.install_field_defs(&ctx, &CLASS_POINTER_PROTO)?;

                    // create constructor
                    let mut ctor: JSValue = ctx
                        .create_native_function(
                            Callback::ConstructorOrFunc(point_constructor),
                            "Point",
                            2,
                            class_id as i16,
                        )?
                        .into();
                    ctor.install_field_defs(&ctx, &CLASS_POINTER_PROTO)?;

                    ctx.bind_ctor_proto(ctor, proto.into())?;

                    // expose ctor to global
                    ctx.global_obj()
                        .define_property_value_str(&ctx, "Point", ctor, JS_PROP_C_W_E)?;

                    Ok(Some(proto))
                },
            )
            .finalize_opaque(|_: &mut RuntimeImpl, opaque: *mut c_void, id: usize| {
                debug_assert!(!opaque.is_null());
                unsafe {
                    let p = Box::from_raw(opaque.cast::<PointData>());
                    println!("--- Point instance free: {opaque:?}, obj_id: {id}, data: {p:?}");
                }
            })
    });
}

pub fn register_class_point(rt: &mut RuntimeImpl) -> Result<ClassID, i32> {
    CLASS_DEF_POINTER.with(|c| rt.register_class_def(c))
}
