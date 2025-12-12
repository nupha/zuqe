#![allow(non_snake_case)]

mod class_file;
mod global;
mod mod_os;
mod mod_std;

use std::process::ExitCode;

use colorized::{Colors::*, colorize_println};
use zhuque::{
    atom::Atom,
    builtins::promise::{Promise, PromiseState},
    context::Context,
    eval::JS_EVAL_TYPE_GLOBAL,
    module::ModuleDef,
    parse::JSParseState,
    runtime::{Runtime, TickStatus},
    value::JSValue,
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

    let res = if filename.ends_with(".mjs") || JSParseState::detect_module(script.as_bytes()) {
        ctx.eval_module(script.as_bytes(), &filename, false)
            .map(|p| p.into())
    } else {
        ctx.eval(script.as_bytes(), &filename, JS_EVAL_TYPE_GLOBAL)
    };
    match res {
        Ok(v) => {
            rt.gc_set_root_value(v, true);

            // #[cfg(debug_assertions)]
            // {
            //     println!(
            //         ">>> RESULT: {v:?}, {}",
            //         v.inspect(&ctx).unwrap_or("<can't repr>".to_string())
            //     );
            //     if let JSValue::Object(o) = &v {
            //         println!("    OBJECT DUMP");
            //         o.dump(ctx.rt_ref());
            //     }
            // }

            let mut promise = Promise::try_from(v).ok();
            let result = if let Some(p) = promise {
                match p.state() {
                    PromiseState::Fulfilled => {
                        promise = None;
                        p.result()
                    }
                    PromiseState::Rejected => {
                        print_exception(ctx, p.result());
                        promise = None;
                        JSValue::UNDEFINED
                    }
                    _ => JSValue::UNDEFINED,
                }
            } else {
                v
            };

            let exit_code = loop {
                match rt.tick() {
                    Ok(TickStatus::Continue) => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Ok(TickStatus::Stop(code)) => {
                        if let Some(p) = promise {
                            match p.state() {
                                PromiseState::Fulfilled => {
                                    colorize_println(
                                        format!("{}", p.result().to_string(&ctx).unwrap()),
                                        GreenFg,
                                    );
                                }
                                PromiseState::Rejected => {
                                    print_exception(ctx, p.result());
                                }
                                _ => {
                                    unreachable!()
                                }
                            }
                        } else {
                            colorize_println(
                                format!("{}", result.to_string(&ctx).unwrap()),
                                GreenFg,
                            );
                        }
                        break code;
                    }
                    Err(_) => {
                        eprintln!("--- Error in tick ---");
                        let exc = ctx.rt_ref().exception();
                        debug_assert!(!exc.is_undefined_or_null()); // FIXME: this assert fails!
                        print_exception(ctx, exc);
                        break -1;
                    }
                }
            };

            if exit_code < 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::from(exit_code as u8)
            }
        }
        Err(_) => {
            let exc = ctx.rt_ref().exception();
            debug_assert!(!exc.is_undefined_or_null());
            rt.gc_set_root_value(exc, true);
            print_exception(ctx, exc);
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

fn print_exception(ctx: Context, r: JSValue) {
    colorize_println(format!("{}", r.to_string(&ctx).unwrap()), RedFg);

    match r.get_property(&ctx, Atom::CONST_stack) {
        Ok(JSValue::NULL) | Ok(JSValue::UNDEFINED) => {}
        Ok(s) => {
            let ss = s.inspect(&ctx).unwrap();
            colorize_println(ss, RedFg);
        }
        Err(_) => {}
    }
}
