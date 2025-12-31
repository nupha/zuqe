use std::{
    ffi::c_void,
    io::{Read, Write},
    os::fd::AsRawFd,
};

use once_cell::unsync::Lazy;
use zuqe::{
    class::{ClassDef, ClassID},
    context::Context,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::ObjectRef,
    runtime::RuntimeImpl,
    value::JSValue,
};

#[derive(Debug)]
enum FileDesc {
    File(std::fs::File),
    Stdin,
    Stdout,
    Stderr,
}

impl FileDesc {
    pub fn as_raw_fd(&self) -> i32 {
        match self {
            Self::File(f) => f.as_raw_fd(),
            Self::Stdin => std::io::stdin().as_raw_fd(),
            Self::Stdout => std::io::stdout().as_raw_fd(),
            Self::Stderr => std::io::stderr().as_raw_fd(),
        }
    }
}

fn js_file_get_desc_mut<'a>(ctx: Context, obj: JSValue) -> Result<&'a mut FileDesc, i32> {
    let opaque = obj
        .retrive_object_payload(&ctx, JSFile::class_id())?
        .other()
        .unwrap()
        .opaque();

    if !opaque.is_null() {
        Ok(unsafe { &mut *opaque.cast::<FileDesc>() })
    } else {
        ctx.throw_type_error("file was closed");
        Err(-1)
    }
}

fn js_file_fileno(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let fh = js_file_get_desc_mut(ctx, *cc.this())?;
    Ok(JSValue::Int(fh.as_raw_fd()))
}

fn js_file_readAsString(cc: CallContext) -> JSResult {
    let ctx = cc.context();
    let fh = js_file_get_desc_mut(ctx, *cc.this())?;

    let max_size_val = *cc.arg(0);
    let max_size = if !max_size_val.is_undefined() {
        Some(std::cmp::min(max_size_val.to_index(&ctx)?, usize::MAX as i64) as usize)
    } else {
        None
    };

    let mut buf = Vec::<u8>::new();
    if let Some(len) = max_size {
        buf.try_reserve_exact(len).map_err(|_| {
            ctx.throw_out_of_memory();
            -1
        })?;
        unsafe {
            buf.set_len(len);
        }
        buf.fill(0);
    }

    match fh {
        FileDesc::File(fh) => {
            if max_size.is_some() {
                let n = fh.read(&mut buf).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
                buf.truncate(n);
            } else {
                fh.read_to_end(&mut buf).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
            }
        }
        FileDesc::Stdin => {
            let mut f = std::io::stdin();
            if max_size.is_some() {
                let n = f.read(&mut buf).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
                buf.truncate(n);
            } else {
                f.read_to_end(&mut buf).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
            }
        }
        FileDesc::Stdout | FileDesc::Stderr => {}
    }

    ctx.create_string_from_utf8(buf).map(JSValue::String)
}

pub(super) fn js_file_puts(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let ctx = cc.context();

    let fh = if magic == 1 {
        js_file_get_desc_mut(ctx, *cc.this())?
    } else {
        &mut FileDesc::Stdout
    };

    for arg in cc.arguments() {
        let s = arg.to_string(&ctx)?;
        let s = s.content().encode_utf8(false);

        match fh {
            FileDesc::File(f) => {
                f.write_all(s.as_bytes()).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
            }
            FileDesc::Stdout => {
                std::io::stdout().write_all(s.as_bytes()).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
            }
            FileDesc::Stderr => {
                std::io::stderr().write_all(s.as_bytes()).map_err(|e| {
                    ctx.throw_internal_error(&e.to_string());
                    -1
                })?;
            }
            FileDesc::Stdin => {}
        }
    }

    Ok(JSValue::UNDEFINED)
}

fn js_file_flush(cc: CallContext) -> JSResult {
    match js_file_get_desc_mut(cc.context(), *cc.this())? {
        FileDesc::File(f) => {
            let _ = f.flush().inspect_err(|e| {
                eprintln!("flush file error: {}", e);
            });
        }
        FileDesc::Stdout => {
            let _ = std::io::stdout().flush().inspect_err(|e| {
                eprintln!("flush stdout error: {}", e);
            });
        }
        FileDesc::Stderr => {
            let _ = std::io::stderr().flush().inspect_err(|e| {
                eprintln!("flush stderr error: {}", e);
            });
        }
        FileDesc::Stdin => {}
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

static CLASS_FILE_PROTO: once_cell::sync::Lazy<Box<[FieldDef]>> =
    once_cell::sync::Lazy::new(|| {
        let lst = vec![
            FieldDef::CALL("fileno", 0, js_file_fileno, 0),
            FieldDef::CALL("readAsString", 0, js_file_readAsString, 0),
            FieldDef::CALL("puts", 1, js_file_puts, 1),
            FieldDef::CALL("flush", 0, js_file_flush, 0),
            FieldDef::CALL("close", 0, js_file_close, 0),
        ];
        lst.into()
    });

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
            let p = unsafe { Box::from_raw(opaque.cast::<FileDesc>()) };
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

    fn new_instance_internal(ctx: Context, fd: FileDesc) -> Result<ObjectRef, i32> {
        let class_id = Self::class_id();
        let opaque = Box::into_raw(Box::new(fd)).cast::<c_void>();
        ctx.create_object_user_class(class_id, opaque).map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<FileDesc>()) };
            -1
        })
    }
    pub fn new_instance(ctx: Context, fh: std::fs::File) -> Result<ObjectRef, i32> {
        Self::new_instance_internal(ctx, FileDesc::File(fh))
    }
    pub fn new_instance_stdin(ctx: Context) -> Result<ObjectRef, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdin)
    }
    pub fn new_instance_stdout(ctx: Context) -> Result<ObjectRef, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdout)
    }
    pub fn new_instance_stderr(ctx: Context) -> Result<ObjectRef, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stderr)
    }
}
