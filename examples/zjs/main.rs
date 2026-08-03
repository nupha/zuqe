// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_snake_case)]

mod class_emitter;
mod class_file;
mod global;
mod inspect;
mod mod_os;
mod mod_std;

use std::{io::Write, process::ExitCode};

use clap::Parser;
use colorized::{Colors::*, colorize_println};
use libloading::Library;
use url::Url;
use zuqe::{
    Context, EvalFlags, EvalSource, EvalType, Exception, ExitStatus, JSValue, ModuleDef, Runtime,
    ScopedContext,
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
        help = "set virtual stack arena size to n (default: 256k)"
    )]
    stack_arena: Option<String>,

    #[arg(
        long,
        value_name = "n",
        help = "set virtual stack byte limit to n (default: 512k)"
    )]
    stack_limit: Option<String>,

    #[arg(
        long,
        value_name = "n",
        help = "set C-stack recursion limit to n (default: 512k)"
    )]
    stack_overflow: Option<String>,

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

        if let Some(ss) = &args.stack_overflow {
            if let Some(size) = parse_byte_size(ss) {
                rt.set_stack_overflow(size);
            } else {
                colorize_println(format!("invalid stack-overflow: {ss}"), RedFg);
                std::process::exit(-1);
            }
        }

        if let Some(ss) = &args.stack_limit {
            if let Some(size) = parse_byte_size(ss) {
                rt.set_stack_limit(size);
            } else {
                colorize_println(format!("invalid stack-limit: {ss}"), RedFg);
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
                colorize_println(
                    inspect::js_inspect_value(&mut scope, val, &inspect::InspectOptions::default()),
                    GreenFg,
                );
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

    // Convert the entry filename to a canonical `file://` URL when running a
    // Convert the entry filename to a canonical `file://` URL. `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required so
    // that dynamic `import()` from a *script* entry resolves relative specifiers
    // correctly instead of being handed a bare filesystem path.
    let entry_name =
        zuqe::path_to_url(&args.filename).expect("entry path must convert to a file:// URL");

    // Eval the entry and drive the event loop to completion. `eval_wait`
    // handles top-level module Promise observation internally via
    // `futures_util::future::select` (never `tokio::join!`), so a rejected
    // top-level promise is surfaced as `ExitStatus::Exception` and never
    // silently dropped. A synchronous (non-Promise) result is returned as
    // `ExitStatus::Value` for the host to print.
    let run_result = ctx
        .eval_wait(
            &mut rt,
            script.as_bytes(),
            EvalSource::BaseUrl(entry_name),
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            EvalFlags::empty(),
        )
        .await;

    match run_result {
        ExitStatus::Value(v) => {
            // A module (or a script with no meaningful tail value) evaluates to
            // `undefined`; printing it is just noise. Only surface real values,
            // matching how a REPL/CLI treats an absent result.
            if !v.is_undefined() {
                ctx.with_root_scope(|mut cx| {
                    colorize_println(
                        inspect::js_inspect_value(&mut cx, v, &inspect::InspectOptions::default()),
                        GreenFg,
                    );
                });
            }
            ExitCode::SUCCESS
        }
        ExitStatus::Interrupted(code) => {
            #[cfg(debug_assertions)]
            log::debug!("Process interrupted with code {:?}", code);
            code
        }
        ExitStatus::Exception(Exception::OutOfMemory) => {
            colorize_println("out of memory", RedFg);
            ExitCode::FAILURE
        }
        ExitStatus::Exception(Exception::TooManyAtoms) => {
            colorize_println("too many atoms", RedFg);
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
            return Err(ctx.throw_reference_error(&format!(
                "could not load module '{file_name}': not a file:// URL"
            )));
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
            ))
        })?;

        unsafe {
            let init_fn = match lib.get::<JsInitModuleFn>(b"js_init_module\0") {
                Ok(sym) => sym,
                Err(_) => {
                    return Err(ctx.throw_reference_error(&format!(
                        "could not load module filename '{file_name}': js_init_module not found"
                    )));
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
        // js file — check for import attributes
        let attr_type = get_attr_type(ctx, attrs);

        match std::fs::read_to_string(&path) {
            Ok(content) => match attr_type.as_deref() {
                Some("json") => {
                    // JSON module: wrap as `export default <json>;\n`
                    let mut src = String::from("export default ");
                    src.push_str(&content);
                    src.push('\n');
                    ctx.compile_module(src.as_bytes(), file_name, false)
                }
                Some("text") => {
                    // Text module: wrap as `export default "<escaped>";\n`
                    let mut src = String::from("export default \"");
                    src.push_str(&escape_js_string(&content));
                    src.push_str("\";\n");
                    ctx.compile_module(src.as_bytes(), file_name, false)
                }
                Some(other) => Err(
                    ctx.throw_type_error(&format!("unsupported import attribute type '{}'", other))
                ),
                None => ctx.compile_module(content.as_bytes(), file_name, false),
            },
            Err(_) => {
                Err(ctx.throw_reference_error(&format!("could not load module '{file_name}'")))
            }
        }
    }
}

/// Extract the `type` value from import attributes, returning `None` if
/// the attrs object is absent or has no `type` property.
fn get_attr_type(ctx: &mut ScopedContext, attrs: JSValue) -> Option<String> {
    if !attrs.is_object() {
        return None;
    }
    attrs.get_property_str(ctx, "type").ok().and_then(|v| {
        v.as_string(true)
            .map(|s| s.content().encode_utf8(false).to_string())
    })
}

/// Escape a string for use as a JS string literal (double-quoted).
fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x00'..='\x1f' => out.push_str(&format!("\\x{:02x}", ch as u8)),
            c => out.push(c),
        }
    }
    out
}
