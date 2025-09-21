#![allow(non_snake_case)]

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicI32, Ordering},
};

use serde::Deserialize;
use zhuque::{
    array_buffer::JS_DetachArrayBuffer,
    atom::{
        JS_FreeAtom,
        consts::{JS_ATOM_message, JS_ATOM_stack},
    },
    context::JSContext,
    error::JSResult,
    eval::{JS_EVAL_FLAG_ASYNC, JS_EVAL_FLAG_STRICT, JS_EVAL_TYPE_GLOBAL},
    function::{CallContext, NativeFunctionType},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JS_SetModuleLoaderFunc, ModuleDef},
    object::JS_PROP_C_W_E,
    promise::{JSPromiseStateEnum, Promise},
    runtime::JSRuntime,
    value::JSValue,
};

/// counter of "Test262:AsyncTestComplete" messages
static ASYNC_DONE: AtomicI32 = AtomicI32::new(0);

fn js_print(ctx: &JSContext, args: CallContext) -> JSResult {
    for i in 0..args.length() {
        if i > 0 {
            print!(" ");
        }
        let arg = args.arg(i);
        let repr = arg.repr(ctx)?;
        print!("{}", repr);

        if repr.eq("Test262:AsyncTestComplete") {
            ASYNC_DONE.fetch_add(1, Ordering::SeqCst);
        } else if repr.starts_with("Test262:AsyncTestFailure") {
            ASYNC_DONE.store(2, Ordering::SeqCst);
        }
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
    let script = cc.arg(0).repr(ctx)?;
    ctx.eval(script.as_bytes(), "<eval>", JS_EVAL_TYPE_GLOBAL)
}

fn js_string_codePointRange(ctx: &JSContext, cc: CallContext) -> JSResult {
    let mut start = cc.arg(0).to_i32(ctx)? as u32;
    let end = std::cmp::min(cc.arg(1).to_i32(ctx)? as u32, 0x10ffff + 1);
    if start > end {
        start = end;
    }

    let mut n = end - start;
    if end > 0x10000 {
        n += end - std::cmp::max(start, 0x10000);
    }

    let mut cc = Vec::<u8>::with_capacity(n as usize);

    for i in start..end {
        cc.extend_from_slice(
            char::from_u32(i)
                .unwrap()
                .encode_utf8(&mut [0u8; 4])
                .as_bytes(),
        );
    }
    ctx.create_string_from_utf8(cc).map(JSValue::String)
}

fn js_createRealm(ctx: &JSContext, _: CallContext) -> JSResult {
    let ctx1 = ctx.rt_mut().create_context()?;
    setup_ctx_test262_helpers(&ctx1)
}

fn setup_ctx_test262_helpers(ctx: &JSContext) -> Result<JSValue, i32> {
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
    global.set_property_str(ctx, "$262", obj262.clone().into())?;

    Ok(obj262.into())
}

pub fn js_module_loader(
    ctx: &JSContext,
    module_name: &str,
    opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    match std::fs::read(module_name) {
        Ok(buf) => {
            // make abs url
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
            let aurl = format!("{}", normalized.display());

            let md = ctx.eval_module(buf.as_slice(), &aurl, false)?;
            // // XXX: could propagate the exception
            // unsafe {
            //     js_module_set_import_meta(ctx, &func_val, true as i32, false);
            // }
            Ok(md)
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module filename '{module_name}'"));
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
        let module_name_atom = m.module_name_atom().dup(ctx.rt());
        let module_name = module_name_atom.to_utf8(ctx.rt());
        JS_FreeAtom(ctx, module_name_atom);

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
    } else {
        unreachable!()
    }
}

#[derive(Debug, Deserialize, Default)]
struct TestConfig {
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

fn run_harness(ctx: &JSContext, harness_files: &[PathBuf]) -> Result<(), i32> {
    for file in harness_files {
        let src = std::fs::read_to_string(&file).unwrap();
        ctx.eval(
            src.as_bytes(),
            file.as_os_str().to_string_lossy().as_ref(),
            JS_EVAL_TYPE_GLOBAL,
        )?;
    }

    Ok(())
}

fn run_test_script(
    ctx: &JSContext,
    script: &str,
    filename: &Path,
    is_module: bool,
    is_async: bool,
    is_strict: bool,
) -> Result<(), i32> {
    if is_async {
        ASYNC_DONE.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    let res = if is_module {
        ctx.eval_module(
            script.as_bytes(),
            filename.as_os_str().to_str().unwrap(),
            false,
        )
        .map(JSValue::Module)?
    } else {
        ctx.eval(
            script.as_bytes(),
            filename.as_os_str().to_str().unwrap(),
            JS_EVAL_TYPE_GLOBAL
                | if is_strict { JS_EVAL_FLAG_STRICT } else { 0 }
                | if is_async { JS_EVAL_FLAG_ASYNC } else { 0 },
        )?
    };

    while ctx.rt_mut().tick()? {
        std::thread::sleep(std::time::Duration::from_millis(3));
    }

    if is_async {
        // test if the test called $DONE() once
        let n = ASYNC_DONE.load(Ordering::SeqCst);
        if n != 1 {
            ctx.throw_type_error(&format!("$DONE() not called {n}"));
            return Err(-1);
        }
    }

    if is_async || is_module {
        let promise = if is_module {
            let JSValue::Module(md) = res else {
                unreachable!("expect module value");
            };
            md.promise()
        } else {
            Promise::try_from(res).ok()
        };
        if let Some(promise) = promise {
            // check if the returned promise is fulfilled
            match promise.state() {
                JSPromiseStateEnum::JS_PROMISE_FULFILLED => {}
                JSPromiseStateEnum::JS_PROMISE_REJECTED => {
                    ctx.throw(promise.result());
                    return Err(-1);
                }
                JSPromiseStateEnum::JS_PROMISE_PENDING => {
                    ctx.throw_type_error("promise is pending");
                    return Err(-1);
                }
            }
        }
    }

    Ok(())
}

fn dump_error(
    ctx: &JSContext,
    err_val: &JSValue,
    msg: &str,
    strict: bool,
    is_module: bool,
    is_async: bool,
    outf: &mut dyn Write,
) {
    writeln!(
        outf,
        "[ENV] strict={strict}, module={is_module}, async={is_async}"
    )
    .unwrap();
    writeln!(outf, "[MESSAGE] {msg}").unwrap();

    match err_val.get_property(ctx, JS_ATOM_stack) {
        Ok(JSValue::NULL) | Ok(JSValue::UNDEFINED) => {}
        Err(_) => {}
        Ok(s) => {
            let ss = s.repr(ctx).unwrap();
            writeln!(outf, "[STACK] {}", ss).unwrap();
        }
    }
}

fn run_test_file(
    filename: &Path,
    include_harness: bool,
    excludes: Option<&[String]>,
    outf: &mut dyn Write,
    errf: &mut dyn Write,
) {
    write!(outf, "{} ... ", filename.display()).unwrap();

    // read source
    let content = std::fs::read_to_string(filename).unwrap();

    // parse front matter config
    let start = content.find("/*---").unwrap_or(0);
    let end = content.find("---*/").unwrap_or(0);
    let config = if start < end {
        let front_matter = content[start + 5..end].trim();
        let cfg = serde_yaml::from_str::<TestConfig>(front_matter).unwrap();
        cfg
    } else {
        // no config, exit
        return;
    };

    match (config.features.as_deref(), excludes) {
        (Some(features), Some(excl)) => {
            if features.iter().any(|x| excl.contains(x)) {
                writeln!(outf, "skip").unwrap();
                return;
            }
        }
        _ => {}
    }

    let mut harness_files = Vec::<PathBuf>::new();

    if include_harness {
        let harness_path = filename
            .ancestors()
            .find(|p| p.ends_with("test"))
            .and_then(|p| p.parent().map(|parent| parent.to_path_buf()))
            .unwrap_or("".into())
            .join("harness");

        harness_files = vec![harness_path.join("assert.js"), harness_path.join("sta.js")];

        if let Some(lst) = &config.flags {
            if lst.iter().any(|s| s == "async") {
                harness_files.push(harness_path.join("doneprintHandle.js"));
            }
        }

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
    setup_ctx_test262_helpers(&ctx).unwrap();

    let mut has_err = false;

    if !harness_files.is_empty() {
        // eval harness includes
        if run_harness(&ctx, &harness_files).is_err() {
            let exc = ctx.exception();
            let msg = exc.repr(&ctx).unwrap();

            // write to outf
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(&ctx, &exc, msg.as_ref(), false, false, false, outf);

            // write to err
            writeln!(errf, "{}", filename.display()).unwrap();
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(&ctx, &exc, msg.as_ref(), false, false, false, errf);

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

        // eval in nostrict, strict mode
        for strict in [
            if use_nostrict { Some(false) } else { None },
            if use_strict { Some(true) } else { None },
        ]
        .into_iter()
        .filter_map(|s| s)
        {
            if run_test_script(
                &ctx,
                content.as_str(),
                &filename,
                is_module,
                is_async,
                strict,
            )
            .is_err()
            {
                let exc = ctx.exception();

                println!("EXCEPTION: {:?}", exc);
                let msg = if let JSValue::Object(o) = &exc {
                    o.dump(ctx.rt());
                    let msg = o.get_property(&ctx, JS_ATOM_message).unwrap();
                    msg.repr(&ctx).unwrap().to_string()
                } else {
                    "<no msg>".into()
                };

                if let Some(neg) = &negative {
                    if let Some(ty) = neg.type_.as_ref() {
                        let err_class = msg.split(":").next().unwrap();
                        if err_class == ty {
                            writeln!(outf, "pass").unwrap();
                        } else {
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&ctx, &exc, msg.as_ref(), strict, is_module, is_async, outf);

                            // write to err file
                            writeln!(errf, "{}", filename.display()).unwrap();
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&ctx, &exc, msg.as_ref(), strict, is_module, is_async, errf);
                        }
                    } else {
                        writeln!(outf, "pass").unwrap();
                    }
                } else {
                    writeln!(outf, "fail").unwrap();
                    dump_error(&ctx, &exc, msg.as_ref(), strict, is_module, is_async, outf);

                    writeln!(errf, "{}", filename.display()).unwrap();
                    dump_error(&ctx, &exc, msg.as_ref(), strict, is_module, is_async, errf);
                }
            } else {
                if let Some(neg) = &negative {
                    writeln!(
                        outf,
                        "fail: [NEGATIVE] expects error {}",
                        neg.type_.as_ref().map_or("", |s| s.as_str())
                    )
                    .unwrap();

                    writeln!(errf, "{}", filename.display()).unwrap();
                    writeln!(
                        errf,
                        "fail: [NEGATIVE] expects error {}",
                        neg.type_.as_ref().map_or("", |s| s.as_str())
                    )
                    .unwrap();
                } else {
                    writeln!(outf, "pass").unwrap();
                }
            }
        }
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct AppConfig {
    base_path: Option<String>,
    out_path: Option<String>,
    tests: Option<Vec<String>>,
    exclude_features: Option<Vec<String>>,
}

fn main() {
    unsafe {
        std::env::set_var("RUST_LOG", "INFO");
    }
    env_logger::init();

    let mut app_cfg = {
        let mut fp = std::fs::OpenOptions::new()
            .read(true)
            .open("test262.yml")
            .unwrap();
        let mut txt = String::new();

        fp.read_to_string(&mut txt).unwrap();
        serde_yaml::from_str::<AppConfig>(txt.as_str()).unwrap()
    };
    println!("app config: {:?}", app_cfg);

    let mut base_path: PathBuf = if let Some(txt) = app_cfg.base_path.take() {
        let p = PathBuf::from(txt);
        match std::fs::metadata(&p) {
            Ok(m) => {
                if !m.is_dir() {
                    log::error!("invalid directory: {}", p.display());
                    std::process::exit(1);
                }
                p
            }
            Err(_) => {
                log::error!("invalid directory '{}'", p.display());
                std::process::exit(1);
            }
        }
    } else {
        std::env::current_dir().unwrap()
    };
    if !base_path
        .to_str()
        .unwrap()
        .ends_with(std::path::MAIN_SEPARATOR)
    {
        let mut s = base_path.to_string_lossy().into_owned();
        s.push(std::path::MAIN_SEPARATOR);
        base_path = s.into()
    }

    let mut out_path: PathBuf = if let Some(txt) = app_cfg.out_path.take() {
        let p = PathBuf::from(txt);
        match std::fs::metadata(&p) {
            Ok(m) => {
                if !m.is_dir() {
                    log::error!("invalid directory: {}", p.display());
                    std::process::exit(1);
                }
                p
            }
            Err(_) => {
                log::error!("invalid directory '{}'", p.display());
                std::process::exit(1);
            }
        }
    } else {
        std::env::current_dir().unwrap()
    };
    if !out_path
        .to_str()
        .unwrap()
        .ends_with(std::path::MAIN_SEPARATOR)
    {
        let mut s = out_path.to_string_lossy().into_owned();
        s.push(std::path::MAIN_SEPARATOR);
        out_path = s.into()
    }

    let mut outf = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("test262-result.txt")
        .unwrap();

    let mut errf = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open("test262-fail.txt")
        .unwrap();

    if let Some(tests) = app_cfg.tests.take() {
        let excludes = if let Some(lst) = app_cfg.exclude_features.take() {
            if lst.is_empty() { None } else { Some(lst) }
        } else {
            None
        };

        for item in tests {
            let pat = base_path.join(format!("./test/{item}"));
            for fname in glob::glob(pat.to_str().unwrap()).unwrap() {
                let fname = fname.unwrap();
                run_test_file(&fname, true, excludes.as_deref(), &mut outf, &mut errf);
                outf.flush().unwrap();
            }
        }
    }
}
