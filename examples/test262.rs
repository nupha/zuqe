#![allow(non_snake_case)]

use std::{
    ffi::{CStr, CString},
    io::Write,
    ops::DerefMut,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use zhuque::{
    array_buffer::JS_DetachArrayBuffer,
    atom::{consts::JS_ATOM_stack, JS_FreeAtom},
    context::{JSContext, JSContextHandle, JS_FreeContext},
    error::{JSResult, JS_ThrowReferenceError},
    eval::{
        JS_EVAL_FLAG_COMPILE_ONLY, JS_EVAL_FLAG_STRICT, JS_EVAL_FLAG_STRIP, JS_EVAL_TYPE_GLOBAL,
        JS_EVAL_TYPE_MODULE,
    },
    function::{CallArgs, JSCFunctionEnum},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JSModuleDef, JS_SetModuleLoaderFunc},
    promise::{JSPromiseStateEnum, PromiseValue},
    runtime::JSRuntime,
    value::{
        object::JS_PROP_C_W_E, JSValue, JSValueConst, JS_DumpValue, JS_FreeValue, StringContent,
        JS_TAG_EXCEPTION, JS_TAG_MODULE, JS_TAG_NULL, JS_TAG_UNDEFINED, JS_VALUE_GET_PTR,
        JS_VALUE_GET_TAG,
    },
};

fn js_print(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    for i in 0..args.length() {
        if i > 0 {
            print!(" ");
        }
        let arg = args.get_raw(i);
        print!("{}", arg.coerce_to_utf8(ctx.handle())?);
    }
    println!();

    Ok(JSValue::UNDEFINED)
}

fn js_detachArrayBuffer(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    unsafe {
        JS_DetachArrayBuffer(ctx, args.get_raw(0));
    }
    Ok(JSValue::UNDEFINED)
}

fn js_evalScript(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let script = args.get_raw(0).coerce_to_utf8(ctx.handle())?;
    ctx.eval(
        CString::new(script.as_ref()).unwrap().as_c_str(),
        "<evalScript>",
        JS_EVAL_TYPE_GLOBAL,
    )
    .check_exception()
}

fn js_string_codePointRange(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    let mut start = args.get_raw(0).coerce_to_i32(ctx)? as u32;
    let end = std::cmp::min(args.get_raw(1).coerce_to_i32(ctx)? as u32, 0x10ffff + 1);
    if start > end {
        start = end;
    }

    let mut n = end - start;
    if end > 0x10000 {
        n += end - std::cmp::max(start, 0x10000);
    }

    let mut cc = StringContent::with_capacity(n as _, end >= 0x100).map_err(|_| {
        ctx.JS_ThrowOutOfMemory();
        -1
    })?;

    for i in start..end {
        cc.push_char(char::from_u32(i).unwrap()).map_err(|_| {
            ctx.JS_ThrowOutOfMemory();
            -1
        })?;
    }

    ctx.create_string_from_content(cc).check_exception()
}

fn js_createRealm(ctx: &mut JSContext, _: CallArgs) -> JSResult {
    let ctx1 = ctx.rt_mut().create_context()?;
    let ret = setup_ctx_helpers(ctx1);
    unsafe {
        // ctx1 has a refcount so it stays alive
        JS_FreeContext(ctx1.as_ptr());
    }
    ret
}

