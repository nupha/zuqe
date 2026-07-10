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
use url::Url;
use zuqe::{
    Context, EvalFlags, EvalType, Exception, ExitStatus, JSValue, ModuleDef, Runtime,
    ScopedContext, builtins::promise::Promise,
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
        value_name = "n",
        help = "set memory limit to n (default: unlimited)"
    )]
    memory_limit: Option<String>,

    #[arg(
        long,
        value_name = "n",
        help = "set stack arena size to n (default: 256k)"
    )]
    stack_arena: Option<String>,

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

        if let Some(ss) = &args.memory_limit {
            if let Some(size) = parse_byte_size(ss) {
                rt.gc_set_memory_limit(size);
            } else {
                colorize_println(format!("invalid memory-limit: {ss}"), RedFg);
                std::process::exit(-1);
            }
        }

        if let Some(ss) = &args.stack_arena {
            if let Some(size) = parse_byte_size(ss) {
                if rt.set_stack_arena(size).is_err() {
                    colorize_println(format!("cannot allocate stack-arena of size {ss}"), RedFg);
                    std::process::exit(-1);
                }
            } else {
                colorize_println(format!("invalid stack-arena: {ss}"), RedFg);
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

    // Convert the entry filename to a canonical `file://` URL. `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required so
    // that dynamic `import()` from a *script* entry resolves relative specifiers
    // correctly instead of being handed a bare filesystem path.
    let entry_name = zuqe::path_to_url(&args.filename)
        .expect("entry path must convert to a file:// URL")
        .to_string();

    let exit_code = ctx.with_new_scope(|mut scope| {
        let result = scope.eval_wait(
            script.as_bytes(),
            &entry_name,
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
        code
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

    // Convert the entry filename to a canonical `file://` URL when running a
    // Convert the entry filename to a canonical `file://` URL. `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required so
    // that dynamic `import()` from a *script* entry resolves relative specifiers
    // correctly instead of being handed a bare filesystem path.
    let entry_name = zuqe::path_to_url(&args.filename)
        .expect("entry path must convert to a file:// URL")
        .to_string();

    let result = ctx.eval(
        script.as_bytes(),
        &entry_name,
        if is_module {
            EvalType::Module
        } else {
            EvalType::Global
        },
        EvalFlags::empty(),
    );
    let res = ctx.with_root_scope(|mut cx| match result {
        Ok(val) => {
            if let Ok(promise) = Promise::try_from(val) {
                match promise.into_future(&mut cx) {
                    Ok(fut) => {
                        promise_future = Some(fut);
                        Ok(())
                    }
                    Err(_) => Err(cx.take_exception().to_text(&mut cx)),
                }
            } else {
                colorize_println(val.inspect(&mut cx).unwrap(), GreenFg);
                Ok(())
            }
        }
        Err(_) => Err(cx.take_exception().to_text(&mut cx)),
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
        ExitStatus::Value(_) => ExitCode::SUCCESS,
        ExitStatus::Interrupted(code) => {
            #[cfg(debug_assertions)]
            log::debug!("Process interrupted with code {:?}", code);
            code
        }
        ExitStatus::Exception(Exception::OutOfMemory) => {
            colorize_println("out of memory", RedFg);
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::Error(err)) => {
            ctx.with_root_scope(|mut cx| {
                colorize_println(err.to_string(&mut cx).unwrap().encode_utf8(false), RedFg);
            });
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::No | Exception::TooManyClasses) => {
            unreachable!();
        }
    }
}

pub type JsInitModuleFn = fn(&mut ScopedContext, &str) -> Result<ModuleDef, i32>;

fn js_module_loader(
    ctx: &mut ScopedContext,
    file_name: &Url,
    attrs: JSValue,
    _: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    // `file_name` is a canonical `file://` URL. Convert it back to a local path
    // for filesystem access; dispatch by scheme here for future `https:` support.
    let path = match zuqe::url_to_abspath(file_name) {
        Some(p) => p,
        None => {
            ctx.throw_reference_error(&format!(
                "could not load module '{file_name}': not a file:// URL"
            ));
            return Err(-1);
        }
    };

    let is_native_lib = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e, "dylib" | "so" | "dll"));

    if is_native_lib {
        let lib = unsafe { Library::new(&path) }.map_err(|_| {
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

            // Pass the URL (`file_name`) as the module name so the loaded native
            // module's identity matches the cache key used for lookup.
            match init_fn(ctx, file_name.as_str()) {
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
        // js file — check for import attributes: type: "json"
        let is_json = attrs.is_object()
            && attrs
                .get_property_str(ctx, "type")
                .map(|v| {
                    v.as_string()
                        .is_some_and(|s| s.content().encode_utf8(false).as_ref() == "json")
                })
                .unwrap_or(false);

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                if is_json {
                    // JSON module: wrap as `export default <json>;\n`
                    let mut src = String::from("export default ");
                    src.push_str(&content);
                    src.push('\n');
                    ctx.compile_module(src.as_bytes(), file_name.as_str(), false)
                } else {
                    ctx.compile_module(content.as_bytes(), file_name.as_str(), false)
                }
            }
            Err(_) => {
                ctx.throw_reference_error(&format!("could not load module '{file_name}'"));
                Err(-1)
            }
        }
    }
}
