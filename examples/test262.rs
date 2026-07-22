// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_snake_case)]

use std::{
    collections::HashSet,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
};

use clap::Parser;
use serde::Deserialize;
use url::Url;
use zuqe::{
    CallContext, Callback, Context, EvalFlags, EvalType, ExceptionText, JSResult, JSValue,
    ModuleDef, Runtime, ScopedContext,
};

/// counter of "Test262:AsyncTestComplete" messages
static ASYNC_DONE: AtomicI32 = AtomicI32::new(0);

/// write data both to stderr and file
struct TeeWriter<A: Write, B: Write> {
    a: Option<Arc<Mutex<A>>>,
    b: Option<Arc<Mutex<B>>>,
}

impl<A: Write, B: Write> TeeWriter<A, B> {
    fn new(a: Option<A>, b: Option<B>) -> Self {
        Self {
            a: a.map(|w| Arc::new(Mutex::new(w))),
            b: b.map(|w| Arc::new(Mutex::new(w))),
        }
    }
}

impl<A: Write, B: Write> Write for TeeWriter<A, B> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n1 = if let Some(w) = &self.a {
            Some(w.lock().unwrap().write(buf)?)
        } else {
            None
        };
        let n2 = if let Some(w) = &self.b {
            Some(w.lock().unwrap().write(buf)?)
        } else {
            None
        };

        Ok(n1.or(n2).unwrap_or(0))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(w) = &self.a {
            w.lock().unwrap().flush()?;
        }
        if let Some(w) = &self.b {
            w.lock().unwrap().flush()?;
        }
        Ok(())
    }
}

impl<A: Write, B: Write> Clone for TeeWriter<A, B> {
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            b: self.b.clone(),
        }
    }
}

