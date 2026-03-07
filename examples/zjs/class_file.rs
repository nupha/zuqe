// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::Cell,
    ffi::c_void,
    io::{Read, Write},
};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use gc_lite::GcScope;
use zuqe::{
    CallContext, ClassDef, ClassID, Context, FieldDef, JSResult, JSValue, Object, Runtime,
    RuntimeImpl,
};

#[derive(Debug)]
#[repr(u8)]
enum FileDesc {
    Stdin,
    Stdout,
    Stderr,
    File(std::fs::File),
}

impl FileDesc {
    pub fn as_raw_fd(&self) -> i32 {
        #[cfg(unix)]
        match self {
            Self::File(f) => f.as_raw_fd(),
            Self::Stdin => std::io::stdin().as_raw_fd(),
            Self::Stdout => std::io::stdout().as_raw_fd(),
            Self::Stderr => std::io::stderr().as_raw_fd(),
        }

        #[cfg(windows)]
        match self {
            Self::Stdin => 0,
            Self::Stdout => 1,
            Self::Stderr => 2,
            Self::File(f) => f.as_raw_handle() as i32,
        }
    }
}

fn js_file_get_desc_mut<'a>(mut ctx: Context, obj: JSValue) -> Result<&'a mut FileDesc, i32> {
    let opaque = obj
        .retrive_object_payload(&mut ctx, JSFile::class_id())?
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
    let fh = js_file_get_desc_mut(ctx, cc.this())?;
    Ok(JSValue::Int(fh.as_raw_fd()))
}

fn js_file_readAsString(cc: CallContext) -> JSResult {
    let mut ctx = cc.context();
    let fh = js_file_get_desc_mut(ctx, cc.this())?;

    let max_size_val = cc.arg(0);
    let max_size = if !max_size_val.is_undefined() {
        Some(std::cmp::min(max_size_val.to_index(&mut ctx)?, usize::MAX as i64) as usize)
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

    ctx.create_string_from_utf8(buf).map(JSValue::from)
}

pub(super) fn js_file_puts(cc: CallContext) -> JSResult {
    let magic = cc.magic();
    let mut ctx = cc.context();

    let fh = if magic == 1 {
        js_file_get_desc_mut(ctx, cc.this())?
    } else {
        &mut FileDesc::Stdout
    };

    for arg in cc.arguments() {
        let s = arg.to_string(&mut ctx)?;
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
    match js_file_get_desc_mut(cc.context(), cc.this())? {
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
    let mut ctx = cc.context();

    let mut this = cc.this();
    let u = this
        .retrive_object_payload_mut(&mut ctx, JSFile::class_id())?
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

const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::CALL("fileno", 0, js_file_fileno, 0),
    FieldDef::CALL("readAsString", 0, js_file_readAsString, 0),
    FieldDef::CALL("puts", 1, js_file_puts, 1),
    FieldDef::CALL("flush", 0, js_file_flush, 0),
    FieldDef::CALL("close", 0, js_file_close, 0),
];

pub struct JSFile;

impl JSFile {
    fn handle_init(mut ctx: Context, _: ClassID) -> Result<Option<Object>, i32> {
        let mut proto = ctx.create_base_object()?;
        proto.install_fields(&mut ctx, CLASS_FILE_PROTO)?;
        Ok(Some(proto))
    }
    fn handle_fini(_: &mut RuntimeImpl, opaque: *mut c_void, _: usize) {
        if !opaque.is_null() {
            let p = unsafe { Box::from_raw(opaque.cast::<FileDesc>()) };
            drop(p);
        }
    }

    thread_local! {
        static CLASS_ID: Cell<ClassID> = const { Cell::new(0) };
    }

    /// register class
    pub fn register(mut rt: Runtime) -> Result<ClassID, i32> {
        let class_id = rt
            .register_class(
                &ClassDef::new("File")
                    .initialize(Self::handle_init)
                    .finalize_opaque(Self::handle_fini),
            )
            .expect("register class File failed");

        // save assigned class_id
        Self::CLASS_ID.with(|s| s.set(class_id));
        Ok(class_id)
    }

    /// get class id
    #[inline(always)]
    pub fn class_id() -> ClassID {
        Self::CLASS_ID.with(|x| x.get())
    }

    fn new_instance_internal(mut ctx: Context, fd: FileDesc) -> Result<Object, i32> {
        let class_id = Self::class_id();
        let opaque = Box::into_raw(Box::new(fd)).cast::<c_void>();
        ctx.create_object_user_class(class_id, opaque).map_err(|e| {
            let p = e.other().unwrap();
            let _ = unsafe { Box::from_raw(p.opaque().cast::<FileDesc>()) };
            -1
        })
    }
    pub fn new_instance(ctx: Context, fh: std::fs::File) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::File(fh))
    }
    pub fn new_instance_stdin(ctx: Context) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdin)
    }
    pub fn new_instance_stdout(ctx: Context) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stdout)
    }
    pub fn new_instance_stderr(ctx: Context) -> Result<Object, i32> {
        Self::new_instance_internal(ctx, FileDesc::Stderr)
    }
}
