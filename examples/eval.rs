use std::{ffi::CStr, ops::DerefMut, path::PathBuf};

use zhuque::{
    atom::JS_FreeAtom,
    context::{JSContext, JS_FreeContext},
    error::JS_ThrowReferenceError,
    eval::{
        JS_Eval, JS_EvalFunction, JS_EVAL_FLAG_COMPILE_ONLY, JS_EVAL_TYPE_GLOBAL,
        JS_EVAL_TYPE_MASK, JS_EVAL_TYPE_MODULE,
    },
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JSModuleDef, JS_GetImportMeta, JS_SetModuleLoaderFunc},
    parse::JS_DetectModule,
    runtime::JSRuntime,
    value::{
        object::{JS_DefinePropertyValueStr, JS_PROP_C_W_E},
        JSValue, JSValueConst, JS_FreeValue, JS_NewBool, JS_TAG_MODULE, JS_VALUE_GET_PTR,
        JS_VALUE_GET_TAG,
    },
};

fn main() {
    std::env::set_var("RUST_LOG", "INFO");
    env_logger::init();

    let args: Vec<String> = std::env::args().collect();
    if args.len() == 1 {
        println!("argument required");
        std::process::exit(-1);
    }

    let filename = args.get(1).unwrap().clone();
    let script = std::fs::read_to_string(&filename).unwrap();
    let src = format!("{script}\0"); // add tailing '\0'

    unsafe {
        if let Ok(mut js) = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()) {
            JS_SetModuleLoaderFunc(js.as_mut(), None, js_module_loader, std::ptr::null_mut());

            let mut ctx = js.create_context().unwrap();

            let mut eval_flags = if filename.ends_with(".mjs") {
                JS_EVAL_TYPE_MODULE
            } else if JS_DetectModule(CStr::from_ptr(src.as_ptr().cast())) {
                JS_EVAL_TYPE_MODULE
            } else {
                JS_EVAL_TYPE_GLOBAL
            };

            let r = if eval_flags & JS_EVAL_TYPE_MASK == JS_EVAL_TYPE_MODULE {
                // for the modules, we compile then run to be able to set import.meta
                let mfun = JS_Eval(
                    ctx.deref_mut(),
                    src.as_ptr().cast(),
                    src.len() - 1,
                    &filename,
                    eval_flags | JS_EVAL_FLAG_COMPILE_ONLY,
                );
                if !mfun.is_exception() {
                    js_module_set_import_meta(ctx.as_raw().as_ptr(), mfun, 1, true);
                    JS_EvalFunction(ctx.as_raw().as_mut(), mfun)
                } else {
                    JSValue::EXCEPTION
                }
            } else {
                JS_Eval(
                    ctx.deref_mut(),
                    src.as_ptr().cast(),
                    src.len() - 1,
                    &filename,
                    eval_flags,
                )
            };

            loop {
                if let Ok((None, false)) = js.execute_pending_job() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }

            if r.is_exception() {
                let exc = js.current_exception;
                js.current_exception = JSValue::NULL;
                println!("!!! EXCEPTION {exc:?}");

                exc.object().dump(js.as_ref());
                JS_FreeValue(ctx.deref_mut(), exc);
            } else {
                println!(">>> RESULT: {r:?}, {:?}", r.to_utf8_string(ctx.deref_mut()));

                if let Some(pr) = r.as_promise() {
                    let v = pr.result();
                    println!("promise state: {:?}", pr.state());
                    println!("promise result: {:?}", v);
                    JS_FreeValue(ctx.as_raw().as_ptr(), v);
                }

                JS_FreeValue(ctx.deref_mut(), r);
            }

            // JS_DumpGCObjects(js.as_mut());
            // JS_DumpAtoms(js.as_mut());

            // println!("================ free eval result");
            // JS_DumpGCObjects(js.as_mut());
            // JS_DumpAtoms(js.as_mut());

            JS_FreeContext(ctx.deref_mut());
            drop(js);
        } else {
            println!("can't create runtime");
        }
    }
}

pub fn js_module_loader(
    ctx: &mut JSContext,
    module_name: &str,
    opaque: *mut std::ffi::c_void,
) -> *mut JSModuleDef {
    match std::fs::read(module_name) {
        Ok(buf) => {
            // compile the module
            let func_val = unsafe {
                JS_Eval(
                    ctx,
                    buf.as_ptr().cast::<u8>(),
                    buf.len(),
                    module_name,
                    JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY,
                )
            };
            if func_val.is_exception() {
                return std::ptr::null_mut();
            }

            // XXX: could propagate the exception
            unsafe {
                js_module_set_import_meta(ctx, func_val, true as i32, false);
            }

            // the module is already referenced, so we must free it
            let m = JS_VALUE_GET_PTR(func_val).cast::<JSModuleDef>();
            unsafe {
                JS_FreeValue(ctx, func_val);
            }

            m
        }
        Err(_) => {
            unsafe {
                JS_ThrowReferenceError(
                    ctx,
                    CStr::from_ptr(
                        format!("could not load module filename '{module_name}\0'")
                            .as_ptr()
                            .cast(),
                    ),
                );
            }
            std::ptr::null_mut()
        }
    }
}

pub unsafe fn js_module_set_import_meta(
    ctx: *mut JSContext,
    func_val: JSValueConst,
    use_realpath: i32,
    is_main: bool,
) -> i32 {
    debug_assert_eq!(JS_VALUE_GET_TAG(func_val), JS_TAG_MODULE);

    // let mut buf: [u8; PATH_MAX as usize + 16] = [0; PATH_MAX as usize + 16];

    // Rust does not have a direct equivalent of assert, so we leave it as a comment.
    let m = JS_VALUE_GET_PTR(func_val).cast::<JSModuleDef>();

    // make module name to absolute url
    let module_name_atom = (*m).module_name.dup((*ctx).rt());
    let module_name = module_name_atom.to_utf8_string(ctx.as_mut().unwrap());
    JS_FreeAtom(ctx, module_name_atom);
    if module_name.is_err() {
        return -1;
    }
    let module_name = module_name.unwrap();

    let p = PathBuf::from(module_name.as_ref());
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

    let meta_obj = JS_GetImportMeta(ctx, m);
    if meta_obj.is_exception() {
        return -1;
    }

    JS_DefinePropertyValueStr(
        ctx,
        meta_obj,
        c"url",
        (*ctx).create_string_from_utf8(aurl.into_bytes()),
        JS_PROP_C_W_E,
    );
    JS_DefinePropertyValueStr(ctx, meta_obj, c"main", JS_NewBool(is_main), JS_PROP_C_W_E);
    JS_FreeValue(ctx, meta_obj);

    0
}
