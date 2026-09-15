// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>
//
// Corpus harness for the core-extraction effort (memo/core.md step 2).
//
// Freezes (script, bytecode blob, expected output) triples under corpus/, then
// every later verification round only needs to: deserialize the frozen blob in
// a fresh runtime, execute it, and diff against the frozen expected output.
//
// Modes:
//   record     run each script directly, serialize its entry value into
//              corpus/blobs/<stem>.bin, write corpus/expected/<stem>.txt; before
//              freezing, the blob is restored in ANOTHER fresh runtime and its
//              output asserted identical to the direct run.
//   verify     (default) restore the frozen blob in a fresh runtime, execute,
//              diff vs frozen expected output; also re-run the source directly
//              as a cross-check.
//   blob-only  restore + execute + diff only. No source compilation at all —
//              this is the only mode a parser-less core crate can run, and the
//              contract every deletion round must keep green.
//
// Script contract:
//   *.js  — global script; top level only declares const/let/class; the LAST
//           statement is an expression whose value is the entry callable.
//   *.mjs — module; sets `globalThis.__ns = { main }`; no printing at top level.
//   All output goes through the host-provided print() native.
//   Core-tier scripts must not call any prototype method library builtins
//   (push/join/map/charAt/...): everything must be executable by a bare core.
//   A first line `//@tier host` marks cases needing host-registered builtins.
//
// Usage: corpus [--record] [--only <substr>] [--tiers core|host|all]

use std::{
    cell::RefCell,
    path::{Path, PathBuf},
    process::ExitCode,
};
use url::Url;
use zuqe::{
    Callback, Context, EvalFlags, EvalName, EvalType, ExitStatus, JSResult, JSValue, ModuleDef,
    Runtime, ScopedContext,
};

const JS_PROP_C_W_E: u32 =
    zuqe::JS_PROP_CONFIGURABLE | zuqe::JS_PROP_WRITABLE | zuqe::JS_PROP_ENUMERABLE;

thread_local! {
    static OUT: RefCell<String> = const { RefCell::new(String::new()) };
}

fn out_line(line: &str) {
    OUT.with_borrow_mut(|o| {
        o.push_str(line);
        o.push('\n');
    });
}

fn out_take() -> String {
    OUT.with_borrow_mut(|o| std::mem::take(o))
}

fn js_print(ctx: &mut ScopedContext, cc: zuqe::CallContext) -> JSResult {
    let mut line = String::new();
    for i in 0..cc.length() {
        if i > 0 {
            line.push(' ');
        }
        let sr = cc.arg(i).to_string(ctx)?;
        line.push_str(&String::from_utf8_lossy(&sr.encode_utf8_bytes(false)));
    }
    out_line(&line);
    Ok(JSValue::UNDEFINED)
}

fn js_module_loader(
    ctx: &mut ScopedContext<'_>,
    file_name: &Url,
    _attrs: JSValue,
    _opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    let path = zuqe::url_to_abspath(file_name).ok_or(-1)?;
    let source = std::fs::read(&path).map_err(|_| -1)?;
    ctx.compile_module(&source, file_name, false)
}

fn new_runtime() -> Runtime {
    let mut rt = Runtime::create().unwrap();
    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());
    rt
}

macro_rules! mk_ctx {
    ($rt:expr) => {
        match $rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        }) {
            Ok(c) => c,
            Err(e) => panic!("create_context failed: {e}"),
        }
    };
}

/// Register the host surface every corpus runtime gets: print/console.log.
fn register_globals(c: &mut Context<'_>) -> Result<(), i32> {
    c.with_root_scope(|mut s| {
        let f = s.create_native_function(Callback::Func(js_print), "print", 0, 0)?;
        let mut g = s.global_obj();
        g.define_property_value_str(&mut s, "print", JSValue::from(f), JS_PROP_C_W_E)?;
        let mut console = s.create_base_object()?;
        console.define_property_value_str(&mut s, "log", JSValue::from(f), JS_PROP_C_W_E)?;
        g.define_property_value_str(&mut s, "console", JSValue::from(console), JS_PROP_C_W_E)?;
        Ok(())
    })
}

