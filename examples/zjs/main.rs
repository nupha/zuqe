// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_snake_case)]

mod class_file;
mod global;
mod mod_os;
mod mod_std;

use std::{io::Write, process::ExitCode};

use clap::Parser;
use colorized::{Colors::*, colorize_println};
use libloading::Library;
use zuqe::{Context, EvalFlags, EvalType, ModuleDef, Runtime, ScopedContext};

use crate::{
    class_file::JSFile,
    global::setup_globals,
    mod_os::{init_mod_os, mod_os_gc_trace},
    mod_std::init_mod_std,
};

/// write data both to stderr and file
struct TeeWriter<A: Write, B: Write> {
    a: Option<A>,
    b: Option<B>,
}

impl<A: Write, B: Write> TeeWriter<A, B> {
    fn new(a: Option<A>, b: Option<B>) -> Self {
        Self { a, b }
    }
}

impl<A: Write, B: Write> Write for TeeWriter<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(w) = &mut self.a {
            Some(w.write_all(buf)?)
        } else {
            None
        };
        if let Some(w) = &mut self.b {
            Some(w.write_all(buf)?)
        } else {
            None
        };

        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(w) = &mut self.a {
            w.flush()?;
        }
        if let Some(w) = &mut self.b {
            w.flush()?;
        }
        Ok(())
    }
}

#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[arg(short, long, default_value_t = false, help = "load as ES6 module")]
    module: bool,

    #[arg(
        long,
        default_value_t = String::from("1m"),
        value_name = "n",
        help = "set memory limit to n"
    )]
    memory_limit: String,

    #[arg(
        long,
        default_value_t = String::from("1m"),
        value_name = "n",
        help = "set stack size to n"
    )]
    stack_size: String,

    #[arg(help = "file to execute")]
    filename: String,

    #[arg(
        allow_hyphen_values = true,
        trailing_var_arg = true,
        help = "arguments to pass to script"
    )]
    script_args: Vec<String>,
}

fn parse_byte_size(ss: &str) -> Option<usize> {
    let i = ss
        .as_bytes()
        .iter()
        .position(|c| !c.is_ascii_digit())
        .unwrap_or(ss.len());

    if i > 0
        && let Ok(num) = ss[0..i].parse::<usize>()
    {
        if i == ss.len() - 1 {
            match ss.as_bytes()[i] {
                b'k' | b'K' => Some(num << 10),
                b'm' | b'M' => Some(num << 20),
                b'g' | b'G' => Some(num << 30),
                _ => None,
            }
        } else if i == ss.len() {
            Some(num)
        } else {
            None
        }
    } else {
        None
    }
}

fn main() -> ExitCode {
    #[cfg(debug_assertions)]
    {
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open("debug.log")
            .unwrap();

        let tee = TeeWriter::new(Some(log_file), Option::<std::fs::File>::None);

        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Trace)
            //.filter_level(log::LevelFilter::Debug)
            .target(env_logger::Target::Pipe(Box::new(tee)))
            .init();
    }

    let args = Args::parse();

    let filename = args.filename;
    let script = match std::fs::read_to_string(&filename) {
        Ok(s) => s,
        Err(e) => {
            colorize_println(format!("can't load file: {e}"), RedFg);
            std::process::exit(-1);
        }
    };

    // Create runtime
    let mut rt = Runtime::create().unwrap();

    // In release build, when set panic="abort" in Cargo.toml,
    // should install panic handler to ensure gc heap deallocate when panic.
    rt.install_panic_handler();

    // memory-limit
    {
        let ss = args.memory_limit;
        if let Some(size) = parse_byte_size(&ss) {
            rt.gc_set_memory_limit(size);
        } else {
            colorize_println(format!("invalid memory-limit: {ss}"), RedFg);
            std::process::exit(-1);
        }
    }

    // stack-size
    {
        let ss = args.stack_size;
        if let Some(size) = parse_byte_size(&ss) {
            rt.set_stack_size(size);
        } else {
            colorize_println(format!("invalid stack-size: {ss}"), RedFg);
            std::process::exit(-1);
        }
    }

    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

    // 'os' module has gc objects inside which should be traced.
    // install customized gc trace handler provided by 'os' module.
    rt.gc_set_trace_handler(|tr| {
        mod_os_gc_trace(tr);
    });

    // register native class 'File'
    let class_id_file = JSFile::register(rt.clone()).unwrap();

    let is_module =
        args.module || filename.ends_with(".mjs") || ModuleDef::detect_module(script.as_bytes());

    // create context and setup
    let mut ctx = rt
        .create_context(|cx| {
            Context::ECMA_CONTEXT_SETUP(cx)?;

            // use File class
            cx.use_class(class_id_file).unwrap();

            // register native modules
            init_mod_std(cx).unwrap();
            init_mod_os(cx).unwrap();

            setup_globals(cx, &args.script_args)
        })
        .unwrap();

    #[cfg(debug_assertions)]
    debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 0, "no scope now");

    let exit_code = ctx.with_new_scope(|mut scope| -> ExitCode {
        let ret = scope.eval_wait(
            script.as_bytes(),
            &filename,
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            EvalFlags::empty(),
        );

        match ret {
            Ok(val) => {
                colorize_println(val.inspect(&mut scope).unwrap(), GreenFg);
                ExitCode::SUCCESS
            }
            Err(e) => {
                let mut err_msg = e.message;
                if let Some(tb) = e.traceback {
                    err_msg = format!("{err_msg}\n{tb}");
                }
                colorize_println(err_msg, RedFg);
                ExitCode::FAILURE
            }
        }
    });

    #[cfg(debug_assertions)]
    debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 0);

    exit_code
}

pub type JsInitModuleFn = fn(&mut ScopedContext, &str) -> Result<ModuleDef, i32>;

fn js_module_loader(
    ctx: &mut ScopedContext,
    file_name: &str,
    _: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    if file_name.ends_with(".dylib") || file_name.ends_with(".so") || file_name.ends_with(".dll") {
        let lib = unsafe { Library::new(file_name) }.map_err(|_| {
            ctx.throw_reference_error(&format!(
                "could not load module filename '{file_name}' as shared library"
            ));
            -1
        })?;

        unsafe {
            let init_fn = match lib.get::<JsInitModuleFn>(b"js_init_module\0") {
                Ok(sym) => sym,
                Err(_) => {
                    ctx.throw_reference_error(&format!(
                        "could not load module filename '{file_name}': js_init_module not found"
                    ));
                    return Err(-1);
                }
            };

            match init_fn(ctx, file_name) {
                Ok(m) => {
                    // avoid to close the library
                    // TODO: save the lib handle somewhere
                    std::mem::forget(lib);
                    Ok(m)
                }
                Err(e) => {
                    log::error!("could not load library '{file_name}': initialization error");
                    Err(e)
                }
            }
        }
    } else {
        // js file
        match std::fs::read(file_name) {
            Ok(buf) => ctx.compile_module(&buf, file_name, false),
            Err(_) => {
                ctx.throw_reference_error(&format!("could not load module '{file_name}'"));
                Err(-1)
            }
        }
    }
}
