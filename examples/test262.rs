// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_snake_case)]

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
};

use clap::Parser;
use serde::Deserialize;
use serde_json;
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
        scope.eval(
            source.as_bytes(),
            "<eval>",
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
            ctx.throw_reference_error(&format!("could not load module '{module_name}'"));
            return Err(-1);
        }
    };

    match std::fs::read_to_string(&path) {
        Ok(content) => match attr_type.as_deref() {
            Some("json") => {
                // JSON module: wrap as `export default <json>;\n`
                let mut src = String::from("export default ");
                src.push_str(&content);
                src.push('\n');
                ctx.compile_module(src.as_bytes(), module_name.as_str(), false)
            }
            Some("text") => {
                // Text module: wrap as `export default "<escaped>";\n`
                let mut src = String::from("export default \"");
                src.push_str(&escape_js_string(&content));
                src.push_str("\";\n");
                ctx.compile_module(src.as_bytes(), module_name.as_str(), false)
            }
            Some(other) => {
                ctx.throw_type_error(&format!("unsupported import attribute type '{}'", other));
                Err(-1)
            }
            None => ctx.compile_module(content.as_bytes(), module_name.as_str(), false),
        },
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

/// Write one JSONL error record with an explicit message and stack string.
fn write_jsonl_raw(w: &mut File, script_rel: &str, scenario: &str, message: &str, stack: &str) {
    let entry = serde_json::json!({
        "script": script_rel,
        "error": message,
        "stack": stack,
        "scenario": scenario,
    });
    let _ = writeln!(w, "{}", entry);
}

/// Write one JSONL error record from an `ExceptionText` (message + raw stack).
fn write_error_jsonl(w: &mut File, script_rel: &str, scenario: &str, exc: &ExceptionText) {
    write_jsonl_raw(
        w,
        script_rel,
        scenario,
        &exc.message,
        exc.traceback.as_deref().unwrap_or(""),
    );
}

/// Given an error-file path `foo.jsonl`, return the backup path `foo-last.jsonl`.
fn make_last_jsonl_path(path: &str) -> PathBuf {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = p.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}-last.jsonl"))
}

async fn run_test_file(
    filename: &Path,
    harness_path: &Path,
    dec: &Decisions,
    outf: &mut dyn Write,
    errf: &mut dyn Write,
    script_rel: &str,
    errjson: &mut File,
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

    if should_skip_test(config.features.as_deref(), dec) {
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
    let ctx = rt
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
            write_error_jsonl(errjson, script_rel, "harness", &exc);

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

            let scenario = if strict { "strict" } else { "default" };

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
                            write_error_jsonl(errjson, script_rel, scenario, &exc);
                        }
                    } else {
                        writeln!(outf, "pass").unwrap();
                    }
                } else {
                    writeln!(outf, "fail").unwrap();
                    dump_error(&exc, strict, is_module, is_async, outf);

                    writeln!(errf, "{}", filename.display()).unwrap();
                    dump_error(&exc, strict, is_module, is_async, errf);
                    write_error_jsonl(errjson, script_rel, scenario, &exc);
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
            } else {
                writeln!(outf, "pass").unwrap();
            }
        }
    }
}