struct Case {
    stem: String,
    path: PathBuf,
    url: Url,
    module: bool,
    tier_host: bool,
}

fn discover(scripts_dir: &Path) -> Vec<Case> {
    let mut names: Vec<String> = std::fs::read_dir(scripts_dir)
        .expect("scripts dir")
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| (n.ends_with(".js") || n.ends_with(".mjs")) && !n.contains("-dep."))
        .collect();
    names.sort();

    names
        .into_iter()
        .map(|name| {
            let path = scripts_dir.join(&name);
            let head = std::fs::read_to_string(&path).unwrap_or_default();
            let tier_host = head
                .lines()
                .next()
                .is_some_and(|l| l.contains("//@tier host"));
            let url = zuqe::path_to_url(&path.to_string_lossy()).expect("script url");
            Case {
                stem: name
                    .trim_end_matches(".js")
                    .trim_end_matches(".mjs")
                    .to_owned(),
                path,
                url,
                module: name.ends_with(".mjs"),
                tier_host,
            }
        })
        .collect()
}

/// Evaluate the script/module in an already-prepared context and return its
/// entry value (not yet called).
async fn eval_source(
    ctx: &mut Context<'_>,
    rt: &mut Runtime,
    case: &Case,
) -> Result<JSValue, String> {
    let src = std::fs::read(&case.path).expect("read script");
    let etype = if case.module {
        EvalType::Module
    } else {
        EvalType::Global
    };
    let entry = match ctx
        .eval_wait(
            rt,
            &src,
            &EvalName::BaseUrl(case.url.clone()),
            etype,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => v,
        other => {
            let msg = ctx.with_root_scope(|mut s| {
                let exc = s.take_exception();
                format!("{:?}", exc.to_text(&mut s))
            });
            return Err(format!("eval failed ({other:?}): {msg}"));
        }
    };

    if !case.module {
        return Ok(entry);
    }

    // Modules bridge their exports out via globalThis.__ns; the ns OBJECT is
    // what gets serialized (its `main` property is fetched after restore).
    let repl = EvalName::BaseUrl(case.url.clone());
    let ns = match ctx
        .eval_wait(
            rt,
            b"globalThis.__ns",
            &repl,
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => v,
        other => return Err(format!("cannot read __ns: {other:?}")),
    };
    ctx.with_root_scope(|mut s| {
        let main = ns
            .get_property_str(&mut s, "main")
            .map_err(|_| "no main".to_string())?;
        if !main.is_callable() {
            return Err("module main is not callable".to_string());
        }
        Ok(ns)
    })
}

/// Call the entry value, drain the event loop, collect printed output.
async fn invoke_and_collect(
    ctx: &mut Context<'_>,
    rt: &mut Runtime,
    entry: JSValue,
) -> Result<String, String> {
    out_take(); // reset buffer
    let res: Result<(), String> = ctx.with_root_scope(|mut s| {
        match entry.call(&mut s, JSValue::UNDEFINED, &[]) {
            Ok(r) => {
                // Anchor an async result on the global object so it stays
                // reachable while we drain microtasks below.
                let mut g = s.global_obj();
                let _ = g.define_property_value_str(&mut s, "__last", r, JS_PROP_C_W_E);
                Ok(())
            }
            Err(_) => {
                let exc = s.take_exception();
                Err(format!("{:?}", exc.to_text(&mut s)))
            }
        }
    });
    res?;

    rt.run_async().await;
    Ok(out_take())
}

/// Direct execution: fresh runtime, eval + call + drain.
async fn run_direct(case: &Case) -> Result<String, String> {
    let mut rt = new_runtime();
    let mut ctx = mk_ctx!(&mut rt);
    register_globals(&mut ctx).map_err(|e| format!("register globals failed: {e}"))?;
    let dump_val = eval_source(&mut ctx, &mut rt, case).await?;
    let entry = entry_callable(&mut ctx, dump_val, case.module)?;
    invoke_and_collect(&mut ctx, &mut rt, entry).await
}

