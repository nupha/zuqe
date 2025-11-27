#![allow(non_snake_case)]

mod class_file;
mod class_point;
mod mod_std;

use std::time::Instant;

use colorized::*;
use zhuque::{
    builtins::promise::Promise,
    context::ContextRef,
    error::JSResult,
    eval::JS_EVAL_TYPE_GLOBAL,
    function::{CallContext, NativeFunctionType},
    module::{JS_SetModuleLoaderFunc, ModuleDef},
    object::JS_PROP_C_W_E,
    parse::JSParseState,
    runtime::Runtime,
    value::JSValue,
};

use crate::{class_file::JSFile, class_point::register_class_point, mod_std::init_mod_std};

fn js_print(ctx: ContextRef, cc: CallContext) -> JSResult {
    let level = cc.magic();
    let inspect = level == 100;

    let color = || {
        match level {
            1 => Colors::CyanFg, // debug
            2 => Colors::GreenFg,
            3 => Colors::YellowFg,
            4 => Colors::RedBg,
            _ => Colors::WhiteFg,
        }
    };

    for i in 0..cc.length() {
        if i > 0 {
            colorize_print(" ", color());
        }

        let arg = cc.arg(i);

        colorize_print(
            if inspect {
                arg.inspect(&ctx)?.to_string()
            } else {
                format!("{}", arg.to_string(&ctx)?)
            },
            color(),
        );
    }

    colorize_println("", color());
    Ok(JSValue::UNDEFINED)
}