fn setup_ctx_helpers(mut ctx: JSContextHandle) -> Result<JSValue, i32> {
    let ctx = ctx.deref_mut();
    let global = ctx.global_object_dup().check_exception()?;

    {
        let s = ctx.create_native_function(js_print, "print", 1, JSCFunctionEnum::NativeFunc, 0)?;
        global.set_property_str(ctx, "print", s)?;
    }

    // $262
    let obj262 = ctx.create_object()?;

    {
        let f = ctx.create_native_function(
            js_detachArrayBuffer,
            "detachArrayBuffer",
            1,
            JSCFunctionEnum::NativeFunc,
            0,
        )?;
        obj262.set_property_str(ctx, "detachArrayBuffer", f)?;
    }

    {
        let f = ctx.create_native_function(
            js_evalScript,
            "evalScript",
            1,
            JSCFunctionEnum::NativeFunc,
            0,
        )?;
        obj262.set_property_str(ctx, "evalScript", f)?;
    }

    {
        let f = ctx.create_native_function(
            js_string_codePointRange,
            "codePointRange",
            1,
            JSCFunctionEnum::NativeFunc,
            0,
        )?;
        obj262.set_property_str(ctx, "codePointRange", f)?;
    }

    {
        let f = ctx.create_native_function(
            js_createRealm,
            "createRealm",
            1,
            JSCFunctionEnum::NativeFunc,
            0,
        )?;
        obj262.set_property_str(ctx, "createRealm", f)?;
    }

    obj262.set_property_str(ctx, "global", global.dup())?;
    global.set_property_str(ctx, "$262", obj262.dup())?;

    unsafe {
        JS_FreeValue(ctx, global);
    }
    Ok(obj262)
}