/// For global scripts the dumped value is itself the entry; for modules it is
/// the namespace object whose `main` property is the entry.
fn entry_callable(
    ctx: &mut Context<'_>,
    dump_val: JSValue,
    module: bool,
) -> Result<JSValue, String> {
    if !module {
        return Ok(dump_val);
    }
    ctx.with_root_scope(|mut s| {
        let main = dump_val
            .get_property_str(&mut s, "main")
            .map_err(|_| "no main".to_string())?;
        if !main.is_callable() {
            return Err("module main is not callable".to_string());
        }
        Ok(main)
    })
}

async fn make_blob(case: &Case) -> Result<Box<[u8]>, String> {
    let mut rt = new_runtime();
    let mut ctx = mk_ctx!(&mut rt);
    register_globals(&mut ctx).map_err(|e| format!("register globals failed: {e}"))?;
    let entry = eval_source(&mut ctx, &mut rt, case).await?;
    ctx.with_root_scope(|mut s| JSValue::dump(&mut s, entry).map_err(|e| format!("dump: {e}")))
}

/// Restore a blob in a fresh runtime and execute its entry.
async fn run_blob(blob: &[u8], module: bool) -> Result<String, String> {
    let mut rt = new_runtime();
    let mut ctx = mk_ctx!(&mut rt);
    register_globals(&mut ctx).map_err(|e| format!("register globals failed: {e}"))?;
    let restored = ctx.with_root_scope(|mut s| {
        JSValue::restore(&mut s, blob).map_err(|e| format!("restore: {e}"))
    })?;
    let entry = if module {
        ctx.with_root_scope(|mut s| {
            let main = restored
                .get_property_str(&mut s, "main")
                .map_err(|_| "no main".to_string())?;
            if !main.is_callable() {
                let kind = if main.is_undefined() {
                    "undefined"
                } else if main.is_object() {
                    "object"
                } else if main.is_string() {
                    "string"
                } else {
                    "other"
                };
                return Err(format!("restored module main is not callable ({kind})",));
            }
            Ok(main)
        })?
    } else {
        restored
    };
    invoke_and_collect(&mut ctx, &mut rt, entry).await
}

struct Args {
    record: bool,
    blob_only: bool,
    only: Option<String>,
    tiers: String,
}

fn parse_args() -> Args {
    let mut args = Args {
        record: false,
        blob_only: false,
        only: None,
        tiers: "all".into(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "record" | "--record" => args.record = true,
            "blob-only" | "--blob-only" => args.blob_only = true,
            "--only" => args.only = it.next(),
            "--tiers" => args.tiers = it.next().unwrap_or_else(|| "all".into()),
            _ => {}
        }
    }
    args
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = parse_args();

    if args.record || !args.blob_only {
        eprintln!("this build has no parser: only `blob-only` mode is available");
        return ExitCode::FAILURE;
    }

    if args.blob_only && args.record {
        eprintln!("--record and --blob-only are mutually exclusive");
        return ExitCode::FAILURE;
    }
    // Corpus data lives next to this crate; fall back to the authoring repo
    // (zhuque) so both engines verify against one frozen set.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = {
        let local = manifest.join("corpus");
        if local.join("scripts").is_dir() {
            local
        } else {
            manifest.join("../zhuque/corpus")
        }
    };
    let scripts = root.join("scripts");
    let blobs_dir = root.join("blobs");
    let expected_dir = root.join("expected");

    let cases = discover(&scripts);
    let mut failed = 0usize;

    for case in cases {
        if args.tiers != "all" && (args.tiers == "core") == case.tier_host {
            continue;
        }
        if let Some(f) = &args.only {
            if !case.stem.contains(f.as_str()) && !case.path.to_string_lossy().contains(f) {
                continue;
            }
        }

        let tag = format!(
            "{}{}",
            case.stem,
            if case.tier_host { " [host]" } else { "" }
        );
        let outcome: Result<(), String> = if args.record {
            record_case(&case, &blobs_dir, &expected_dir).await
        } else if args.blob_only {
            verify_blob_only(&case, &blobs_dir, &expected_dir).await
        } else {
            verify_case(&case, &blobs_dir, &expected_dir).await
        };

        match outcome {
            Ok(()) => println!("[OK  ] {tag}"),
            Err(e) => {
                failed += 1;
                println!("[FAIL] {tag}: {e}");
            }
        }
    }

    if failed == 0 {
        println!(
            "\ncorpus {} passed",
            if args.record {
                "recorded"
            } else if args.blob_only {
                "blob-only"
            } else {
                "verify"
            }
        );
        ExitCode::SUCCESS
    } else {
        println!("\n{failed} corpus case(s) FAILED");
        ExitCode::FAILURE
    }
}