fn js_print(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    for i in 0..cc.length() {
        if i > 0 {
            print!(" ");
        }
        let arg = cc.arg(i);
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

fn js_detachArrayBuffer(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(mut ab) = cc.arg(0).as_object_ArrayBuffer() {
        ab.detatch(ctx);
    }
    Ok(JSValue::UNDEFINED)
}

fn js_evalScript(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let source = cc.arg(0).inspect(ctx)?;
    ctx.with_new_scope(|scope| {
        let result = scope.eval(
            source.as_bytes(),
            "<eval>",
            EvalType::Global,
            EvalFlags::empty(),
        );
        result
    })
}

fn js_string_codePointRange(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut start = cc.arg(0).to_number(ctx)?.to_u32();
    let end = std::cmp::min(cc.arg(1).to_number(ctx)?.to_u32(), 0x10ffff + 1);
    if start > end {
        start = end;
    }

    let mut n = end - start;
    if end > 0x10000 {
        n += end - std::cmp::max(start, 0x10000);
    }

    let mut buf = Vec::<u8>::with_capacity(n as usize);
    for i in start..end {
        buf.extend_from_slice(
            char::from_u32(i)
                .unwrap()
                .encode_utf8(&mut [0u8; 4])
                .as_bytes(),
        );
    }
    ctx.create_string_from_utf8(buf).map(JSValue::from)
}

fn js_createRealm(ctx: &mut ScopedContext, _: CallContext) -> JSResult {
    // SAFETY: ctx2 is used immediately and dropped before rt.
    let ctx2 = ctx
        .rt_mut()
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

    ctx2.root_scope().with_new_scope(setup_ctx_test262_helpers)
}

fn setup_ctx_test262_helpers(ctx: &mut ScopedContext) -> Result<JSValue, i32> {
    let mut global = JSValue::from(ctx.global_obj());

    {
        let s = ctx.create_native_function(Callback::Func(js_print), "print", 1, 0)?;
        global.set_property_str(ctx, "print", s.into())?;
    }

    // $262
    let mut obj262 = ctx.create_base_object()?;

    {
        let f = ctx.create_native_function(
            Callback::Func(js_detachArrayBuffer),
            "detachArrayBuffer",
            1,
            0,
        )?;
        obj262.set_property_str(ctx, "detachArrayBuffer", f.into())?;
    }

    {
        let f = ctx.create_native_function(Callback::Func(js_evalScript), "evalScript", 1, 0)?;
        obj262.set_property_str(ctx, "evalScript", f.into())?;
    }

    {
        let f: JSValue = ctx
            .create_native_function(
                Callback::Func(js_string_codePointRange),
                "codePointRange",
                1,
                0,
            )?
            .into();
        obj262.set_property_str(ctx, "codePointRange", f)?;
    }

    {
        let f = ctx.create_native_function(Callback::Func(js_createRealm), "createRealm", 1, 0)?;
        obj262.set_property_str(ctx, "createRealm", f.into())?;
    }

    let gc_func = ctx.create_native_function(
        Callback::Func(|ctx, _| -> JSResult {
            ctx.rt_mut().gc_request();
            Ok(JSValue::UNDEFINED)
        }),
        "gc",
        0,
        0,
    )?;
    obj262.set_property_str(ctx, "gc", gc_func.into())?;

    obj262.set_property_str(ctx, "global", global)?;
    global.set_property_str(ctx, "$262", obj262.into())?;

    Ok(obj262.into())
}

pub fn js_module_loader(
    ctx: &mut ScopedContext,
    module_name: &Url,
    attrs: JSValue,
    _opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    // Check for import attributes: type: "json"
    let is_json = attrs.is_object()
        && attrs
            .get_property_str(ctx, "type")
            .map(|v| {
                v.as_string()
                    .is_some_and(|s| s.content().encode_utf8(false).as_ref() == "json")
            })
            .unwrap_or(false);

    // `module_name` is a canonical `file://` URL. Dispatch by scheme here and
    // convert the URL back to a local path to read the source. The module name
    // (the URL) is passed through to `compile_module` so it becomes the
    // `module_name` cache key unchanged.
    let path = match zuqe::url_to_abspath(module_name) {
        Some(p) => p,
        None => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            return Err(-1);
        }
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => {
            if is_json {
                // JSON module: wrap as `export default <json>;\n`
                let mut src = String::from("export default ");
                src.push_str(&content);
                src.push('\n');
                ctx.compile_module(src.as_bytes(), module_name.as_str(), false)
            } else {
                ctx.compile_module(content.as_bytes(), module_name.as_str(), false)
            }
        }
        Err(_) => {
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            Err(-1)
        }
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

fn run_harness(mut ctx: Context, harness_files: &[PathBuf]) -> Result<(), ExceptionText> {
    for file in harness_files {
        let script = std::fs::read_to_string(file)
            .inspect_err(|_| {
                println!("Failed to read file: {}", file.display());
            })
            .unwrap();

        ctx.eval(
            script.as_bytes(),
            file.as_os_str().to_string_lossy().as_ref(),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .map_err(|_| ctx.take_exception().to_text(&mut ctx.root_scope()))?;
    }

    Ok(())
}

#[cfg(not(feature = "async_support"))]
fn run_test_script(
    ctx: &mut Context<'_>,
    script: &str,
    filename: &Path,
    is_module: bool,
    is_async: bool,
    is_strict: bool,
) -> Result<i32, ExceptionText> {
    if is_async {
        ASYNC_DONE.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    // Convert the entry filename to a canonical `file://` URL. `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required so
    // that (top-level) dynamic `import()` from a *script* entry resolves relative
    // specifiers correctly, instead of being handed a bare filesystem path.
    let entry_name = zuqe::path_to_url(filename.to_str().unwrap())
        .expect("entry path must convert to a file:// URL")
        .to_string();

    ctx.with_new_scope(|scope| {
        let result = scope.eval_wait(
            script.as_bytes(),
            &entry_name,
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            if is_strict {
                EvalFlags::STRICT
            } else {
                EvalFlags::empty()
            },
        );
        result
    })?;

    if is_async {
        let n = ASYNC_DONE.load(Ordering::SeqCst);
        if n != 1 {
            return Err(ExceptionText {
                message: format!("TypeError: $DONE() not called {n}"),
                traceback: None,
            });
        }
    }

    Ok(0)
}

#[cfg(feature = "async_support")]
async fn run_test_script<'c>(
    rt: &'c mut Runtime,
    mut ctx: Context<'c>,
    script: &str,
    filename: &Path,
    is_module: bool,
    is_async: bool,
    is_strict: bool,
) -> Result<i32, ExceptionText> {
    if is_async {
        ASYNC_DONE.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    // Convert the entry filename to a canonical `file://` URL. `module_name`
    // (and `import.meta.url`) is a URL, and a `file://` base is also required so
    // that (top-level) dynamic `import()` from a *script* entry resolves relative
    // specifiers correctly, instead of being handed a bare filesystem path.
    let entry_name = zuqe::path_to_url(filename.to_str().unwrap())
        .expect("entry path must convert to a file:// URL")
        .to_string();

    let val = ctx
        .eval(
            script.as_bytes(),
            &entry_name,
            if is_module {
                EvalType::Module
            } else {
                EvalType::Global
            },
            if is_strict {
                EvalFlags::STRICT
            } else {
                EvalFlags::empty()
            },
        )
        .map_err(|_| ctx.take_exception().to_text(&mut ctx.root_scope()))?;

    // Check if the returned value is a rejected Promise.
    // Only for module eval — the module function may throw synchronously
    // and the error is captured as a rejected Promise, not as an Err.
    // Non-module eval may return a Promise for other reasons (e.g. import()),
    // and a rejected Promise there should NOT fail the test.
    if is_module
        && let Ok(p) = zuqe::builtins::promise::Promise::try_from(val)
        && matches!(p.state(), zuqe::builtins::promise::PromiseState::Rejected)
    {
        return Err(ExceptionText {
            message: p
                .result()
                .to_string(&mut ctx.root_scope())
                .map_or(String::from("<can't get error message>"), |s| {
                    s.content().encode_utf8(false).to_string()
                }),
            traceback: None,
        });
    }

    let _ = rt.run_async().await;

    // After run_async(), re-check the promise state for modules with
    // top-level-await. The async evaluation (including dynamic import
    // rejections, dependency module errors, etc.) is resolved during
    // run_async(), and the module's evaluation promise should be settled.
    if is_module
        && let Ok(p) = zuqe::builtins::promise::Promise::try_from(val)
        && matches!(p.state(), zuqe::builtins::promise::PromiseState::Rejected)
    {
        return Err(ExceptionText {
            message: p
                .result()
                .to_string(&mut ctx.root_scope())
                .map_or(String::from("<can't get error message>"), |s| {
                    s.content().encode_utf8(false).to_string()
                }),
            traceback: None,
        });
    }

    if is_async {
        let n = ASYNC_DONE.load(Ordering::SeqCst);
        if n != 1 {
            return Err(ExceptionText {
                message: format!("TypeError: $DONE() not called {n}"),
                traceback: None,
            });
        }
    }

    Ok(0)
}

fn dump_error(
    exc_text: &ExceptionText,
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

    writeln!(outf, "{}", exc_text.message).unwrap();

    if let Some(tb) = exc_text.traceback.as_ref() {
        writeln!(outf, "{tb}").unwrap();
    }
}

#[cfg(not(feature = "async_support"))]
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

    println!("{}", filename.to_str().unwrap());

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
    let mut rt = Runtime::create().unwrap();

    // for panic="abort" config, use install_panic_handler to free gc allocated nodes
    #[cfg(debug_assertions)]
    rt.install_panic_handler();

    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

    // SAFETY: ctx is used within this function and dropped before rt.
    let mut ctx = unsafe {
        rt.create_context(|ctx| {
            ContextImpl::ECMA_CONTEXT_SETUP(ctx)?;
            setup_ctx_test262_helpers(ctx)?;
            Ok(())
        })
    }
    .unwrap();

    let mut has_err = false;

    if !harness_files.is_empty() {
        #[cfg(debug_assertions)]
        debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 1);

        if let Err(exc) = run_harness(ctx, &harness_files) {
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(&exc, false, false, false, outf);

            writeln!(errf, "{}", filename.display()).unwrap();
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(&exc, false, false, false, errf);

            has_err = true;
        }

        #[cfg(debug_assertions)]
        debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 1);
    }

    if !has_err {
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

        for strict in [
            if use_nostrict { Some(false) } else { None },
            if use_strict { Some(true) } else { None },
        ]
        .into_iter()
        .flatten()
        {
            #[cfg(debug_assertions)]
            debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 1);

            let res = run_test_script(
                &mut ctx,
                content.as_str(),
                filename,
                is_module,
                is_async,
                strict,
            );

            #[cfg(debug_assertions)]
            debug_assert_eq!(ctx.gc_heap().scope_max_depth(), 1);

            if let Err(exc) = res {
                if let Some(neg) = &negative {
                    if let Some(ty) = neg.type_.as_ref() {
                        let err_class = exc.message.split(":").next().unwrap();
                        if err_class == ty {
                            writeln!(outf, "pass").unwrap();
                        } else {
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&exc, strict, is_module, is_async, outf);

                            writeln!(errf, "{}", filename.display()).unwrap();
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&exc, strict, is_module, is_async, errf);
                        }
                    } else {
                        writeln!(outf, "pass").unwrap();
                    }
                } else {
                    writeln!(outf, "fail").unwrap();
                    dump_error(&exc, strict, is_module, is_async, outf);

                    writeln!(errf, "{}", filename.display()).unwrap();
                    dump_error(&exc, strict, is_module, is_async, errf);
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

#[cfg(feature = "async_support")]
async fn run_test_file(
    filename: &Path,
    harness_path: &Path,
    excludes: Option<&[String]>,
    outf: &mut dyn Write,
    errf: &mut dyn Write,
) {
    write!(outf, "{} ... ", filename.display()).unwrap();

    let content = std::fs::read_to_string(filename).unwrap();

    let start = content.find("/*---").unwrap_or(0);
    let end = content.find("---*/").unwrap_or(0);
    let config = if start < end {
        let front_matter = content[start + 5..end].trim();
        serde_yaml::from_str::<TestConfig>(front_matter).unwrap()
    } else {
        writeln!(outf, "no-test").unwrap();
        return;
    };

    if let (Some(features), Some(excl)) = (config.features.as_deref(), excludes)
        && features.iter().any(|x| excl.contains(x))
    {
        writeln!(outf, "skip").unwrap();
        return;
    }

    println!("{}", filename.to_str().unwrap());

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

    let mut rt = Runtime::create().unwrap();

    #[cfg(debug_assertions)]
    rt.install_panic_handler();

    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());

    // SAFETY: ctx is used within this function and dropped before rt.
    let mut ctx = rt
        .create_context(|ctx| {
            ScopedContext::ECMA_CONTEXT_SETUP(ctx)?;
            setup_ctx_test262_helpers(ctx)?;
            Ok(())
        })
        .unwrap();

    let mut has_err = false;

    if !harness_files.is_empty() {
        #[cfg(debug_assertions)]
        debug_assert_eq!(ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()), 1);

        if let Err(exc) = run_harness(ctx, &harness_files) {
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(&exc, false, false, false, outf);

            writeln!(errf, "{}", filename.display()).unwrap();
            writeln!(outf, "FAIL: can't eval harness").unwrap();
            dump_error(&exc, false, false, false, errf);

            has_err = true;
        }

        #[cfg(debug_assertions)]
        debug_assert_eq!(ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()), 1);
    }

    if !has_err {
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

        for strict in [
            if use_nostrict { Some(false) } else { None },
            if use_strict { Some(true) } else { None },
        ]
        .into_iter()
        .flatten()
        {
            #[cfg(debug_assertions)]
            debug_assert_eq!(ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()), 1);

            let res = run_test_script(
                &mut rt,
                ctx,
                content.as_str(),
                filename,
                is_module,
                is_async,
                strict,
            )
            .await;

            #[cfg(debug_assertions)]
            debug_assert_eq!(ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()), 1);

            if let Err(exc) = res {
                if let Some(neg) = &negative {
                    if let Some(ty) = neg.type_.as_ref() {
                        let err_class = exc.message.split(':').next().unwrap();
                        if err_class == ty {
                            writeln!(outf, "pass").unwrap();
                        } else {
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&exc, strict, is_module, is_async, outf);

                            writeln!(errf, "{}", filename.display()).unwrap();
                            writeln!(
                                outf,
                                "fail: expects error class '{}', got '{}'",
                                ty, err_class
                            )
                            .unwrap();
                            dump_error(&exc, strict, is_module, is_async, errf);
                        }
                    } else {
                        writeln!(outf, "pass").unwrap();
                    }
                } else {
                    writeln!(outf, "fail").unwrap();
                    dump_error(&exc, strict, is_module, is_async, outf);

                    writeln!(errf, "{}", filename.display()).unwrap();
                    dump_error(&exc, strict, is_module, is_async, errf);
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
    log: Option<String>,
}

#[derive(Debug, Parser)]
#[command(version)]
struct Args {
    #[arg(default_value_t = String::from("test262.yml"), help = "config file")]
    config_file: String,
}

#[cfg(not(feature = "async_support"))]
fn main_impl() {
    let args = Args::parse();

    let mut app_cfg = {
        let mut fp = std::fs::OpenOptions::new()
            .read(true)
            .open(args.config_file)
            .unwrap();
        let mut txt = String::new();

        fp.read_to_string(&mut txt).unwrap();
        serde_yaml::from_str::<AppConfig>(txt.as_str()).unwrap()
    };

    let log_tee: Option<TeeWriter<std::fs::File, std::fs::File>> = if let Some(log) = app_cfg.log {
        // create log file
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(log)
            .unwrap();

        let tee = TeeWriter::new(Some(log_file), Option::<std::fs::File>::None);

        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Trace)
            .target(env_logger::Target::Pipe(Box::new(tee.clone())))
            .init();

        Some(tee)
    } else {
        None
    };

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
            for path in
                glob::glob(base_path.join(format!("test/{item}")).to_str().unwrap()).unwrap()
            {
                let file_name = path.unwrap();

                if !exclude_tests.contains(&file_name) {
                    if let Some(f) = log_tee.as_ref().and_then(|t| t.a.clone()) {
                        // truncate log file
                        let mut fp = f.lock().unwrap();
                        fp.flush().unwrap();
                        fp.set_len(0).unwrap();
                        fp.seek(std::io::SeekFrom::Start(0)).unwrap();
                        fp.flush().unwrap();
                    }

                    run_test_file(
                        &file_name,
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

#[cfg(feature = "async_support")]
async fn main_impl_async() {
    let args = Args::parse();

    let mut app_cfg = {
        let mut fp = std::fs::OpenOptions::new()
            .read(true)
            .open(args.config_file)
            .unwrap();
        let mut txt = String::new();

        fp.read_to_string(&mut txt).unwrap();
        serde_yaml::from_str::<AppConfig>(txt.as_str()).unwrap()
    };

    let log_tee: Option<TeeWriter<std::fs::File, std::fs::File>> = if let Some(log) = app_cfg.log {
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(log)
            .unwrap();

        let tee = TeeWriter::new(Some(log_file), Option::<std::fs::File>::None);

        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Trace)
            .target(env_logger::Target::Pipe(Box::new(tee.clone())))
            .init();

        Some(tee)
    } else {
        None
    };

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
            for path in
                glob::glob(base_path.join(format!("test/{item}")).to_str().unwrap()).unwrap()
            {
                let file_name = path.unwrap();

                if !exclude_tests.contains(&file_name) {
                    if let Some(f) = log_tee.as_ref().and_then(|t| t.a.clone()) {
                        let mut fp = f.lock().unwrap();
                        fp.flush().unwrap();
                        fp.set_len(0).unwrap();
                        fp.seek(std::io::SeekFrom::Start(0)).unwrap();
                        fp.flush().unwrap();
                    }

                    run_test_file(
                        &file_name,
                        &harness_path,
                        excludes.as_deref(),
                        &mut outf,
                        &mut errf,
                    )
                    .await;
                    outf.flush().unwrap();
                }
            }
        }
    }
}

#[cfg(not(feature = "async_support"))]
fn main() {
    main_impl();
}

#[cfg(feature = "async_support")]
#[tokio::main]
async fn main() {
    main_impl_async().await;
}
