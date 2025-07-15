use std::{ffi::CString, path::PathBuf};

use zhuque::{
    atom::JS_FreeAtom,
    context::JSContext,
    error::{JS_ThrowReferenceError, JSResult},
    eval::{
        JS_EVAL_FLAG_COMPILE_ONLY, JS_EVAL_FLAG_STRICT, JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MASK,
        JS_EVAL_TYPE_MODULE,
    },
    function::{CallContext, NativeFunctionType},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JS_SetModuleLoaderFunc, ModuleDef},
    object::JS_PROP_C_W_E,
    parse::JS_DetectModule,
    promise::Promise,
    runtime::JSRuntime,
    value::JSValue,
};

fn js_print(ctx: &JSContext, args: CallContext) -> JSResult {
    for i in 0..args.length() {
        if i > 0 {
            print!(" ");
        }
        let arg = args.get_raw(i);
        print!("{}", arg.coerce_to_utf8(ctx)?);
    }
    println!();

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

    unsafe {
        if let Ok(mut js) = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()) {
            JS_SetModuleLoaderFunc(js.as_mut(), None, js_module_loader, std::ptr::null_mut());

            let ctx = js.create_context().unwrap();

            {
                let global = JSValue::Object(ctx.global_obj());
                global
                    .set_property_str(
                        &ctx,
                        "print",
                        ctx.create_native_function(
                            NativeFunctionType::Func(js_print),
                            "print",
                            1,
                            0,
                        )
                        .unwrap(),
                    )
                    .unwrap();
            }

            let eval_flags = if filename.ends_with(".mjs") {
                JS_EVAL_TYPE_MODULE
            } else if JS_DetectModule(script.as_bytes()) {
                JS_EVAL_TYPE_MODULE
            } else {
                JS_EVAL_TYPE_GLOBAL
            };

            let r = if eval_flags & JS_EVAL_TYPE_MASK == JS_EVAL_TYPE_MODULE {
                // for the modules, we compile then run to be able to set import.meta
                let esm_func = ctx.eval(
                    script.as_bytes(),
                    &filename,
                    eval_flags | JS_EVAL_FLAG_COMPILE_ONLY,
                );
                if !esm_func.is_exception() {
                    debug_assert!(esm_func.is_module());
                    js_module_set_import_meta(&ctx, &esm_func, 1, true);
                    if let JSValue::Module(md) = esm_func {
                        md.eval(&ctx)
                    } else {
                        unreachable!()
                    }
                } else {
                    JSValue::EXCEPTION
                }
            } else {
                ctx.eval(script.as_bytes(), &filename, eval_flags)
            };

            loop {
                if let Ok(None) = js.execute_job_once() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            if r.is_exception() {
                let exc = ctx.take_exception();
                debug_assert!(!exc.is_undefined_or_null());

                println!("!!! EXCEPTION {exc:?} {}", exc.repr());

                // println!("{:?}", exc.coerce_to_string(&ctx));
                if let JSValue::Object(o) = exc {
                    o.dump(ctx.rt());
                };
            } else {
                println!(">>> RESULT: {r:?}, {}", r.repr());

                if let Ok(pr) = Promise::try_from(r) {
                    let v = pr.result();
                    println!("promise state: {:?}", pr.state());
                    println!("promise result: {v:?}");
                }
            }

            // JS_DumpGCObjects(js.as_mut());
            // JS_DumpAtoms(js.as_mut());

            // println!("================ free eval result");
            // JS_DumpGCObjects(js.as_mut());
            // JS_DumpAtoms(js.as_mut());

            // drop(ctx);
            // drop(js);
        } else {
            println!("can't create runtime");
        }
    }
}

pub fn js_module_loader(
    ctx: &JSContext,
    module_name: &str,
    opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    match std::fs::read(module_name) {
        Ok(buf) => {
            // compile the module
            let func_val = ctx
                .eval(
                    buf.as_slice(),
                    module_name,
                    JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY,
                )
                .check_exception()?;

            // XXX: could propagate the exception
            unsafe {
                js_module_set_import_meta(ctx, &func_val, true as i32, false);
            }

            if let JSValue::Module(m) = func_val {
                Ok(m)
            } else {
                unreachable!()
            }
        }
        Err(_) => {
            JS_ThrowReferenceError(
                ctx,
                &format!("could not load module filename '{module_name}'"),
            );
            Err(-1)
        }
    }
}

pub unsafe fn js_module_set_import_meta(
    ctx: &JSContext,
    func_val: &JSValue,
    use_realpath: i32,
    is_main: bool,
) -> i32 {
    if let JSValue::Module(m) = func_val {
        // make module name to absolute url
        let module_name_atom = m.module_name_atom().dup(&ctx.rt());
        let module_name = module_name_atom.to_utf8_string(&ctx);
        JS_FreeAtom(ctx, module_name_atom);
        if module_name.is_err() {
            return -1;
        }
        let module_name = module_name.unwrap();

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

        let meta_obj = if let Ok(m) = m.import_meta(ctx) {
            m
        } else {
            return -1;
        };

        meta_obj
            .define_property_value_str(
                ctx,
                "url",
                ctx.create_string_from_utf8(aurl.into_bytes()),
                JS_PROP_C_W_E,
            )
            .unwrap();

        meta_obj
            .define_property_value_str(ctx, "main", is_main.into(), JS_PROP_C_W_E)
            .unwrap();

        0
    } else {
        unreachable!()
    }
}