async fn record_case(case: &Case, blobs_dir: &Path, expected_dir: &Path) -> Result<(), String> {
    let expected = run_direct(case).await.map_err(|e| format!("direct: {e}"))?;

    let blob = make_blob(case).await?;
    let back = run_blob(&blob, case.module)
        .await
        .map_err(|e| format!("restore-run: {e}"))?;
    if back != expected {
        return Err(format!(
            "restore-run output differs from direct run:\n--direct--\n{expected}--restore--\n{back}"
        ));
    }

    std::fs::create_dir_all(blobs_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(expected_dir).map_err(|e| e.to_string())?;
    let blob_path = blobs_dir.join(format!("{}.bin", case.stem));
    let exp_path = expected_dir.join(format!("{}.txt", case.stem));
    std::fs::write(&blob_path, &blob).map_err(|e| e.to_string())?;
    std::fs::write(&exp_path, &expected).map_err(|e| e.to_string())?;
    Ok(())
}

async fn verify_case(case: &Case, blobs_dir: &Path, expected_dir: &Path) -> Result<(), String> {
    let blob_path = blobs_dir.join(format!("{}.bin", case.stem));
    let exp_path = expected_dir.join(format!("{}.txt", case.stem));
    let blob = std::fs::read(&blob_path)
        .map_err(|_| format!("frozen blob missing ({})", blob_path.display()))?;
    let expected = std::fs::read_to_string(&exp_path)
        .map_err(|_| format!("frozen expected missing ({})", exp_path.display()))?;

    let got_blob = run_blob(&blob, case.module)
        .await
        .map_err(|e| format!("restore-run: {e}"))?;
    if got_blob != expected {
        return Err(format!(
            "blob execution differs from frozen expected:\n--expected--\n{expected}--got--\n{got_blob}"
        ));
    }

    // Cross-check: direct run of the source must still match the frozen output.
    let got_direct = run_direct(case).await.map_err(|e| format!("direct: {e}"))?;
    if got_direct != expected {
        return Err(format!(
            "direct execution differs from frozen expected:\n--expected--\n{expected}--got--\n{got_direct}"
        ));
    }
    Ok(())
}

/// Parser-less mode: only the frozen blob path (restore → execute → diff).
async fn verify_blob_only(
    case: &Case,
    blobs_dir: &Path,
    expected_dir: &Path,
) -> Result<(), String> {
    let blob_path = blobs_dir.join(format!("{}.bin", case.stem));
    let exp_path = expected_dir.join(format!("{}.txt", case.stem));
    let blob = std::fs::read(&blob_path)
        .map_err(|_| format!("frozen blob missing ({})", blob_path.display()))?;
    let expected = std::fs::read_to_string(&exp_path)
        .map_err(|_| format!("frozen expected missing ({})", exp_path.display()))?;

    let got = run_blob(&blob, case.module)
        .await
        .map_err(|e| format!("restore-run: {e}"))?;
    if got != expected {
        return Err(format!(
            "blob execution differs from frozen expected:\n--expected--\n{expected}--got--\n{got}"
        ));
    }
    Ok(())
}
