use std::{
    ffi::c_void,
    io::{Read, Seek},
};

use once_cell::unsync::Lazy;
use zhuque::{
    JS_CFUNC_NATIVE_DEF, JS_CGETSET_NATIVE_DEF,
    class::{ClassDef, ClassID},
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{JS_PROP_C_W_E, ObjectPayload, ObjectRef},
    runtime::RuntimeImpl,
    value::JSValue,
};

#[deny(clippy::mut_from_ref)]
fn js_file_get<'a>(ctx: ContextRef, obj: JSValue) -> Result<&'a mut std::fs::File, i32> {
    let opaque = obj
        .retrive_object_payload(&ctx, class_id_file())?
        .other()
        .unwrap()
        .opaque();

    if !opaque.is_null() {
        Ok(unsafe { &mut *opaque.cast::<std::fs::File>() })
    } else {
        ctx.throw_type_error("file was closed");
        Err(-1)
    }
}

// XXX: could use less memory and go faster
fn js_file_readAsString(ctx: ContextRef, cc: CallContext) -> JSResult {
    let fh = js_file_get(ctx, *cc.this())?;

    let max_size_val = *cc.arg(0);
    let max_size = if !max_size_val.is_undefined() {
        std::cmp::min(max_size_val.to_array_index(&ctx)?, usize::MAX as i64) as usize
    } else {
        usize::MAX
    };

    let size = fh.seek(std::io::SeekFrom::End(0)).map_err(|e| {
        ctx.throw_internal_error(&e.to_string());
        -1
    })?;
    fh.seek(std::io::SeekFrom::Start(0)).map_err(|e| {
        ctx.throw_internal_error(&e.to_string());
        -1
    })?;

    let len = std::cmp::min(std::cmp::min(size, usize::MAX as u64) as usize, max_size);
    let mut buf = Vec::<u8>::new();
    buf.try_reserve_exact(len).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;
    unsafe {
        buf.set_len(len);
    }
    buf.fill(0);

    fh.read(&mut buf).map_err(|e| {
        ctx.throw_internal_error(&e.to_string());
        -1
    })?;

    ctx.create_string_from_utf8(buf).map(JSValue::String)
}

static CLASS_FILE_PROTO: [FieldDef; 1] = [JS_CFUNC_NATIVE_DEF!(
    "readAsString",
    0,
    js_file_readAsString,
    0
)];

pub fn class_file_new_instance(ctx: &ContextImpl, fh: std::fs::File) -> Result<ObjectRef, i32> {
    let class_id = class_id_file();
    let opaque = Box::into_raw(Box::new(fh)).cast::<c_void>();

    ctx.create_object_user_class(class_id, opaque).map_err(|e| {
        let p = e.other().unwrap();
        let _ = unsafe { Box::from_raw(p.opaque().cast::<std::fs::File>()) };
        -1
    })
}

thread_local! {
    static CLASS_DEF_FILE: Lazy<ClassDef> = Lazy::new(|| {
        ClassDef::define("File")
            .initialize(
                |ctx: ContextRef, _: ClassID| -> Result<Option<ObjectRef>, i32> {
                    let mut proto = ctx.create_base_object()?;
                    proto.install_field_defs(&ctx, &CLASS_FILE_PROTO)?;
                    Ok(Some(proto))
                },
            )
            .finalize_opaque(|_: &mut RuntimeImpl, opaque: *mut c_void, _: usize| {
                if !opaque.is_null() {
                    let p = unsafe { Box::from_raw(opaque.cast::<std::fs::File>())};
                    println!("[O.o] --- drop {p:?}");
                    drop(p);
                }
            })
    });
}

pub fn register_class_file(rt: &mut RuntimeImpl) -> Result<ClassID, i32> {
    CLASS_DEF_FILE.with(|cdef| rt.register_class_def(cdef))
}

#[inline(always)]
pub fn class_id_file() -> ClassID {
    CLASS_DEF_FILE.with(|cd| cd.class_id())
}
