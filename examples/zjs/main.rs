// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_snake_case)]

mod class_emitter;
mod class_file;
mod global;
mod mod_os;
mod mod_std;

use std::{io::Write, process::ExitCode};

use clap::Parser;
use colorized::{Colors::*, colorize_println};
use libloading::Library;
use zuqe::{
    Context, EvalFlags, EvalType, Exception, ModuleDef, Runtime, ScopedContext,
    builtins::promise::Promise,
};

use crate::{
    class_emitter::JSEmitter,
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

fn run_zjs_prepare() -> (Runtime, Context<'static>, Args, String, bool) {
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

    let script = match std::fs::read_to_string(&args.filename) {
        Ok(s) => s,
        Err(e) => {
            colorize_println(format!("can't load file: {e}"), RedFg);
            std::process::exit(-1);
        }
    };

    let (mut rt, is_module) = {
        let mut rt = Runtime::create().unwrap();

        rt.install_panic_handler();

        {
            let ss = &args.memory_limit;
            if let Some(size) = parse_byte_size(ss) {
                rt.gc_set_memory_limit(size);
            } else {
                colorize_println(format!("invalid memory-limit: {ss}"), RedFg);
                std::process::exit(-1);
            }
        }

        {
            let ss = &args.stack_size;
            if let Some(size) = parse_byte_size(ss) {
                rt.set_stack_size(size);
            } else {
                colorize_println(format!("invalid stack-size: {ss}"), RedFg);
                std::process::exit(-1);
            }
        }

        rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

        rt.gc_set_trace_handler(|tr| {
            mod_os_gc_trace(tr);
        });

        let is_module = args.module
            || args.filename.ends_with(".mjs")
            || ModuleDef::detect_module(script.as_bytes());
        (rt, is_module)
    };

    let class_id_file = JSFile::register(&mut rt).unwrap();
    let class_id_emitter = JSEmitter::register(&mut rt).unwrap();

    let ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;

            cx.use_class(class_id_file).unwrap();
            cx.use_class(class_id_emitter).unwrap();

            init_mod_std(cx).unwrap();
            init_mod_os(cx).unwrap();

            setup_globals(cx, &args.script_args)
        })
        .unwrap();

    #[cfg(debug_assertions)]
    debug_assert_eq!(
        ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()),
        1,
        "top-level scope exists"
    );

    (rt, ctx, args, script, is_module)
}

#[cfg(not(feature = "async_support"))]
fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    let exit_code = ctx.with_new_scope(|mut scope| {
        let result = scope.eval_wait(
            script.as_bytes(),
            &args.filename,
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            EvalFlags::empty(),
        );

        let code = match result {
            Ok(val) => {
                colorize_println(val.inspect(scope.rt_mut()).unwrap(), GreenFg);
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
        };
        scope.return_with(code)
    });

    #[cfg(debug_assertions)]
    debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 1);

    exit_code
}

#[cfg(feature = "async_support")]
#[tokio::main]
async fn main() -> ExitCode {
    let (mut rt, mut ctx, args, script, is_module) = run_zjs_prepare();

    let mut promise_future = None;

    let res = ctx.with_new_scope(|mut scope| {
        let result = scope.eval(
            script.as_bytes(),
            &args.filename,
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            EvalFlags::empty(),
        );

        let r = match result {
            Ok(val) => {
                if let Ok(promise) = Promise::try_from(val) {
                    match promise.into_future(&mut scope) {
                        Ok(fut) => {
                            promise_future = Some(fut);
                            Ok(())
                        }
                        Err(_) => Err(scope.take_exception().to_text(&mut scope)),
                    }
                } else {
                    colorize_println(val.inspect(&mut scope).unwrap(), GreenFg);
                    Ok(())
                }
            }
            Err(_) => Err(scope.take_exception().to_text(&mut scope)),
        };
        scope.return_with(r)
    });

    #[cfg(debug_assertions)]
    debug_assert_eq!(
        ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()),
        1,
        "top-level scope exists"
    );

    if let Err(e) = res {
        let mut err_msg = e.message;
        if let Some(tb) = e.traceback {
            err_msg = format!("{err_msg}\n{tb}");
        }
        colorize_println(err_msg, RedFg);
        return ExitCode::FAILURE;
    }

    let run_result = if let Some(js_fut) = promise_future {
        let (rt_res, promise_res) = tokio::join!(rt.run_async(), js_fut);

        // print result or error
        ctx.with_root_scope(|mut cx| match promise_res {
            Ok(val) => {
                colorize_println(val.inspect(&mut cx).unwrap(), GreenFg);
            }
            Err(reason) => {
                let err_msg = reason
                    .to_string(&mut cx)
                    .unwrap()
                    .encode_utf8(false)
                    .to_string();
                colorize_println(err_msg, RedFg);
            }
        });

        rt_res
    } else {
        rt.run_async().await
    };

    match run_result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Exception::OutOfMemory) => {
            colorize_println("out of memory", RedFg);
            ExitCode::FAILURE
        }
        Err(Exception::Interrupted(code)) => code,
        Err(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                colorize_println(err.to_string(&mut cx).unwrap().encode_utf8(false), RedFg);
            });
            ExitCode::FAILURE
        }
        Err(Exception::No | Exception::TooManyClasses) => {
            unreachable!();
        }
    }
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
