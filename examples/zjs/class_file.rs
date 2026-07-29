// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::RefCell,
    io::{Read, Write},
};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use zuqe::{CallContext, FieldDef, JSResult, JSValue, Object, ScopedContext, def_native_object};

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

fn get_file_fd<'a>(
    ctx: &mut ScopedContext,
    val: &'a JSValue,
) -> Result<&'a RefCell<FileDesc>, i32> {
    JSFile::payload_of(val).ok_or_else(|| ctx.throw_type_error("not a File instance"))
}

fn js_file_fileno(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let fd = get_file_fd(ctx, &this)?;
    Ok(JSValue::from(fd.borrow().as_raw_fd()))
}

fn js_file_readAsString(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let fd = get_file_fd(ctx, &this)?;
    let mut fdesc = fd.borrow_mut();

    let max_size_val = cc.arg(0);
    let max_size = if !max_size_val.is_undefined() {
        Some(std::cmp::min(max_size_val.to_index(ctx)?, usize::MAX as i64) as usize)
    } else {
        None
    };

    let mut buf = Vec::<u8>::new();
    if let Some(len) = max_size {
        buf.try_reserve_exact(len).map_err(|_| ctx.throw_out_of_memory())?;
        unsafe {
            buf.set_len(len);
        }
        buf.fill(0);
    }

    match &mut *fdesc {
        FileDesc::File(fh) => {
            if max_size.is_some() {
                let n = fh.read(&mut buf).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
                buf.truncate(n);
            } else {
                fh.read_to_end(&mut buf).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
            }
        }
        FileDesc::Stdin => {
            let mut f = std::io::stdin();
            if max_size.is_some() {
                let n = f.read(&mut buf).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
                buf.truncate(n);
            } else {
                f.read_to_end(&mut buf).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
            }
        }
        FileDesc::Stdout | FileDesc::Stderr => {}
    }

    ctx.create_string_from_utf8(buf).map(JSValue::make_string)
}

pub(super) fn js_file_puts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let magic = cc.magic();

    if magic == 1 {
        let this = cc.this();
        let fd = get_file_fd(ctx, &this)?;
        let mut fdesc = fd.borrow_mut();
        for arg in cc.arguments() {
            let s = arg.to_string(ctx)?;
            let s = s.content().encode_utf8(false);

            match &mut *fdesc {
                FileDesc::File(f) => {
                    f.write_all(s.as_bytes()).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
                }
                FileDesc::Stdout => {
                    std::io::stdout().write_all(s.as_bytes()).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
                }
                FileDesc::Stderr => {
                    std::io::stderr().write_all(s.as_bytes()).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
                }
                FileDesc::Stdin => {}
            }
        }
    } else {
        for arg in cc.arguments() {
            let s = arg.to_string(ctx)?;
            let s = s.content().encode_utf8(false);
            std::io::stdout().write_all(s.as_bytes()).map_err(|e| ctx.throw_internal_error(&e.to_string()))?;
        }
    }

    Ok(JSValue::UNDEFINED)
}

fn js_file_flush(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let fd = get_file_fd(ctx, &this)?;
    let mut fdesc = fd.borrow_mut();

    match &mut *fdesc {
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

fn js_file_close(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let mut jsfile = this.as_native_object::<JSFile>().ok_or_else(|| ctx.throw_type_error("not a File instance"))?;
    let _ = jsfile.take_payload();
    Ok(JSValue::UNDEFINED)
}

const CLASS_FILE_PROTO: &[FieldDef] = &[
    FieldDef::FUNC("fileno", 0, js_file_fileno, 0),
    FieldDef::FUNC("readAsString", 0, js_file_readAsString, 0),
    FieldDef::FUNC("puts", 1, js_file_puts, 1),
    FieldDef::FUNC("flush", 0, js_file_flush, 0),
    FieldDef::FUNC("close", 0, js_file_close, 0),
];

def_native_object!("File", JSFile, RefCell<FileDesc>, CLASS_FILE_PROTO, None);

impl JSFile {
    /// Create a File instance from a std::fs::File handle.
    pub fn create_file_instance(ctx: &mut ScopedContext, fh: std::fs::File) -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::File(fh)))
            .map(Into::into)
    }
    /// Create a File instance wrapping stdin.
    pub fn create_stdin_instance(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::Stdin)).map(Into::into)
    }
    /// Create a File instance wrapping stdout.
    pub fn create_stdout_instance(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::Stdout)).map(Into::into)
    }
    /// Create a File instance wrapping stderr.
    pub fn create_stderr_instance(ctx: &mut ScopedContext) -> Result<Object, i32> {
        Self::new_instance(ctx, JSValue::UNDEFINED, RefCell::new(FileDesc::Stderr)).map(Into::into)
    }
}
