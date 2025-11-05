#![allow(non_snake_case)]

use std::{
    collections::HashSet,
    io::{Read, Write},
    ops::DerefMut,
    path::{Path, PathBuf},
    sync::atomic::{AtomicI32, Ordering},
};

use serde::Deserialize;
use zhuque::{
    atom::consts::{JS_ATOM_message, JS_ATOM_stack},
    context::JSContext,
    error::JSResult,
    eval::{JS_EVAL_FLAG_STRICT, JS_EVAL_TYPE_GLOBAL},
    function::{CallContext, NativeFunctionType},
    mem::JS_DEFAULT_MALLOC_FUNCTIONS,
    module::{JS_SetModuleLoaderFunc, ModuleDef},
    object::JS_PROP_C_W_E,
    promise::{Promise, PromiseState},
    runtime::JSRuntime,
    value::JSValue,
};

/// counter of "Test262:AsyncTestComplete" messages
static ASYNC_DONE: AtomicI32 = AtomicI32::new(0);

fn js_print<'a>(ctx: &JSContext<'a>, args: CallContext<'_, 'a>) -> JSResult<'a> {
    for i in 0..args.length() {
        if i > 0 {
            print!(" ");
        }
        let arg = args.arg(i);
        let repr = arg.inspect(ctx)?;
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

fn js_detachArrayBuffer<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    if let Some(ab) = cc.arg(0).as_object_ArrayBuffer() {
        ab.detatch(ctx);
    }
    Ok(JSValue::UNDEFINED)
}

fn js_evalScript<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
    let script = cc.arg(0).inspect(ctx)?;
    ctx.eval(script.as_bytes(), "<eval>", JS_EVAL_TYPE_GLOBAL)
}

fn js_string_codePointRange<'a>(ctx: &JSContext<'a>, cc: CallContext<'_, 'a>) -> JSResult<'a> {
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

fn js_createRealm<'a>(ctx: &JSContext<'a>, _: CallContext<'_, 'a>) -> JSResult<'a> {
    let ctx1 = ctx.rt_mut().create_context_ecma()?;
    setup_ctx_test262_helpers(&ctx1)
}

fn setup_ctx_test262_helpers<'a>(ctx: &JSContext<'a>) -> Result<JSValue<'a>, i32> {
    let global = JSValue::Object(ctx.global_obj());

    {
        let s = ctx.create_native_function(NativeFunctionType::Func(js_print), "print", 1, 0)?;
        global.set_property_str(ctx, "print", s.into())?;
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
        obj262.set_property_str(ctx, "detachArrayBuffer", f.into())?;
    }

    {
        let f = ctx.create_native_function(
            NativeFunctionType::Func(js_evalScript),
            "evalScript",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "evalScript", f.into())?;
    }

    {
        let f: JSValue = ctx
            .create_native_function(
                NativeFunctionType::Func(js_string_codePointRange),
                "codePointRange",
                1,
                0,
            )?
            .into();
        obj262.set_property_str(ctx, "codePointRange", f)?;
    }

    {
        let f = ctx.create_native_function(
            NativeFunctionType::Func(js_createRealm),
            "createRealm",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "createRealm", f.into())?;
    }

    obj262.set_property_str(ctx, "global", global.clone())?;
    global.set_property_str(ctx, "$262", obj262.clone().into())?;

    Ok(obj262.into())
}

pub fn js_module_loader<'a>(
    ctx: &JSContext<'a>,
    module_name: &str,
    opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef<'a>, i32> {
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

            let md = ModuleDef::compile(ctx, buf.as_slice(), &aurl, false)?;
            Ok(md)
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module filename '{module_name}'"));
            Err(-1)
        }
    }
}

unsafe fn js_module_set_import_meta<'a>(
    ctx: &JSContext<'a>,
    func_val: &JSValue<'a>,
    use_realpath: i32,
    is_main: bool,
) -> i32 {
    if let JSValue::Module(m) = func_val {
        // make module name to absolute url
        let module_name_atom = m.module_name();
        let module_name = module_name_atom.to_utf8();

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
        let src = std::fs::read_to_string(file)
            .inspect_err(|_| {
                println!("Failed to read file: {}", file.display());
            })
            .unwrap();

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
        .map(|p| p.into())?
    } else {
        ctx.eval(
            script.as_bytes(),
            filename.as_os_str().to_str().unwrap(),
            JS_EVAL_TYPE_GLOBAL | if is_strict { JS_EVAL_FLAG_STRICT } else { 0 },
        )?
    };

    let promise = if is_async || is_module {
        Promise::try_from(res).ok()
    } else {
        None
    };

    while ctx.rt_mut().tick()? {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    if is_async {
        // test if the test called $DONE() once
        let n = ASYNC_DONE.load(Ordering::SeqCst);
        if n != 1 {
            ctx.throw_type_error(&format!("$DONE() not called {n}"));
            return Err(-1);
        }
    }

    if let Some(promise) = promise {
        // check if the returned promise is fulfilled
        match promise.state() {
            PromiseState::JS_PROMISE_FULFILLED => {}
            PromiseState::JS_PROMISE_REJECTED => {
                ctx.throw(promise.result());
                return Err(-1);
            }
            PromiseState::JS_PROMISE_PENDING => {
                ctx.throw_type_error("promise is pending");
                return Err(-1);
            }
        }
    }

    Ok(())
}

fn dump_error<'a>(
    ctx: &JSContext<'a>,
    err_val: &JSValue<'a>,
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

    match err_val.get_property(ctx, &JS_ATOM_stack) {
        Ok(JSValue::NULL) | Ok(JSValue::UNDEFINED) => {}
        Err(_) => {}
        Ok(s) => {
            let ss = s.inspect(ctx).unwrap();
            writeln!(outf, "[STACK] {}", ss).unwrap();
        }
    }
}

