// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::RefCell,
    fs::File,
    path::{Path, PathBuf},
    sync::atomic::{AtomicI32, Ordering},
};

use url::Url;
use zuqe::{
    CallContext, Callback, Context, EvalFlags, EvalName, EvalType, ExceptionText, JSResult,
    JSValue, ModuleDef, Runtime, ScopedContext,
};

use crate::{
    TestConfig,
    presets::{Decisions, should_skip_test},
    report::{write_error_jsonl, write_jsonl_raw},
};

/// Minimal value printer: no options, no coloring; covers only the basic
/// types and Symbol. Objects/numbers fall back to JS string coercion (same as
/// `to_string`). Used by the harness `print` and eval echo.
fn val_to_string(ctx: &mut ScopedContext, v: JSValue) -> String {
    if v.is_undefined() {
        return "undefined".to_string();
    }
    if v.is_null() {
        return "null".to_string();
    }
    if let Some(b) = v.as_bool() {
        return b.to_string();
    }
    if let Some(s) = v.as_string(false) {
        return s.content().encode_utf8(false).to_string();
    }
    if let Some(sym) = v.as_symbol(false) {
        return if sym.content().is_ident() {
            "Symbol()".to_string()
        } else {
            format!("Symbol({})", sym.content().encode_utf8(false))
        };
    }
    // numbers / objects: fall back to JS string coercion
    v.to_string(ctx)
        .map(|s| s.content().encode_utf8(false).to_string())
        .unwrap_or_else(|_| "<...>".to_string())
}

/// counter of "Test262:AsyncTestComplete" messages
pub static ASYNC_DONE: AtomicI32 = AtomicI32::new(0);

/// Outcome of a single test file: it may be skipped (by preset filtering, or
/// because it carries no test front matter), pass, or fail.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TestStatus {
    Skip,
    Pass,
    Fail,
}

// Captured `print(...)` output of the test currently being run. Tests run one
// at a time on the current thread, so a thread-local buffer is enough. The
// buffer keeps `print` output off stdout (which would break the progress
// line); async completion markers are detected while capturing.
thread_local! {
    static PRINT_BUF: RefCell<String> = const { RefCell::new(String::new()) };
}

fn js_print(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut line = String::new();
    for i in 0..cc.length() {
        if i > 0 {
            line.push(' ');
        }
        let arg = cc.arg(i);
        let repr = val_to_string(ctx, arg);

        line.push_str(&repr);

        if repr.eq("Test262:AsyncTestComplete") {
            ASYNC_DONE.fetch_add(1, Ordering::SeqCst);
        } else if repr.starts_with("Test262:AsyncTestFailure") {
            ASYNC_DONE.store(2, Ordering::SeqCst);
        }
    }
    line.push('\n');

    PRINT_BUF.with(|b| b.borrow_mut().push_str(&line));

    Ok(JSValue::UNDEFINED)
}

fn js_detachArrayBuffer(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    if let Some(mut ab) = cc.arg(0).as_object_ArrayBuffer() {
        ab.detatch(ctx);
    }
    Ok(JSValue::UNDEFINED)
}

