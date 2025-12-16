#![allow(non_snake_case)]

mod class_file;
mod global;
mod mod_os;
mod mod_std;

use std::process::ExitCode;

use colorized::{Colors::*, colorize_println};
use zuqe::{
    context::Context,
    eval::{EvalFlags, EvalType},
    module::ModuleDef,
    parse::JSParseState,
    runtime::Runtime,
};

use crate::{
    class_file::JSFile,
    global::setup_globals,
    mod_os::{init_mod_os, mod_os_gc_trace},
    mod_std::init_mod_std,
};

fn main() -> ExitCode {
    unsafe {
        std::env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 {
        println!("argument required");
        std::process::exit(-1);
    }

    let filename = args.get(1).unwrap().clone();
    let script = std::fs::read_to_string(&filename).unwrap();

    let mut rt = Runtime::create().unwrap();

    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());
    rt.gc_set_trace_handler(|tracer| {
        mod_os_gc_trace(tracer);
    });

    // register user defined classes
    let class_id_file = JSFile::register(&mut rt).unwrap();

    // create context
    let mut ctx = rt.create_context_ecma().unwrap();
    ctx.use_class(class_id_file).unwrap();

    // register native modules
    init_mod_std(ctx).unwrap();
    init_mod_os(ctx).unwrap();

    setup_globals(ctx).unwrap();

    let is_module = filename.ends_with(".mjs") || JSParseState::detect_module(script.as_bytes());
    let cx = ctx;

    let res = ctx.run_wait_with(
        script.as_bytes(),
        &filename,
        if is_module {
            EvalType::Module
        } else {
            EvalType::Global
        },
        EvalFlags::empty(),
        |val| {
            colorize_println(val.inspect(&cx).unwrap(), GreenFg);
        },
    );

    if let Err(text) = res {
        let mut err_msg = text.message;
        if let Some(tb) = text.traceback {
            err_msg = format!("{err_msg}\n{tb}");
        }
        colorize_println(err_msg, RedFg);
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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