fn run_test_file(
    filename: &Path,
    harness_path: &Path,
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
        serde_yaml::from_str::<TestConfig>(front_matter).unwrap()
    } else {
        // no config, exit
        writeln!(outf, "no-test").unwrap();
        return;
    };

    if let (Some(features), Some(excl)) = (config.features.as_deref(), excludes)
        && features.iter().any(|x| excl.contains(x))
    {
        writeln!(outf, "skip").unwrap();
        return;
    }

    // collect included harness files
    let mut harness_files = Vec::<PathBuf>::with_capacity(16);
    harness_files.extend_from_slice(&[harness_path.join("assert.js"), harness_path.join("sta.js")]);
    if let Some(ff) = &config.flags
        && ff.iter().any(|s| s == "async")
    {
        harness_files.push(harness_path.join("doneprintHandle.js"));
    }

    if let Some(includes) = config.includes {
        for include in includes {
            harness_files.push(harness_path.join(include));
        }
    }

    // make runtime & context
    let mut rt = JSRuntime::new(JS_DEFAULT_MALLOC_FUNCTIONS, std::ptr::null_mut()).unwrap();
    JS_SetModuleLoaderFunc(rt.deref_mut(), None, js_module_loader, std::ptr::null_mut());

    let ctx = rt.create_context_ecma().unwrap();
    setup_ctx_test262_helpers(&ctx).unwrap();

    let mut has_err = false;

    if !harness_files.is_empty() {
        // eval included harness files
        if run_harness(&ctx, &harness_files).is_err() {
            let exc = ctx.exception();
            let msg = exc.to_string(&ctx).unwrap();

            // write to outf
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(
                &ctx,
                exc,
                msg.content().to_utf8(true).as_ref(),
                false,
                false,
                false,
                outf,
            );

            // write to err
            writeln!(errf, "{}", filename.display()).unwrap();
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(
                &ctx,
                exc,
                msg.content().to_utf8(true).as_ref(),
                false,
                false,
                false,
                errf,
            );

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
            if flags.iter().any(|s| s.as_str() == "module") {
                is_module = true;
            }
            if flags.iter().any(|s| s.as_str() == "onlyStrict") {
                use_strict = true;
                use_nostrict = false;
            }
            if flags.iter().any(|s| s.as_str() == "noStrict") {
                use_strict = false;
                use_nostrict = true;
            }
            is_async = flags.iter().any(|s| s.as_str() == "async");
        }

        let negative = config.negative;

        // eval in nostrict, strict mode
        for strict in [
            if use_nostrict { Some(false) } else { None },
            if use_strict { Some(true) } else { None },
        ]
        .into_iter()
        .flatten()
        {
            if run_test_script(
                &ctx,
                content.as_str(),
                filename,
                is_module,
                is_async,
                strict,
            )
            .is_err()
            {
                let exc = ctx.exception();

                println!("TEST262 EXCEPTION: {:?}", exc);
                let msg = if let JSValue::Object(o) = &exc {
                    o.dump(ctx.rt());
                    let msg = o.get_property(&ctx, &JS_ATOM_message).unwrap();
                    msg.inspect(&ctx).unwrap().to_string()
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
                            dump_error(&ctx, exc, msg.as_ref(), strict, is_module, is_async, outf);

                            // write to err file
                            writeln!(errf, "{}", filename.display()).unwrap();
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&ctx, exc, msg.as_ref(), strict, is_module, is_async, errf);
                        }
                    } else {
                        writeln!(outf, "pass").unwrap();
                    }
                } else {
                    writeln!(outf, "fail").unwrap();
                    dump_error(&ctx, exc, msg.as_ref(), strict, is_module, is_async, outf);

                    writeln!(errf, "{}", filename.display()).unwrap();
                    dump_error(&ctx, exc, msg.as_ref(), strict, is_module, is_async, errf);
                }
            } else if let Some(neg) = &negative {
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

#[derive(Debug, Deserialize, Default)]
pub struct AppConfig {
    base_path: Option<String>,
    out_path: Option<String>,
    tests: Option<Vec<String>>,
    exclude_tests: Option<Vec<String>>,
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

    let harness_path = base_path.join("harness");

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

    let mut exclude_tests = HashSet::<PathBuf>::with_capacity(128);
    if let Some(skip_tests) = app_cfg.exclude_tests.take() {
        for item in skip_tests {
            let pat = base_path.join(format!("./test/{item}"));
            for fname in glob::glob(pat.to_str().unwrap()).unwrap() {
                let fname = fname.unwrap();
                exclude_tests.insert(fname);
            }
        }
    }

    if let Some(tests) = app_cfg.tests.take() {
        let excludes = app_cfg
            .exclude_features
            .take()
            .filter(|lst| !lst.is_empty());

        for item in tests {
            let pat = base_path.join(format!("test/{item}"));
            for fname in glob::glob(pat.to_str().unwrap()).unwrap() {
                let fname = fname.unwrap();
                if !exclude_tests.contains(&fname) {
                    run_test_file(
                        &fname,
                        &harness_path,
                        excludes.as_deref(),
                        &mut outf,
                        &mut errf,
                    );
                    outf.flush().unwrap();
                }
            }
        }
    }
}
