#![allow(non_snake_case)]

use std::{
    ffi::CString,
    io::Write,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use zhuque::{
    array_buffer::JS_DetachArrayBuffer,
    atom::{
        JS_FreeAtom,
        consts::{JS_ATOM_message, JS_ATOM_stack},
    },
    context::JSContext,
    error::{JS_ThrowReferenceError, JSResult},
    eval::{
        JS_EVAL_FLAG_COMPILE_ONLY, JS_EVAL_FLAG_STRICT, JS_EVAL_TYPE_GLOBAL, JS_EVAL_TYPE_MODULE,
    },
    function::{CallContext, NativeFunctionType},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JS_SetModuleLoaderFunc, JSModuleDef},
    object::JS_PROP_C_W_E,
    promise::{JSPromiseStateEnum, PromiseValue},
    runtime::JSRuntime,
    value::{JSValue, StringContent},
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

fn js_detachArrayBuffer(ctx: &JSContext, cc: CallContext) -> JSResult {
    unsafe {
        JS_DetachArrayBuffer(ctx, cc.arg(0));
    }
    Ok(JSValue::UNDEFINED)
}

fn js_evalScript(ctx: &JSContext, cc: CallContext) -> JSResult {
    let script = cc.arg(0).coerce_to_utf8(ctx)?;
    ctx.eval(
        CString::new(script.as_ref()).unwrap().as_c_str(),
        "<evalScript>",
        JS_EVAL_TYPE_GLOBAL,
    )
    .check_exception()
}

fn js_string_codePointRange(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut start = cc.arg(0).coerce_to_i32(ctx)? as u32;
    let end = std::cmp::min(cc.arg(1).coerce_to_i32(ctx)? as u32, 0x10ffff + 1);
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

fn js_createRealm(ctx: &JSContext, _: CallContext) -> JSResult {
    let ctx1 = ctx.rt_mut().create_context()?;
    setup_ctx_helpers(&ctx1)
}

fn setup_ctx_helpers(ctx: &JSContext) -> Result<JSValue, i32> {
    let global = JSValue::Object(ctx.global_obj());

    {
        let s = ctx.create_native_function(NativeFunctionType::Func(js_print), "print", 1, 0)?;
        global.set_property_str(ctx, "print", s)?;
    }

    // $262
    let obj262 = ctx.create_base_object()?;

    {
        let f = ctx.create_native_function(
            NativeFunctionType::Func(js_detachArrayBuffer),
            "detachArrayBuffer",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "detachArrayBuffer", f)?;
    }

    {
        let f = ctx.create_native_function(
            NativeFunctionType::Func(js_evalScript),
            "evalScript",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "evalScript", f)?;
    }

    {
        let f = ctx.create_native_function(
            NativeFunctionType::Func(js_string_codePointRange),
            "codePointRange",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "codePointRange", f)?;
    }

    {
        let f = ctx.create_native_function(
            NativeFunctionType::Func(js_createRealm),
            "createRealm",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "createRealm", f)?;
    }

    obj262.set_property_str(ctx, "global", global.clone())?;
    global.set_property_str(ctx, "$262", obj262.clone())?;

    Ok(obj262)
}

pub fn js_module_loader(
    ctx: &JSContext,
    module_name: &str,
    opaque: *mut std::ffi::c_void,
) -> Result<JSModuleDef, i32> {
    match std::fs::read(module_name) {
        Ok(buf) => {
            // compile the module
            let func_val = ctx
                .eval(
                    CString::new(buf).unwrap().as_c_str(),
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
        let module_name = module_name_atom.to_utf8_string(ctx);
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

        let meta_obj = if let Ok(m) = (*m).import_meta(ctx) {
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

fn run_harness(ctx: &JSContext, harness_files: &[PathBuf]) -> Result<(), JSValue> {
    for file in harness_files {
        let src = std::fs::read_to_string(&file).unwrap();

        let r = ctx.eval(
            CString::new(src).unwrap().as_c_str(),
            file.as_os_str().to_string_lossy().as_ref(),
            JS_EVAL_TYPE_GLOBAL,
        );

        if r.is_exception() {
            return Err(ctx.take_exception());
        }
    }

    Ok(())
}

fn run_test_content(
    ctx: &JSContext,
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
    let async_done = 0; // counter of "Test262:AsyncTestComplete" messages

    if ret_promise && !ret_val.is_exception() {
        // ret_val is Promise, execute pending jobs
        loop {
            match ctx.rt_mut().execute_job_once() {
                Ok(None) => {
                    if is_async {
                        // test if the test called $DONE() once
                        if async_done != 1 {
                            ctx.JS_ThrowTypeError("$DONE() not called")
                        }
                        ret_val = JSValue::UNDEFINED;
                    } else {
                        // check that the returned promise is fulfilled
                        let promise: PromiseValue =
                            TryFrom::try_from(JSValue::from(ret_val)).unwrap();

                        ret_val = match promise.state() {
                            Ok(JSPromiseStateEnum::JS_PROMISE_FULFILLED) => JSValue::UNDEFINED,
                            Ok(JSPromiseStateEnum::JS_PROMISE_REJECTED) => {
                                ctx.throw(promise.result());
                                JSValue::EXCEPTION
                            }
                            Ok(JSPromiseStateEnum::JS_PROMISE_PENDING) | Err(_) => {
                                ctx.JS_ThrowTypeError("promise not fulfilled");
                                JSValue::EXCEPTION
                            }
                        };
                    }
                    break;
                }
                Err(_) => {
                    ret_val = JSValue::EXCEPTION;
                    break;
                }
                _ => {}
            }

            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    if ret_val.is_exception() {
        Err(ctx.take_exception())
    } else {
        Ok(())
    }
}

fn dump_error(ctx: &JSContext, err_val: &JSValue, msg: &str, outf: &mut dyn Write) {
    writeln!(outf, "[MESSAGE] {msg}",).unwrap();

    let s = err_val.get_property(ctx, JS_ATOM_stack);
    match s {
        JSValue::EXCEPTION | JSValue::NULL | JSValue::UNDEFINED => {}
        _ => {
            let ss = s.coerce_to_utf8(ctx).unwrap();
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

        harness_files = vec![
            harness_path.join("assert.js"),
            harness_path.join("sta.js"),
            harness_path.join("doneprintHandle.js"),
        ];

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
    let ctx = rt.create_context().unwrap();
    let obj262 = setup_ctx_helpers(&ctx).unwrap();

    let mut has_err = false;

    if !harness_files.is_empty() {
        // eval harness includes
        if let Err(exc) = run_harness(&ctx, &harness_files) {
            let msg = exc.coerce_to_utf8(&ctx).unwrap();
            writeln!(outf, "fail: '{}', can't eval harness", filename.display()).unwrap();
            dump_error(&ctx, &exc, msg.as_ref(), outf);

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
                &ctx,
                content.as_str(),
                &filename,
                eval_flags | if strict { JS_EVAL_FLAG_STRICT } else { 0 },
                is_async,
            ) {
                println!("[O.o][msg] {:?}", exc);
                unsafe {
                    exc.object_unchecked().dump(ctx.rt());
                }

                let msg = exc.get_property(&ctx, JS_ATOM_message);
                let msg = msg.coerce_to_utf8(&ctx).unwrap();

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
                            dump_error(&ctx, &exc, msg.as_ref(), outf);
                        }
                    } else {
                        writeln!(outf, "pass: '{}'", filename.display()).unwrap();
                    }
                } else {
                    writeln!(outf, "fail: '{}'", filename.display()).unwrap();
                    dump_error(&ctx, &exc, msg.as_ref(), outf);
                }
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

    // drop(ctx);
    // drop(rt);
}

fn main() {
    unsafe {
        std::env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

    let mut fp = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("test262-result.txt")
        .unwrap();

    // run_test_file(
    //     &Path::new(
    //         //"/Users/johnray/devel/test262/test/harness/propertyhelper-verifyconfigurable-configurable-object.js"
    //         "/Users/johnray/devel/test262/test/harness/asyncHelpers-throwsAsync-custom-typeerror.js",
    //         // "/Users/johnray/devel/test262/test/harness/asyncHelpers-throwsAsync-incorrect-ctor.js",
    //     ),
    //     true,
    //     &mut fp,
    // );
    // fp.flush().unwrap();

    for fname in glob::glob("/Users/johnray/devel/test262/test/harness/*.js").unwrap() {
        let fname = fname.unwrap();
        if !fname.as_os_str().to_str().unwrap().contains("asyncHelpers") {
            run_test_file(&fname, true, &mut fp);
            fp.flush().unwrap();
        }
    }
}
