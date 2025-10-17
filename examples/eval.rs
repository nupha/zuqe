use std::{path::PathBuf, time::Instant};

use colorized::*;
use zhuque::{
    JS_CFUNC_NATIVE_DEF, JS_PROP_INT32_DEF,
    context::JSContext,
    error::JSResult,
    eval::JS_EVAL_TYPE_GLOBAL,
    function::{CallContext, FieldDef, NativeFunctionType},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JS_SetModuleLoaderFunc, ModuleDef},
    object::JS_PROP_C_W_E,
    parse::JSParseState,
    promise::Promise,
    runtime::JSRuntime,
    value::JSValue,
};

fn js_print(ctx: &JSContext, cc: CallContext) -> JSResult {
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
                format!("{}", arg.inspect(ctx)?)
            } else {
                format!("{}", arg.to_string(ctx)?)
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
    let mut rt = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()).unwrap();

    unsafe {
        JS_SetModuleLoaderFunc(rt.as_mut(), None, js_module_loader, std::ptr::null_mut());
    }

    // the default context
    let ctx = rt.create_context().unwrap();

    // the customized ctx
    // let ctx = js
    //     .create_context_customize(|c| {
    //         c.init_core_objects_ex(true, true, true);
    //         //c.intrinsic_base_objects();

    //         c.intrinsic_global_object(&[]);
    //         c.intrinsic_async();
    //         c.intrinsic_typed_arrays();
    //         // c.intrinsic_math();

    //         Ok(())
    //     })
    //     .unwrap();

    // register native modules
    ctx.register_native_module("std", &MOD_DEMO_FUNCS).unwrap();

    // setup globals
    {
        let global = JSValue::Object(ctx.global_obj());

        let printf = NativeFunctionType::Func(js_print);
        global
            .set_property_str(
                &ctx,
                "print",
                ctx.create_native_function(printf, "print", 0, 0).unwrap(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "debug",
                ctx.create_native_function(printf, "debug", 0, 1).unwrap(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "info",
                ctx.create_native_function(printf, "info", 0, 2).unwrap(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "warn",
                ctx.create_native_function(printf, "warn", 0, 3).unwrap(),
            )
            .unwrap();
        global
            .set_property_str(
                &ctx,
                "error",
                ctx.create_native_function(printf, "error", 0, 4).unwrap(),
            )
            .unwrap();

        let f = ctx
            .create_native_function(
                NativeFunctionType::Func(|cx, _: CallContext| {
                    let ctx1 = cx.rt_mut().create_context()?;
                    let global = JSValue::Object(ctx1.global_obj());
                    Ok(global)
                }),
                "createRealm",
                1,
                0,
            )
            .unwrap();
        global.set_property_str(&ctx, "createRealm", f).unwrap();

        let console = ctx.create_base_object().unwrap();
        console
            .define_property_value_str(
                &ctx,
                "log",
                ctx.create_native_function(printf, "print", 0, 0).unwrap(),
                JS_PROP_C_W_E,
            )
            .unwrap();
        console
            .define_property_value_str(
                &ctx,
                "inspect",
                ctx.create_native_function(printf, "inspect", 0, 100)
                    .unwrap(),
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
                    NativeFunctionType::Func(|ctx: &JSContext, cc: CallContext| -> JSResult {
                        if let Some(ab) = cc.arg(0).as_object_ArrayBuffer() {
                            ab.detatch(ctx);
                        }
                        Ok(JSValue::UNDEFINED)
                    }),
                    "detachArrayBuffer",
                    1,
                    0,
                )
                .unwrap(),
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
            println!(
                ">>> RESULT: {v:?}, {}",
                v.inspect(&ctx).unwrap_or("<can't repr>".to_string())
            );
            if let JSValue::Object(o) = &v {
                println!("    OBJECT DUMP");
                o.dump(ctx.rt());
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

                        let exc = ctx.exception();
                        debug_assert!(!exc.is_undefined_or_null()); // FIXME: this assert fails!

                        println!("!!! EXCEPTION {exc:?}: {:?}", exc.to_string(&ctx));
                        if let JSValue::Object(o) = exc {
                            o.dump(ctx.rt());
                        };
                        break;
                    }
                }
            }
        }
        Err(_) => {
            let exc = ctx.exception();
            debug_assert!(!exc.is_undefined_or_null());

            println!("!!! EXCEPTION {exc:?}: {:?}", exc.to_string(&ctx));
            if let JSValue::Object(o) = exc {
                o.dump(ctx.rt());
            };
        }
    }

    println!("*** run use time: {} ms", tm.elapsed().as_millis());
}

fn js_module_loader(
    ctx: &JSContext,
    module_name: &str,
    _: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    match std::fs::read(module_name) {
        Ok(buf) => {
            let md = ModuleDef::compile(ctx, &buf, module_name, false)?;
            Ok(md)
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
    }
}

pub fn js_module_set_import_meta(
    ctx: &JSContext,
    md: &ModuleDef,
    use_realpath: i32,
    is_main: bool,
) -> i32 {
    // make module name to absolute url
    let module_name_atom = md.module_name().dup(&ctx.rt());
    let module_name = module_name_atom.to_utf8(ctx.rt());
    module_name_atom.free(ctx.rt_mut());

    let p = PathBuf::from(module_name);
    let mut normalized = PathBuf::new();
    for component in p.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => {
                normalized.push(component.as_os_str());
            }
        }
    }
    let aurl = format!("file://{}", normalized.display());

    let meta_obj = if let Ok(m) = md.import_meta(ctx) {
        m
    } else {
        return -1;
    };

    meta_obj
        .define_property_value_str(
            ctx,
            "url",
            ctx.create_string_from_utf8(aurl.into_bytes())
                .unwrap()
                .into(),
            JS_PROP_C_W_E,
        )
        .unwrap();

    meta_obj
        .define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)
        .unwrap();

    0
}

fn js_std_exit(ctx: &JSContext, cc: CallContext) -> JSResult {
    println!("*** exit called");
    Ok(JSValue::UNDEFINED)
}

static MOD_DEMO_FUNCS: [FieldDef; 2] = [
    JS_CFUNC_NATIVE_DEF!("exit", 1, js_std_exit, 0),
    JS_PROP_INT32_DEF!("out", 9, JS_PROP_C_W_E),
];
