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
use zuqe::{Context, EvalFlags, EvalType, ModuleDef, Runtime};

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

    let mut rt = Runtime::create().unwrap();

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
    // module os has internal gc objects need customized trace & collect.
    rt.gc_set_trace_handler(|tr| {
        mod_os_gc_trace(tr);
    });

    // register native class 'File'
    let class_id_file = JSFile::register(rt.clone()).unwrap();

    let is_module =
        args.module || filename.ends_with(".mjs") || ModuleDef::detect_module(script.as_bytes());

    // create context
    let mut ctx = rt
        .create_context(|ctx| {
            Context::ECMA_CONTEXT_SETUP(ctx)?;

            // use File class
            ctx.use_class(class_id_file).unwrap();

            // register native modules
            init_mod_std(ctx).unwrap();
            init_mod_os(ctx).unwrap();

            setup_globals(ctx, &args.script_args).unwrap();
            Ok(())
        })
        .unwrap();

    let mut scope = ctx.scope();
    match scope.run_wait(
        script.as_bytes(),
        &filename,
        if is_module {
            EvalType::Module
        } else {
            EvalType::Global
        },
        EvalFlags::empty(),
    ) {
        Ok(val) => {
            colorize_println(val.inspect(&scope).unwrap(), GreenFg);
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
}

fn js_module_loader(
    mut ctx: Context,
    module_name: &str,
    _: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    match std::fs::read(module_name) {
        Ok(buf) => {
            let md = ModuleDef::compile(&mut ctx, &buf, module_name, false)?;
            Ok(md)
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
    }
}