fn js_evalScript(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let source = val_to_string(ctx, cc.arg(0));
    ctx.with_new_scope(|scope| {
        scope.eval(
            source.as_bytes(),
            &EvalName::Name("<eval>".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
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
    ctx.create_string_from_utf8(buf).map(JSValue::make_string)
}

fn js_createRealm(ctx: &mut ScopedContext, _: CallContext) -> JSResult {
    // SAFETY: ctx2 is used immediately and dropped before rt.
    let ctx2 = ctx
        .rt_mut()
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;

    ctx2.root_scope().with_new_scope(setup_ctx_test262_helpers)
}

pub fn setup_ctx_test262_helpers(ctx: &mut ScopedContext) -> Result<JSValue, i32> {
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

    // $262.agent: test262 multi-agent (Atomics / SharedArrayBuffer) harness.
    let _ = crate::agent::setup_parent_agent(ctx, &mut obj262);

    Ok(obj262.into())
}

/// Extract the `type` value from import attributes, returning `None` if
/// the attrs object is absent or has no `type` property.
fn get_attr_type(ctx: &mut ScopedContext, attrs: JSValue) -> Option<String> {
    if !attrs.is_object() {
        return None;
    }
    attrs.get_property_str(ctx, "type").ok().and_then(|v| {
        v.as_string(true)
            .map(|s| s.content().encode_utf8(false).to_string())
    })
}

/// Escape a string for use as a JS string literal (double-quoted).
fn escape_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x00'..='\x1f' => out.push_str(&format!("\\x{:02x}", ch as u8)),
            c => out.push(c),
        }
    }
    out
}

pub fn js_module_loader(
    ctx: &mut ScopedContext,
    module_name: &Url,
    attrs: JSValue,
    _opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    let attr_type = get_attr_type(ctx, attrs);

    // `module_name` is a canonical `file://` URL. Dispatch by scheme here and
    // convert the URL back to a local path to read the source. The module name
    // (the URL) is passed through to `compile_module` so it becomes the
    // `module_name` cache key unchanged.
    let path = match zuqe::url_to_abspath(module_name) {
        Some(p) => p,
        None => {
            return Err(
                ctx.throw_reference_error(&format!("could not load module '{module_name}'"))
            );
        }
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match attr_type.as_deref() {
            Some("json") => {
                // JSON module: wrap as `export default <json>;\n`
                let mut src = String::from("export default ");
                src.push_str(&content);
                src.push('\n');
                ctx.compile_module(src.as_bytes(), module_name, false)
            }
            Some("text") => {
                // Text module: wrap as `export default "<escaped>";\n`
                let mut src = String::from("export default \"");
                src.push_str(&escape_js_string(&content));
                src.push_str("\";\n");
                ctx.compile_module(src.as_bytes(), module_name, false)
            }
            Some(other) => {
                Err(ctx.throw_type_error(&format!("unsupported import attribute type '{}'", other)))
            }
            None => ctx.compile_module(content.as_bytes(), module_name, false),
        },
        Err(_) => Err(ctx.throw_reference_error(&format!("could not load module '{module_name}'"))),
    }
}

fn run_harness(mut ctx: Context, harness_files: &[PathBuf]) -> Result<(), ExceptionText> {
    for file in harness_files {
        let script = std::fs::read_to_string(file)
            .inspect_err(|_| {
                eprintln!("Failed to read file: {}", file.display());
            })
            .unwrap();

        ctx.eval(
            script.as_bytes(),
            &EvalName::Name(file.as_os_str().to_string_lossy().into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .map_err(|_| ctx.take_exception().to_text(&mut ctx.root_scope()))?;
    }

    Ok(())
}

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
    let entry_url = zuqe::path_to_url(filename.to_str().unwrap())
        .expect("entry path must convert to a file:// URL");

    let val = ctx
        .eval(
            script.as_bytes(),
            &EvalName::BaseUrl(entry_url),
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
            // Surface the harness' own failure message ($DONE(err) prints
            // "Test262:AsyncTestFailure:<msg>"), falling back to a generic note.
            let msg = PRINT_BUF.with(|b| {
                b.borrow()
                    .lines()
                    .find(|l| l.contains("Test262:AsyncTestFailure"))
                    .map(|l| l.trim().to_string())
            });
            return Err(ExceptionText {
                message: msg.unwrap_or_else(|| format!("TypeError: $DONE() not called {n}")),
                traceback: None,
            });
        }
    }

    Ok(0)
}

pub async fn run_test_file(
    filename: &Path,
    harness_path: &Path,
    dec: &Decisions,
    script_rel: &str,
    errjson: &mut File,
) -> (TestStatus, Vec<String>, Option<String>) {
    PRINT_BUF.with(|b| b.borrow_mut().clear());

    // Fresh agent hub for this test so agents from a previous test (even if some
    // are still winding down) do not interfere.
    crate::agent::agents_hub().replace();

    let content = std::fs::read_to_string(filename).unwrap();

    let start = content.find("/*---").unwrap_or(0);
    let end = content.find("---*/").unwrap_or(0);
    let config = if start < end {
        let front_matter = content[start + 5..end].trim();
        serde_yaml::from_str::<TestConfig>(front_matter).unwrap()
    } else {
        return (TestStatus::Skip, Vec::new(), None);
    };

    let feats = config.features.clone().unwrap_or_default();

    if should_skip_test(config.features.as_deref(), dec) {
        return (TestStatus::Skip, feats.clone(), None);
    }

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
    let ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            setup_ctx_test262_helpers(cx)?;
            // Keep the async event loop ticking while an `Atomics.waitAsync`
            // Promise is pending (its worker thread settles it via notify or
            // timeout). Without this, `run_async` stops before the Promise
            // resolves and the test hangs.
            cx.add_tick_handler(|cx| cx.rt().has_pending_wait_async());
            Ok(())
        })
        .unwrap();

    let mut has_err = false;
    let mut failed = false;
    // Most recent error message, returned so the report can display it next to
    // the failing script name.
    let mut last_error: Option<String> = None;

    if !harness_files.is_empty() {
        #[cfg(debug_assertions)]
        debug_assert_eq!(ctx.gc_heap().scope_max_depth(ctx.scope_stack_id()), 1);

        if let Err(exc) = run_harness(ctx, &harness_files) {
            failed = true;
            write_error_jsonl(errjson, script_rel, "harness", &exc);
            last_error = Some(exc.message.clone());

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
        // `CanBlockIsFalse` declares that the host agent cannot be suspended
        // (i.e. `AgentCanSuspend()` is false), so a main-thread `Atomics.wait`
        // must throw a TypeError instead of blocking. `CanBlockIsTrue` (or no
        // flag) is the default: the host may block.
        let mut can_block = true;
        if let Some(flags) = &config.flags {
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
            if flags.iter().any(|s| s.as_str() == "CanBlockIsFalse") {
                can_block = false;
            }
        }

        let negative = config.negative;

        // Tell the engine whether the host agent may suspend while the test's
        // main-thread code runs. Restored at the end of the loop body.
        rt.set_agent_can_suspend(can_block);

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

            let scenario = if strict { "strict" } else { "default" };

            if let Err(exc) = res {
                if let Some(neg) = &negative {
                    // Negative test: only a mismatched error class is a failure;
                    // a matching class (or no expected class) is a pass.
                    if let Some(ty) = neg.type_.as_ref() {
                        let err_class = exc.message.split(':').next().unwrap();
                        if err_class != ty {
                            failed = true;
                            write_error_jsonl(errjson, script_rel, scenario, &exc);
                            last_error = Some(exc.message.clone());
                        }
                    }
                } else {
                    failed = true;
                    write_error_jsonl(errjson, script_rel, scenario, &exc);
                    last_error = Some(exc.message.clone());
                }
            } else if let Some(neg) = &negative {
                failed = true;
                write_jsonl_raw(
                    errjson,
                    script_rel,
                    scenario,
                    &format!(
                        "[NEGATIVE] expects error {}",
                        neg.type_.as_ref().map_or("", |s| s.as_str())
                    ),
                    "",
                );
                last_error = Some(format!(
                    "[NEGATIVE] expects error {}",
                    neg.type_.as_ref().map_or("", |s| s.as_str())
                ));
            }
        }
    }

    // Restore the default (the host may suspend) for the next test.
    rt.set_agent_can_suspend(true);

    // Tear down any agent threads spawned by this test so they don't leak into
    // the next test (which would corrupt broadcast/hub state across tests).
    crate::agent::teardown_agents();

    (
        if failed {
            TestStatus::Fail
        } else {
            TestStatus::Pass
        },
        feats,
        last_error,
    )
}
