use std::{
    ffi::c_void,
    io::{Read, Seek, Write},
};

use once_cell::unsync::Lazy;
use zhuque::{
    class::{ClassDef, ClassID},
    context::Context,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::ObjectRef,
    runtime::RuntimeImpl,
    value::JSValue,
};

fn js_file_get<'a>(ctx: Context, obj: JSValue) -> Result<&'a mut std::fs::File, i32> {
    let opaque = obj
        .retrive_object_payload(&ctx, JSFile::class_id())?
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

fn js_file_readAsString(cc: CallContext) -> JSResult {
    let ctx = cc.context();
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

fn js_file_puts(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let fh = js_file_get(ctx, *cc.this())?;

    for arg in cc.arguments() {
        let s = arg.to_string(&ctx)?;
        let s = s.content().to_utf8(false);

        fh.write_all(s.as_bytes()).map_err(|e| {
            ctx.throw_internal_error(&e.to_string());
            -1
        })?;
    }

    Ok(JSValue::UNDEFINED)
}

fn js_file_close(cc: CallContext) -> JSResult {
    let mut this = *cc.this();
    let u = this
        .retrive_object_payload_mut(&cc.context(), JSFile::class_id())?
        .other_mut()
        .unwrap();

    if !u.opaque().is_null() {
        unsafe {
            let p = u.replace_opaque(std::ptr::null_mut());
            drop(Box::from_raw(p.cast::<std::fs::File>()));
        }
    }

    Ok(JSValue::UNDEFINED)
}

static CLASS_FILE_PROTO: [FieldDef; 3] = [
    FieldDef::CALL("readAsString", 0, js_file_readAsString, 0),
    FieldDef::CALL("puts", 1, js_file_puts, 0),
    FieldDef::CALL("close", 0, js_file_close, 0),
];

pub struct JSFile;

impl JSFile {
    thread_local! {
        static CLASS_DEF: Lazy<ClassDef> = Lazy::new(|| {
            ClassDef::define("File")
                .initialize(JSFile::init)
                .finalize_opaque(JSFile::fini)
        });
    }

    fn init(ctx: Context, _: ClassID) -> Result<Option<ObjectRef>, i32> {
        let mut proto = ctx.create_base_object()?;
        proto.install_field_defs(&ctx, &CLASS_FILE_PROTO)?;
        Ok(Some(proto))
    }
    fn fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        if !opaque.is_null() {
            let p = unsafe { Box::from_raw(opaque.cast::<std::fs::File>()) };
            drop(p);
        }
    }

    pub fn register(rt: &mut RuntimeImpl) -> Result<ClassID, i32> {
        Self::CLASS_DEF.with(|cdef| rt.register_class_def(cdef))
    }

    #[inline(always)]
    pub fn class_id() -> ClassID {
        Self::CLASS_DEF.with(|cdef| cdef.class_id())
    }

    pub fn new_instance(ctx: Context, fh: std::fs::File) -> Result<ObjectRef, i32> {
        let class_id = Self::class_id();
        let opaque = Box::into_raw(Box::new(fh)).cast::<c_void>();

        ctx.create_object_user_class(class_id, opaque).map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<std::fs::File>()) };
            -1
        })
    }
}