/// Built-in edition presets.
///
/// Each entry lists the `features.txt` flags that were *newly* introduced in
/// that edition. The set of features "covered" by an edition is the union of
/// every entry up to and including it. A test is skipped when it requires a
/// feature introduced in a strictly newer edition than the selected preset.
///
/// Feature flags not listed here (stage-3 proposals, Intl features, …) are
/// treated as belonging to the *latest* edition, so they only run when the
/// latest preset is selected and never run under older presets.
const BUILTIN_PRESETS: &[(&str, &[&str])] = &[
    (
        "ES5",
        &[
            "IsHTMLDDA",
            "host-gc-required",
            "cross-realm",
            "__proto__",
            "__getter__",
            "__setter__",
        ],
    ),
    (
        "ES2015",
        &[
            "let",
            "const",
            "arrow-function",
            "class",
            "generators",
            "template",
            "for-of",
            "super",
            "new.target",
            "default-parameters",
            "rest-parameters",
            "destructuring-binding",
            "destructuring-assignment",
            "computed-property-names",
            "Symbol",
            "Symbol.iterator",
            "Symbol.hasInstance",
            "Symbol.isConcatSpreadable",
            "Symbol.match",
            "Symbol.replace",
            "Symbol.search",
            "Symbol.species",
            "Symbol.split",
            "Symbol.toPrimitive",
            "Symbol.toStringTag",
            "Symbol.unscopables",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Proxy",
            "Reflect",
            "Reflect.construct",
            "Reflect.set",
            "Reflect.setPrototypeOf",
            "Promise",
            "ArrayBuffer",
            "Int8Array",
            "Uint8Array",
            "Uint8ClampedArray",
            "Int16Array",
            "Uint16Array",
            "Int32Array",
            "Uint32Array",
            "Float32Array",
            "Float64Array",
            "TypedArray",
            "DataView",
            "DataView.prototype.getFloat32",
            "DataView.prototype.getFloat64",
            "DataView.prototype.getInt16",
            "DataView.prototype.getInt32",
            "DataView.prototype.getInt8",
            "DataView.prototype.getUint16",
            "DataView.prototype.getUint32",
            "DataView.prototype.setUint8",
            "Array.prototype.values",
            "Object.is",
            "String.fromCodePoint",
            "String.prototype.includes",
            "String.prototype.endsWith",
            "tail-call-optimization",
        ],
    ),
    ("ES2016", &["Array.prototype.includes", "exponentiation"]),
    (
        "ES2017",
        &["async-functions", "SharedArrayBuffer", "Atomics"],
    ),
    (
        "ES2018",
        &[
            "async-iteration",
            "object-rest",
            "object-spread",
            "Promise.prototype.finally",
            "regexp-dotall",
            "regexp-lookbehind",
            "regexp-named-groups",
            "regexp-unicode-property-escapes",
            "Symbol.asyncIterator",
        ],
    ),
    (
        "ES2019",
        &[
            "Array.prototype.flat",
            "Array.prototype.flatMap",
            "Object.fromEntries",
            "json-superset",
            "optional-catch-binding",
            "string-trimming",
            "String.prototype.trimEnd",
            "String.prototype.trimStart",
            "Symbol.prototype.description",
            "stable-array-sort",
            "stable-typedarray-sort",
            "well-formed-json-stringify",
        ],
    ),
    (
        "ES2020",
        &[
            "BigInt",
            "coalesce-expression",
            "dynamic-import",
            "globalThis",
            "export-star-as-namespace-from-module",
            "import.meta",
            "optional-chaining",
            "Promise.allSettled",
            "String.prototype.matchAll",
            "Symbol.matchAll",
        ],
    ),
    (
        "ES2021",
        &[
            "AggregateError",
            "FinalizationRegistry",
            "logical-assignment-operators",
            "numeric-separator-literal",
            "Promise.any",
            "String.prototype.replaceAll",
            "WeakRef",
        ],
    ),
    (
        "ES2022",
        &[
            "arbitrary-module-namespace-names",
            "Array.prototype.at",
            "class-fields-private",
            "class-fields-private-in",
            "class-fields-public",
            "class-methods-private",
            "class-static-block",
            "class-static-fields-private",
            "class-static-fields-public",
            "class-static-methods-private",
            "error-cause",
            "Object.hasOwn",
            "regexp-match-indices",
            "String.prototype.at",
            "top-level-await",
            "TypedArray.prototype.at",
        ],
    ),
    (
        "ES2023",
        &[
            "array-find-from-last",
            "Atomics.waitAsync",
            "change-array-by-copy",
            "hashbang",
            "String.prototype.isWellFormed",
            "String.prototype.toWellFormed",
            "symbols-as-weakmap-keys",
        ],
    ),
    (
        "ES2024",
        &[
            "Array.fromAsync",
            "array-grouping",
            "arraybuffer-transfer",
            "Float16Array",
            "promise-with-resolvers",
            "regexp-v-flag",
            "resizable-arraybuffer",
            "set-methods",
            "uint8array-base64",
        ],
    ),
    (
        "ES2025",
        &[
            "iterator-helpers",
            "json-modules",
            "json-parse-with-source",
            "promise-try",
            "RegExp.escape",
            "regexp-modifiers",
        ],
    ),
    (
        "ES2026",
        &[
            "Error.isError",
            "iterator-sequencing",
            "joint-iteration",
            "Math.sumPrecise",
        ],
    ),
];