fn main() {
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

    let tm0 = Instant::now();
    let mut rt = Runtime::create().unwrap();

    JS_SetModuleLoaderFunc(&mut rt, None, js_module_loader, std::ptr::null_mut());

    // register user's native classes
    let class_id_point = register_class_point(&mut rt).unwrap();
    let class_id_file = JSFile::register(&mut rt).unwrap();

    // create context
    let mut ctx = rt.create_context_ecma().unwrap();
    ctx.use_class(class_id_point).unwrap();
    ctx.use_class(class_id_file).unwrap();

    // register native modules
    init_mod_std(ctx.ptr()).unwrap();

    // setup globals
    {
        let mut global: JSValue = ctx.global_obj().into();

        let printf = NativeFunctionType::Func(js_print);
        global
            .set_property_str(
                &ctx,
                "print",
                ctx.create_native_function(printf, "print", 0, 0)
                    .unwrap()
                    .into(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "debug",
                ctx.create_native_function(printf, "debug", 0, 1)
                    .unwrap()
                    .into(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "info",
                ctx.create_native_function(printf, "info", 0, 2)
                    .unwrap()
                    .into(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "warn",
                ctx.create_native_function(printf, "warn", 0, 3)
                    .unwrap()
                    .into(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "error",
                ctx.create_native_function(printf, "error", 0, 4)
                    .unwrap()
                    .into(),
            )
            .unwrap();

        let f = ctx
            .create_native_function(
                NativeFunctionType::Func(|mut cx, _: CallContext| {
                    let ctx1 = cx.rt_mut().create_context_ecma()?;
                    let global = JSValue::Object(ctx1.global_obj());
                    Ok(global)
                }),
                "createRealm",
                1,
                0,
            )
            .unwrap();
        global
            .set_property_str(&ctx, "createRealm", f.into())
            .unwrap();

        let mut console = ctx.create_base_object().unwrap();
        console
            .define_property_value_str(
                &ctx,
                "log",
                ctx.create_native_function(printf, "print", 0, 0)
                    .unwrap()
                    .into(),
                JS_PROP_C_W_E,
            )
            .unwrap();
        console
            .define_property_value_str(
                &ctx,
                "inspect",
                ctx.create_native_function(printf, "inspect", 0, 100)
                    .unwrap()
                    .into(),
                JS_PROP_C_W_E,
            )
            .unwrap();
        global
            .define_property_value_str(&ctx, "console", console.into(), JS_PROP_C_W_E)
            .unwrap();

        // detachArrayBuffer
        global
            .set_property_str(
                &ctx,
                "detachArrayBuffer",
                ctx.create_native_function(
                    NativeFunctionType::Func(|ctx: ContextRef, cc: CallContext| -> JSResult {
                        if let Some(mut ab) = cc.arg(0).as_object_ArrayBuffer().copied() {
                            ab.detatch(&ctx);
                        }
                        Ok(JSValue::UNDEFINED)
                    }),
                    "detachArrayBuffer",
                    1,
                    0,
                )
                .unwrap()
                .into(),
            )
            .unwrap();

        global
            .set_property_str(
                &ctx,
                "gc",
                ctx.create_native_function(
                    NativeFunctionType::Func(|mut ctx, _| -> JSResult {
                        ctx.rt_mut().gc_collect();
                        Ok(JSValue::UNDEFINED)
                    }),
                    "gc",
                    0,
                    0,
                )
                .unwrap()
                .into(),
            )
            .unwrap();
    }

    println!("*** prepare use time: {} ms", tm0.elapsed().as_millis());

    println!("start running ...");
    let tm = Instant::now();

    let res = if filename.ends_with(".mjs") || JSParseState::detect_module(script.as_bytes()) {
        ctx.eval_module(script.as_bytes(), &filename, false)
            .map(|p| p.into())
    } else {
        ctx.eval(script.as_bytes(), &filename, JS_EVAL_TYPE_GLOBAL)
    };
    match res {
        Ok(v) => {
            rt.gc_set_root_value(v, true);

            println!(
                ">>> RESULT: {v:?}, {}",
                v.inspect(&ctx).unwrap_or("<can't repr>".to_string())
            );

            #[cfg(debug_assertions)]
            if let JSValue::Object(o) = &v {
                println!("    OBJECT DUMP");
                o.dump(ctx.rt_ref());
            }

            let promise = Promise::try_from(v).ok();

            loop {
                match rt.tick() {
                    Ok(true) => {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                        continue;
                    }
                    Ok(false) => {
                        if let Some(pr) = promise {
                            let r = pr.result();
                            println!("promise state:  {:?}", pr.state());
                            println!("promise result: {r:?}, {:?}", r.inspect(&ctx));
                            println!("{:?}", r.to_string(&ctx));
                        }
                        break;
                    }
                    Err(_) => {
                        println!("--- Error in promise job ---");

                        let exc = ctx.rt_ref().exception();
                        debug_assert!(!exc.is_undefined_or_null()); // FIXME: this assert fails!

                        println!("!!! EXCEPTION {exc:?}: {:?}", exc.to_string(&ctx));

                        #[cfg(debug_assertions)]
                        if let JSValue::Object(o) = exc {
                            o.dump(ctx.rt_ref());
                        };
                        break;
                    }
                }
            }
        }
        Err(_) => {
            let exc = ctx.rt_ref().exception();
            debug_assert!(!exc.is_undefined_or_null());
            rt.gc_set_root_value(exc, true);

            println!(
                "!!! EXCEPTION {exc:?}: {:?}",
                ctx.rt_ref().exception().to_string(&ctx)
            );

            #[cfg(debug_assertions)]
            if let JSValue::Object(o) = exc {
                o.dump(ctx.rt_ref());
            };
        }
    }

    println!("*** run use time: {} ms", tm.elapsed().as_millis());
}

fn js_module_loader(
    mut ctx: ContextRef,
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

// pub fn js_module_set_import_meta(
//     ctx: &ContextImpl,
//     md: &ModuleDef,
//     use_realpath: i32,
//     is_main: bool,
// ) -> i32 {
//     // make module name to absolute url
//     let module_name_atom = md.module_name();
//     let module_name = module_name_atom.to_utf8();

//     let p = PathBuf::from(module_name);
//     let mut normalized = PathBuf::new();
//     for component in p.components() {
//         match component {
//             std::path::Component::ParentDir => {
//                 normalized.pop();
//             }
//             std::path::Component::CurDir => {}
//             _ => {
//                 normalized.push(component.as_os_str());
//             }
//         }
//     }
//     let aurl = format!("file://{}", normalized.display());

//     let meta_obj = if let Ok(m) = md.import_meta(ctx) {
//         m
//     } else {
//         return -1;
//     };

//     meta_obj
//         .define_property_value_str(
//             ctx,
//             "url",
//             ctx.create_string_from_utf8(aurl.into_bytes())
//                 .unwrap()
//                 .into(),
//             JS_PROP_C_W_E,
//         )
//         .unwrap();

//     meta_obj
//         .define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)
//         .unwrap();

//     0
// }