pub fn js_module_loader(
    ctx: &mut JSContext,
    module_name: &str,
    opaque: *mut std::ffi::c_void,
) -> *mut JSModuleDef {
    match std::fs::read(module_name) {
        Ok(buf) => {
            // compile the module
            let func_val = ctx.eval(
                CString::new(buf).unwrap().as_c_str(),
                module_name,
                JS_EVAL_TYPE_MODULE | JS_EVAL_FLAG_COMPILE_ONLY,
            );
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

    let m = JS_VALUE_GET_PTR(func_val).cast::<JSModuleDef>();

    // make module name to absolute url
    let module_name_atom = (*m).module_name.dup((*ctx).rt());
    let module_name = module_name_atom.to_utf8_string(ctx.into());
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

    let meta_obj = if let Ok(m) = (*m).import_meta(ctx.as_mut().unwrap()) {
        m
    } else {
        return -1;
    };

    meta_obj
        .define_property_value_str(
            ctx.as_mut().unwrap(),
            "url",
            (*ctx).create_string_from_utf8(aurl.into_bytes()),
            JS_PROP_C_W_E,
        )
        .unwrap();

    meta_obj
        .define_property_value_str(ctx.as_mut().unwrap(), "main", is_main.into(), JS_PROP_C_W_E)
        .unwrap();

    0
}

#[derive(Debug, Deserialize, Default)]
struct Config {
    description: Option<String>,
    features: Option<Vec<String>>,
    negative: Option<Negative>,
    includes: Option<Vec<String>>,
    flags: Option<Vec<String>>,
    esid: Option<String>,
    info: Option<String>,
    locale: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Negative {
    phase: Option<String>,
    type_: Option<String>,
}

fn run_harness(ctx: &mut JSContext, harness_files: &[PathBuf]) -> Result<(), JSValue> {
    for file in harness_files {
        let src = std::fs::read_to_string(&file).unwrap();

        let r = ctx.eval(
            CString::new(src).unwrap().as_c_str(),
            file.as_os_str().to_string_lossy().as_ref(),
            JS_EVAL_TYPE_GLOBAL,
        );

        if r.is_exception() {
            let rt = ctx.rt_mut();
            let exc = rt.current_exception;
            rt.current_exception = JSValue::NULL;
            return Err(exc);
        } else {
            unsafe {
                JS_FreeValue(ctx, r);
            }
        }
    }

    Ok(())
}

fn run_test_content(
    ctx: &mut JSContext,
    content: &str,
    filename: &Path,
    eval_flags: u32,
    is_async: bool,
) -> Result<(), JSValue> {
    let mut ret_val = ctx.eval(
        CString::new(content).unwrap().as_c_str(),
        filename.as_os_str().to_str().unwrap(),
        eval_flags,
    );

    let ret_promise = is_async || eval_flags & JS_EVAL_TYPE_MODULE != 0;
    let mut async_done = 0; // counter of "Test262:AsyncTestComplete" messages

    if ret_promise && !ret_val.is_exception() {
        let rt = ctx.rt_mut();
        loop {
            match rt.execute_pending_job() {
                Ok((None, false)) => {
                    if is_async {
                        // test if the test called $DONE() once
                        ret_val = if async_done != 1 {
                            ctx.JS_ThrowTypeError(c"$DONE() not called")
                        } else {
                            JSValue::UNDEFINED
                        };
                    } else {
                        // check that the returned promise is fulfilled
                        let promise = TryInto::<PromiseValue>::try_into(ret_val).unwrap();
                        ret_val = match promise.state() {
                            Ok(JSPromiseStateEnum::JS_PROMISE_FULFILLED) => JSValue::UNDEFINED,
                            Ok(JSPromiseStateEnum::JS_PROMISE_REJECTED) => {
                                let v = promise.result();
                                ctx.throw(v)
                            }
                            Ok(JSPromiseStateEnum::JS_PROMISE_PENDING) | Err(_) => {
                                ctx.JS_ThrowTypeError(c"promise not fulfilled")
                            }
                        };

                        JS_FreeValue(ctx, promise.into());
                    }
                    break;
                }
                Err(_) => {
                    ret_val = JSValue::EXCEPTION;
                    break;
                }
                _ => {}
            }

            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    if ret_val.is_exception() {
        let rt = ctx.rt_mut();
        let exc = rt.current_exception;
        rt.current_exception = JSValue::NULL;
        Err(exc)
    } else {
        JS_FreeValue(ctx, ret_val);
        Ok(())
    }
}

fn dump_error(ctx: &mut JSContext, err_val: JSValue, msg: &str, outf: &mut dyn Write) {
    writeln!(outf, "[MESSAGE] {msg}",).unwrap();

    let s = err_val.get_property(ctx, JS_ATOM_stack);
    match JS_VALUE_GET_TAG(s) {
        JS_TAG_EXCEPTION | JS_TAG_NULL | JS_TAG_UNDEFINED => {}
        _ => {
            let ss = s.coerce_to_utf8(ctx.handle()).unwrap();
            writeln!(outf, "[STACK] {}", ss).unwrap();
        }
    }
}

fn run_test_file(filename: &Path, include_harness: bool, outf: &mut dyn Write) {
    // read source
    let content = std::fs::read_to_string(filename).unwrap();

    // parse front matter config
    let start = content.find("/*---").unwrap_or(0);
    let end = content.find("---*/").unwrap_or(0);
    let config = if start < end {
        let front_matter = content[start + 5..end].trim();
        let cfg = serde_yaml::from_str::<Config>(front_matter).unwrap();
        cfg
    } else {
        Config::default()
    };

    let mut harness_files = Vec::<PathBuf>::new();

    if include_harness {
        let harness_path = filename
            .ancestors()
            .find(|p| p.ends_with("test"))
            .and_then(|p| p.parent().map(|parent| parent.to_path_buf()))
            .unwrap_or("".into())
            .join("harness");

        harness_files = vec![harness_path.join("assert.js"), harness_path.join("sta.js")];

        if let Some(includes) = config.includes {
            for include in includes {
                harness_files.push(harness_path.join(include));
            }
        }
    }

    // make runtime & context
    let mut rt = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()).unwrap();
    unsafe {
        JS_SetModuleLoaderFunc(rt.as_mut(), None, js_module_loader, std::ptr::null_mut());
    }
    let mut ctx = rt.create_context().unwrap();
    let obj262 = setup_ctx_helpers(ctx).unwrap();
    JS_FreeValue(ctx.as_ptr(), obj262);

    let mut has_err = false;

    if !harness_files.is_empty() {
        // eval harness includes
        if let Err(exc) = run_harness(ctx.deref_mut(), &harness_files) {
            let msg = exc.coerce_to_utf8(ctx).unwrap();
            writeln!(outf, "fail: '{}', can't eval harness", filename.display()).unwrap();
            dump_error(ctx.deref_mut(), exc, msg.as_ref(), outf);
            // exc.object_unchecked().dump(rt);
            JS_FreeValue(ctx.as_ptr(), exc);

            has_err = true;
        }
    }

    if !has_err {
        // flags
        let mut use_strict = false;
        let mut use_nostrict = true;
        let mut is_async = false;
        let mut is_module = false;
        if let Some(flags) = config.flags {
            if flags.iter().find(|s| s.as_str() == "module").is_some() {
                is_module = true;
            }
            if flags.iter().find(|s| s.as_str() == "onlyStrict").is_some() {
                use_strict = true;
                use_nostrict = false;
            }
            if flags.iter().find(|s| s.as_str() == "noStrict").is_some() {
                use_strict = false;
                use_nostrict = true;
            }
            is_async = flags.iter().find(|s| s.as_str() == "async").is_some();
        }

        let negative = config.negative;

        let eval_flags = if is_module {
            JS_EVAL_TYPE_MODULE
        } else {
            JS_EVAL_TYPE_GLOBAL
        };

        // eval in nostrict, strict mode
        for strict in [
            if use_nostrict { Some(false) } else { None },
            if use_strict { Some(true) } else { None },
        ]
        .into_iter()
        .filter_map(|s| s)
        {
            if let Err(exc) = run_test_content(
                ctx.deref_mut(),
                content.as_str(),
                &filename,
                eval_flags | if strict { JS_EVAL_FLAG_STRICT } else { 0 },
                is_async,
            ) {
                let msg = exc.coerce_to_utf8(ctx.handle()).unwrap();

                if let Some(neg) = &negative {
                    if let Some(ty) = neg.type_.as_ref() {
                        let err_class = msg.as_ref().split(":").next().unwrap();
                        if err_class == ty {
                            writeln!(outf, "pass: '{}'", filename.display(),).unwrap();
                        } else {
                            writeln!(
                                outf,
                                "fail: '{}', expects error class '{}', got '{}'",
                                filename.display(),
                                ty,
                                err_class
                            )
                            .unwrap();
                            dump_error(ctx.deref_mut(), exc, msg.as_ref(), outf);
                        }
                    } else {
                        writeln!(outf, "pass: '{}'", filename.display()).unwrap();
                    }
                } else {
                    writeln!(outf, "fail: '{}'", filename.display()).unwrap();
                    dump_error(ctx.deref_mut(), exc, msg.as_ref(), outf);
                }

                JS_FreeValue(ctx.as_ptr(), exc);
            } else {
                if let Some(neg) = &negative {
                    writeln!(
                        outf,
                        "fail: [NEGATIVE] '{}', expects error {}",
                        filename.display(),
                        neg.type_.as_ref().map_or("", |s| s.as_str())
                    )
                    .unwrap();
                } else {
                    writeln!(outf, "pass: {}", filename.display()).unwrap();
                }
            }
        }
    }

    // JS_DumpGCObjects(js.as_mut());
    // JS_DumpAtoms(js.as_mut());

    // println!("================ free eval result");
    // JS_DumpGCObjects(js.as_mut());
    // JS_DumpAtoms(js.as_mut());

    unsafe {
        JS_FreeContext(ctx.as_ptr());
    }
    drop(rt);
}

fn main() {
    std::env::set_var("RUST_LOG", "INFO");
    env_logger::init();

    let mut fp = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("test262-result.txt")
        .unwrap();

    // run_test_file(
    //     &Path::new("/Users/johnray/devel/test262/test/harness/compare-array-symbol.js"),
    //     true,
    //     &mut fp,
    // );
    // fp.flush().unwrap();

    for fname in glob::glob("/Users/johnray/devel/test262/test/harness/*.js").unwrap() {
        let fname = fname.unwrap();

        run_test_file(&fname, true, &mut fp);
        fp.flush().unwrap();
    }
}