/// Features excluded by default: stage-3 proposals and separate specs that are
/// not part of any published ECMAScript edition. They are always skipped
/// unless the user removes them from `exclude_features`.
const DEFAULT_EXCLUDE_FEATURES: &[&str] = &[
    "Intl.Era-monthcode",
    "Intl.Locale-info",
    "Intl.NumberFormat-v3",
    "legacy-regexp",
    "ShadowRealm",
    "decorators",
    "source-phase-imports",
    "source-phase-imports-module-source",
    "import-defer",
    "export-defer",
    "import-text",
    "immutable-arraybuffer",
    "import-bytes",
    "nonextensible-applies-to-private",
    "await-dictionary",
    "error-stack-accessor",
    "Temporal",
];

/// Resolved feature-selection decisions for a single run.
struct Decisions {
    /// Maps a feature flag to the index of the edition that introduced it.
    /// Features absent from this map are treated as the latest edition.
    feature_edition: HashMap<String, usize>,
    /// Index of the selected preset in `BUILTIN_PRESETS`.
    selected_index: usize,
    /// Index of the latest preset.
    max_index: usize,
    /// Features that are always skipped.
    exclude: HashSet<String>,
}

/// Build the `feature -> edition index` map from `BUILTIN_PRESETS`.
fn build_feature_edition() -> (HashMap<String, usize>, usize) {
    let mut map = HashMap::new();
    for (idx, (_, feats)) in BUILTIN_PRESETS.iter().enumerate() {
        for f in *feats {
            map.insert((*f).to_string(), idx);
        }
    }
    (map, BUILTIN_PRESETS.len() - 1)
}

/// Comma-separated list of all selectable presets (for error messages).
fn available_presets() -> String {
    BUILTIN_PRESETS
        .iter()
        .map(|(n, _)| *n)
        .chain(std::iter::once("ALL"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `name` names a valid preset (`ALL` or a built-in edition).
fn is_known_preset(name: &str) -> bool {
    name == "ALL" || BUILTIN_PRESETS.iter().any(|(n, _)| *n == name)
}

/// Merge CLI flags onto the config loaded from the file. A CLI flag takes
/// precedence over the matching file setting; file settings stay untouched
/// when the corresponding flag is absent.
fn apply_cli_overrides(cfg: &mut AppConfig, args: &Args) {
    if let Some(preset) = args.preset.clone() {
        cfg.preset = Some(preset);
    }
    if !args.test_files.is_empty() {
        cfg.tests = Some(args.test_files.clone());
    }
    if !args.exclude_features.is_empty() {
        match cfg.exclude_features.as_mut() {
            Some(list) => list.extend(args.exclude_features.iter().cloned()),
            None => cfg.exclude_features = Some(args.exclude_features.clone()),
        }
    }
}

/// Validate the (merged) config in one place, right before running. Any
/// missing or invalid setting surfaces here instead of mid-run.
/// `require_preset` is false in `--error-file` mode, where the preset no
/// longer selects which scripts run.
fn validate_config(cfg: &AppConfig, require_preset: bool) {
    // `preset` is required: either from the file or from `--preset`. Skipped
    // when re-running from an errors list, since preset-based filtering is off.
    if let Some(preset) = cfg.preset.as_deref() {
        if !is_known_preset(preset) {
            eprintln!(
                "error: unknown preset '{preset}'. available presets: {}",
                available_presets()
            );
            std::process::exit(1);
        }
    } else if require_preset {
        eprintln!(
            "error: no preset specified. Set 'preset:' in the config file or pass --preset <NAME>.\n\
             available presets: {}",
            available_presets()
        );
        std::process::exit(1);
    }

    // `base_path` / `out_path`, if set, must be existing directories.
    for (key, p) in [
        ("base_path", cfg.base_path.as_deref()),
        ("out_path", cfg.out_path.as_deref()),
    ] {
        if let Some(p) = p
            && !std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
        {
            eprintln!("error: invalid directory for '{key}': {p}");
            std::process::exit(1);
        }
    }
}

/// Resolve the preset, edition index, and exclude set for a run. Assumes the
/// config has already been merged with CLI flags and validated.
fn resolve_decisions(cfg: &AppConfig) -> Decisions {
    let (feature_edition, max_index) = build_feature_edition();

    let preset_name = cfg.preset.as_deref().expect("preset validated");

    let selected_index = if preset_name == "ALL" {
        // Sit past every edition so the edition gate never fires: a test is
        // skipped only by `exclude` (this is the "ALL" preset).
        max_index + 1
    } else {
        BUILTIN_PRESETS
            .iter()
            .position(|(name, _)| *name == preset_name)
            .expect("preset validated")
    };

    let mut exclude = HashSet::new();
    for f in DEFAULT_EXCLUDE_FEATURES {
        exclude.insert((*f).to_string());
    }
    if let Some(list) = &cfg.exclude_features {
        for f in list {
            exclude.insert(f.clone());
        }
    }

    Decisions {
        feature_edition,
        selected_index,
        max_index,
        exclude,
    }
}

/// Decide whether a single test should be skipped.
///
/// A test is skipped when:
/// * it requires a feature listed in the exclude set, or
/// * it requires a feature introduced in a strictly newer edition than the
///   selected preset (for the "ALL" preset `selected_index` is raised past every
///   edition, so this gate never fires and only `exclude` applies).
///
/// Tests without any `features` annotation (baseline tests) are always run.
fn should_skip_test(features: Option<&[String]>, dec: &Decisions) -> bool {
    let Some(features) = features else {
        return false;
    };
    for f in features {
        if dec.exclude.contains(f) {
            return true;
        }
        let edition = dec.feature_edition.get(f).copied().unwrap_or(dec.max_index);
        if edition > dec.selected_index {
            return true;
        }
    }
    false
}

#[derive(Debug, Deserialize, Default)]
pub struct AppConfig {
    base_path: Option<String>,
    out_path: Option<String>,
    tests: Option<Vec<String>>,
    exclude_tests: Option<Vec<String>>,
    exclude_features: Option<Vec<String>>,
    /// Optional edition preset (overridden by the `--preset` CLI flag).
    preset: Option<String>,
    log: Option<String>,
}

#[derive(Debug, Parser)]
#[command(version, about = "Run test262 with edition presets")]
struct Args {
    /// config file (default: test262.yml in the current directory). All example
    /// flags must follow `cargo run --example test262 --` so cargo does not
    /// intercept them.
    #[arg(long = "config", default_value_t = String::from("test262.yml"))]
    config_file: String,

    /// Edition preset to run. One of the built-in presets: ES5, ES2015, ES2016,
    /// ES2017, ES2018, ES2019, ES2020, ES2021, ES2022, ES2023, ES2024, ES2025,
    /// ES2026, or `ALL` (run every feature, limited only by `exclude_features`).
    /// Required: if `--preset` is omitted, the `preset:` setting from the config
    /// file is used; if neither is set, the runner errors out.
    #[arg(long)]
    preset: Option<String>,

    /// Exclude an additional feature (repeatable), e.g. --exclude-feature Temporal
    #[arg(long = "exclude-feature")]
    exclude_features: Vec<String>,

    /// Specific test file(s) or glob(s) to run; overrides `tests` in the config.
    #[arg(trailing_var_arg = true)]
    test_files: Vec<String>,

    /// Path to a JSONL error log produced by a previous run. When given, only
    /// the scripts listed in that file are re-run (the config's `tests`/`preset`
    /// script filtering is ignored), and the file is rewritten in place with the
    /// current failures (the previous file is backed up to `<name>-last.jsonl`).
    #[arg(long = "error-file")]
    error_file: Option<String>,
}

async fn main_impl() {
    let args = Args::parse();

    // Config is given via `--config` (default: test262.yml). Positional
    // arguments are test scripts/globs to run (matching mainstream test262
    // runners such as test262-harness, QuickJS, and Boa).
    let config_file = &args.config_file;

    let mut app_cfg = {
        let mut fp = std::fs::OpenOptions::new()
            .read(true)
            .open(config_file)
            .unwrap();
        let mut txt = String::new();

        fp.read_to_string(&mut txt).unwrap();
        serde_yaml::from_str::<AppConfig>(txt.as_str()).unwrap()
    };

    // If `--error-file` is given, read the previous error log and back it up.
    // Only the listed scripts are re-run afterwards; the config's preset/tests
    // filtering is ignored in that mode.
    let (errjson_path, forced): (PathBuf, Option<Vec<String>>) =
        if let Some(path) = args.error_file.clone() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let mut seen = HashSet::new();
            let mut scripts = Vec::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(s) = v.get("script").and_then(|x| x.as_str())
                    && seen.insert(s.to_string())
                {
                    scripts.push(s.to_string());
                }
            }

            let backup = make_last_jsonl_path(&path);
            let _ = std::fs::copy(&path, &backup);
            log::info!(
                "error-file: re-running {} script(s); backup -> {}",
                scripts.len(),
                backup.display()
            );
            (PathBuf::from(path), Some(scripts))
        } else {
            (PathBuf::from("test262-errors.jsonl"), None)
        };

    // Merge CLI flags over the file config, then validate everything up front.
    apply_cli_overrides(&mut app_cfg, &args);
    validate_config(&app_cfg, forced.is_none());

    let dec = if forced.is_some() {
        // Re-run mode: run exactly the listed scripts, no preset/feature filtering.
        let (_, max_index) = build_feature_edition();
        Decisions {
            feature_edition: HashMap::new(),
            selected_index: max_index + 1,
            max_index,
            exclude: HashSet::new(),
        }
    } else {
        resolve_decisions(&app_cfg)
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

    // `base_path` was already validated as an existing directory in
    // `validate_config`; here we just resolve it (or fall back to CWD).
    let mut base_path: PathBuf = if let Some(txt) = app_cfg.base_path.take() {
        PathBuf::from(txt)
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

    // `out_path` was already validated as an existing directory in
    // `validate_config`; here we just resolve it (or fall back to CWD).
    let mut out_path: PathBuf = if let Some(txt) = app_cfg.out_path.take() {
        PathBuf::from(txt)
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

    let mut errjson = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&errjson_path)
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

    let test_globs: Vec<String> = if !args.test_files.is_empty() {
        args.test_files.clone()
    } else if let Some(tests) = app_cfg.tests.take() {
        if tests.is_empty() {
            vec!["**/*.js".to_string()]
        } else {
            tests
        }
    } else {
        vec!["**/*.js".to_string()]
    };

    // Build the list of (full_path, relative_path) scripts to run.
    let run_list: Vec<(PathBuf, String)> = if let Some(scripts) = &forced {
        // Re-run mode: the listed scripts are relative to `<base>/test`.
        scripts
            .iter()
            .map(|rel| (base_path.join("test").join(rel), rel.clone()))
            .collect()
    } else {
        let mut list = Vec::new();
        for item in test_globs {
            for path in
                glob::glob(base_path.join(format!("test/{item}")).to_str().unwrap()).unwrap()
            {
                let file_name = path.unwrap();
                if exclude_tests.contains(&file_name) {
                    continue;
                }
                let rel = file_name
                    .strip_prefix(base_path.join("test"))
                    .map(|p| {
                        p.to_string_lossy()
                            .trim_start_matches(std::path::MAIN_SEPARATOR)
                            .to_string()
                    })
                    .unwrap_or_else(|_| file_name.to_string_lossy().to_string());
                list.push((file_name, rel));
            }
        }
        list
    };

    for (file_name, rel) in run_list {
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
            &dec,
            &mut outf,
            &mut errf,
            &rel,
            &mut errjson,
        )
        .await;
        outf.flush().unwrap();
        errjson.flush().unwrap();
    }
}

#[tokio::main]
async fn main() {
    main_impl().await;
}
