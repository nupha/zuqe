// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>
//
// Bytecode persistence round-trip test (design doc bytecode-persistent.md).
//
// Strategy: every case is evaluated in one Runtime (rt1), serialized to an
// in-memory blob, then deserialized in a SECOND, independent Runtime (rt2).
// Cross-Runtime round-trip is what actually proves the blob carries no
// context-local GC pointers (a leaked GcRef would crash or yield garbage on
// rt2). A plain value comparison (to_string / json) then decides OK vs FAIL.

use std::process::ExitCode;

use url::Url;
use zuqe::{
    Context, EvalFlags, EvalName, EvalType, ExitStatus, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE,
    JSValue, ModuleDef, Runtime, ScopedContext,
};

fn new_runtime() -> Runtime {
    let mut rt = Runtime::create().unwrap();
    rt.set_module_loader(None, js_module_loader, std::ptr::null_mut());
    rt
}

fn js_module_loader(
    ctx: &mut ScopedContext<'_>,
    file_name: &Url,
    _attrs: JSValue,
    _opaque: *mut std::ffi::c_void,
) -> Result<ModuleDef, i32> {
    let path = zuqe::url_to_abspath(file_name)
        .ok_or_else(|| ctx.throw_reference_error("module is not a file:// URL"))?;
    let source = std::fs::read_to_string(&path)
        .map_err(|e| ctx.throw_reference_error(&format!("cannot read module: {e}")))?;
    ctx.compile_module(source.as_bytes(), file_name, false)
}

macro_rules! mk_ctx {
    ($rt:expr) => {
        $rt.create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            Ok(())
        })
        .unwrap()
    };
}

fn repr(s: &mut ScopedContext<'_>, v: JSValue) -> String {
    if v.is_object() {
        match s.json_stringify(v, JSValue::UNDEFINED, JSValue::UNDEFINED) {
            Ok(j) => string_of(s, j),
            Err(_) => "<json failed>".to_string(),
        }
    } else {
        string_of(s, v)
    }
}

fn string_of(s: &mut ScopedContext<'_>, v: JSValue) -> String {
    match v.to_string(s) {
        Ok(sr) => String::from_utf8_lossy(&sr.encode_utf8_bytes(false)).into_owned(),
        Err(_) => "<to_string failed>".to_string(),
    }
}

/// Serialize `script`'s value out of rt1, deserialize in a fresh rt2, and
/// compare the textual representation of original vs restored.
async fn roundtrip(script: &str) -> (String, String, bool) {
    let mut rt1 = new_runtime();
    let mut ctx1 = mk_ctx!(&mut rt1);
    let name = zuqe::path_to_url("repl.js").expect("repl.js url");
    let val1 = match ctx1
        .eval_wait(
            &mut rt1,
            script.as_bytes(),
            &EvalName::BaseUrl(name),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => v,
        other => panic!("eval failed for {script:?}: {other:?}"),
    };
    let (orig, blob) = ctx1.with_root_scope(|mut s| {
        let orig = repr(&mut s, val1);
        let blob = JSValue::dump(&mut s, val1).expect("serialize");
        (orig, blob)
    });

    let mut rt2 = new_runtime();
    let mut ctx2 = mk_ctx!(&mut rt2);
    let (back, ok) = ctx2.with_root_scope(|mut s| {
        let back = JSValue::restore(&mut s, &blob).expect("deserialize");
        let back_repr = repr(&mut s, back);
        let ok = orig == back_repr;
        (back_repr, ok)
    });
    (orig, back, ok)
}

async fn roundtrip_module(source: &str) -> (String, String, bool) {
    let mut rt1 = new_runtime();
    let mut ctx1 = mk_ctx!(&mut rt1);
    let name = zuqe::path_to_url("mod.mjs").expect("mod.mjs url");
    let val1 = match ctx1
        .eval_wait(
            &mut rt1,
            source.as_bytes(),
            &EvalName::BaseUrl(name),
            EvalType::Module,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => v,
        other => panic!("eval module failed: {other:?}"),
    };
    let (orig, blob) = ctx1.with_root_scope(|mut s| {
        let orig = repr(&mut s, val1);
        let blob = JSValue::dump(&mut s, val1).expect("serialize module");
        (orig, blob)
    });

    let mut rt2 = new_runtime();
    let mut ctx2 = mk_ctx!(&mut rt2);
    let (back, ok) = ctx2.with_root_scope(|mut s| {
        let back = JSValue::restore(&mut s, &blob).expect("deserialize module");
        let back_repr = repr(&mut s, back);
        let ok = orig == back_repr;
        (back_repr, ok)
    });
    (orig, back, ok)
}

/// Like [`roundtrip_module`] but the blob is written to a file and reloaded in
/// a FRESH Runtime + Context, proving module bytecode is fully self-contained.
async fn roundtrip_module_file(source: &str) -> (String, String, bool) {
    let path = std::env::temp_dir().join("zhuque_persist_module.bin");
    let (orig, blob) = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        let name = zuqe::path_to_url("mod.mjs").expect("mod.mjs url");
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                source.as_bytes(),
                &EvalName::BaseUrl(name),
                EvalType::Module,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => panic!("eval module failed: {other:?}"),
        };
        ctx1.with_root_scope(|mut s| {
            let orig = repr(&mut s, val1);
            let blob = JSValue::dump(&mut s, val1).expect("serialize module");
            (orig, blob)
        })
    };
    std::fs::write(&path, &blob).expect("write module blob to file");

    let blob = std::fs::read(&path).expect("read module blob from file");
    let (back, ok) = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        ctx2.with_root_scope(|mut s| {
            let back = JSValue::restore(&mut s, &blob).expect("deserialize module from file");
            let back_repr = repr(&mut s, back);
            let ok = orig == back_repr;
            (back_repr, ok)
        })
    };
    let _ = std::fs::remove_file(&path);
    (orig, back, ok)
}

async fn check(name: &str, script: &str) {
    let (orig, back, ok) = roundtrip(script).await;
    println!(
        "[{}] {name}\n    orig: {orig}\n    back: {back}",
        if ok { "OK " } else { "FAIL" }
    );
    if !ok {
        std::process::exit(1);
    }
}

/// Serialize in rt1, write the blob to a file, then load it in a FRESH Runtime
/// + Context (different atom table, different address space) and invoke the
/// restored function. This is the real "no-dependency migration" requirement:
/// the persisted blob must be fully self-contained.
async fn check_call_file(name: &str, script: &str, call_args: &[i32], expect: i32) {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        let url = zuqe::path_to_url("repl.js").expect("repl.js url");
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                script.as_bytes(),
                &EvalName::BaseUrl(url),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => panic!("eval failed for {script:?}: {other:?}"),
        };
        ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, val1).expect("serialize"))
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let ok = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        ctx2.with_root_scope(|mut s| {
            let back = JSValue::restore(&mut s, &blob).expect("deserialize from file");
            if !back.is_callable() {
                println!("[FAIL] {name}: restored value is not callable");
                return false;
            }
            let args: Vec<JSValue> = call_args.iter().map(|&a| JSValue::from(a)).collect();
            match back.call(&mut s, JSValue::UNDEFINED, &args) {
                Ok(r) => match r.to_number(&mut s) {
                    Ok(n) => n.to_i32() == expect,
                    Err(_) => {
                        println!("[FAIL] {name}: call returned non-number");
                        false
                    }
                },
                Err(_) => {
                    let exc = s.take_exception();
                    let msg = exc.to_text(&mut s);
                    println!("[FAIL] {name}: call threw: {:?}", msg);
                    false
                }
            }
        })
    };
    let _ = std::fs::remove_file(&path);
    println!(
        "[{}] {name} (CROSS-RUNTIME file reload, call {:?} -> expect {expect})",
        if ok { "OK " } else { "FAIL" },
        call_args
    );
    if !ok {
        std::process::exit(1);
    }
}

async fn check_call(name: &str, script: &str, call_args: &[i32], expect: i32) {
    let mut rt1 = new_runtime();
    let mut ctx1 = mk_ctx!(&mut rt1);
    let url = zuqe::path_to_url("repl.js").expect("repl.js url");
    let val1 = match ctx1
        .eval_wait(
            &mut rt1,
            script.as_bytes(),
            &EvalName::BaseUrl(url),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => v,
        other => panic!("eval failed for {script:?}: {other:?}"),
    };
    let blob = ctx1.with_root_scope(|mut s| match JSValue::dump(&mut s, val1) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("serialize closure failed: {e:?}");
            std::process::exit(1);
        }
    });

    // The persisted blob is now self-contained (atom symbol table + per-element
    // consts), so round-trip and invoke the closure in rt1 (single-context).
    let ok = ctx1.with_root_scope(|mut s| {
        let back = JSValue::restore(&mut s, &blob).expect("deserialize closure");
        if !back.is_callable() {
            println!("[FAIL] {name}: restored value is not callable");
            return false;
        }
        let args: Vec<JSValue> = call_args.iter().map(|&a| JSValue::from(a)).collect();
        match back.call(&mut s, JSValue::UNDEFINED, &args) {
            Ok(r) => match r.to_number(&mut s) {
                Ok(n) => n.to_i32() == expect,
                Err(_) => {
                    println!("[FAIL] {name}: call returned non-number");
                    false
                }
            },
            Err(_) => {
                let exc = s.take_exception();
                let msg = exc.to_text(&mut s);
                println!("[FAIL] {name}: call threw: {:?}", msg);
                false
            }
        }
    });
    println!(
        "[{}] {name} (call {:?} -> expect {expect})",
        if ok { "OK " } else { "FAIL" },
        call_args
    );
    if !ok {
        std::process::exit(1);
    }
}

/// Serialize a module's *namespace* (its exported values), reload it in a
/// FRESH Runtime, then prove the restored exports are still executable: call an
/// exported function and read an exported const. This proves the persisted
/// module bytecode is fully self-contained and runnable across runtimes.
///
/// The module is first executed in rt1 (which evaluates `export` bindings and
/// populates the namespace we bridge out via `globalThis.__ns`), then that
/// namespace object 閳?holding the module's exported function `inc` and const
/// `answer` 閳?is serialized. On rt2 we deserialize and invoke `inc`.
async fn check_module_call_file(name: &str, source: &str) {
    let path = std::env::temp_dir().join("zhuque_persist_module_call.bin");
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        let url = zuqe::path_to_url("mod.mjs").expect("mod.mjs url");
        match ctx1
            .eval_wait(
                &mut rt1,
                source.as_bytes(),
                &EvalName::BaseUrl(url),
                EvalType::Module,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(_) => {}
            other => panic!("eval module failed: {other:?}"),
        }
        // Bridge the module's exports out so we can serialize the namespace.
        let ns_val = match ctx1
            .eval_wait(
                &mut rt1,
                b"globalThis.__ns",
                &EvalName::BaseUrl(zuqe::path_to_url("repl.js").expect("repl.js url")),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => panic!("read module ns failed: {other:?}"),
        };
        ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, ns_val).expect("serialize module ns"))
    };
    std::fs::write(&path, &blob).expect("write module blob to file");

    let blob = std::fs::read(&path).expect("read module blob from file");
    let ok = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        ctx2.with_root_scope(|mut s| {
            let back = JSValue::restore(&mut s, &blob).expect("deserialize module ns from file");
            if !back.is_object() {
                println!("[FAIL] {name}: restored module namespace is not an object");
                return false;
            }
            // exported const `answer`
            let answer = match back.get_property_str(&mut s, "answer") {
                Ok(v) => match v.to_number(&mut s) {
                    Ok(n) => n.to_i32(),
                    Err(_) => {
                        println!("[FAIL] {name}: answer not a number");
                        return false;
                    }
                },
                Err(_) => {
                    println!("[FAIL] {name}: cannot read export `answer`");
                    return false;
                }
            };
            if answer != 42 {
                println!("[FAIL] {name}: answer = {answer}, expect 42");
                return false;
            }
            // exported function `inc`
            let inc = match back.get_property_str(&mut s, "inc") {
                Ok(v) => v,
                Err(_) => {
                    println!("[FAIL] {name}: cannot read export `inc`");
                    return false;
                }
            };
            if !inc.is_callable() {
                println!("[FAIL] {name}: export `inc` is not callable");
                return false;
            }
            match inc.call(&mut s, JSValue::UNDEFINED, &[JSValue::from(41)]) {
                Ok(r) => match r.to_number(&mut s) {
                    Ok(n) => {
                        let got = n.to_i32();
                        if got != 42 {
                            println!("[FAIL] {name}: inc(41) = {got}, expect 42");
                            return false;
                        }
                        true
                    }
                    Err(_) => {
                        println!("[FAIL] {name}: inc returned non-number");
                        false
                    }
                },
                Err(_) => {
                    let exc = s.take_exception();
                    let msg = exc.to_text(&mut s);
                    println!("[FAIL] {name}: inc threw: {:?}", msg);
                    false
                }
            }
        })
    };
    let _ = std::fs::remove_file(&path);
    println!(
        "[{}] {name} (CROSS-RUNTIME module reload + call export inc(41))",
        if ok { "OK " } else { "FAIL" }
    );
    if !ok {
        std::process::exit(1);
    }
}

async fn check_call_file_f64(name: &str, script: &str, call_args: &[f64], expect: f64) {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        let url = zuqe::path_to_url("repl.js").expect("repl.js url");
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                script.as_bytes(),
                &EvalName::BaseUrl(url),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => panic!("eval failed for {script:?}: {other:?}"),
        };
        ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, val1).expect("serialize"))
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let ok = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        ctx2.with_root_scope(|mut s| {
            let back = JSValue::restore(&mut s, &blob).expect("deserialize from file");
            if !back.is_callable() {
                println!("[FAIL] {name}: restored value is not callable");
                return false;
            }
            let args: Vec<JSValue> = call_args.iter().map(|&a| JSValue::from(a)).collect();
            match back.call(&mut s, JSValue::UNDEFINED, &args) {
                Ok(r) => match r.to_number(&mut s) {
                    Ok(n) => (n.to_f64() - expect).abs() < 1e-10,
                    Err(_) => {
                        println!("[FAIL] {name}: call returned non-number");
                        false
                    }
                },
                Err(_) => {
                    let exc = s.take_exception();
                    let msg = exc.to_text(&mut s);
                    println!("[FAIL] {name}: call threw: {:?}", msg);
                    false
                }
            }
        })
    };
    let _ = std::fs::remove_file(&path);
    println!(
        "[{}] {name} (CROSS-RUNTIME file reload, call {:?} -> expect {expect})",
        if ok { "OK " } else { "FAIL" },
        call_args
    );
    if !ok {
        std::process::exit(1);
    }
}

/// Register a native `myAdd(a, b)` on the runtime's global object. Natives are
/// persisted by JS name only, so every runtime that must resolve a persisted
/// native has to re-expose it under the same name.
fn register_my_add(c: &mut Context<'_>) {
    use zuqe::Callback;
    c.with_root_scope(|mut s| {
        let f = s
            .create_native_function(
                Callback::Func(|ctx, cc| {
                    let a = cc.arg(0).to_number(ctx).map_err(|_| -1)?.to_i32();
                    let b = cc.arg(1).to_number(ctx).map_err(|_| -1)?.to_i32();
                    Ok(JSValue::from(a + b))
                }),
                "myAdd",
                2,
                0,
            )
            .expect("create native");
        s.global_obj()
            .define_property_value_str(
                &mut s,
                "myAdd",
                JSValue::from(f),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .expect("register native");
    });
}

/// Serialize a JS closure whose body *calls a native function*, reload it in a
/// FRESH Runtime (which must re-expose that native by the same global name), then
/// invoke. Proves native functions are persisted by name only (QuickJS-aligned):
/// the blob records the function's JS `name`; restore resolves it via the target
/// runtime's global object. If the name is absent there, restore fails.
async fn check_native_call(name: &str, expect: i32) {
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        register_my_add(&mut ctx1);
        let url = zuqe::path_to_url("repl.js").expect("repl.js url");
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                b"(function(x){ return myAdd(x, 1) })",
                &EvalName::BaseUrl(url),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => panic!("eval failed: {other:?}"),
        };
        ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, val1).expect("serialize"))
    };

    let ok = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        register_my_add(&mut ctx2); // re-expose native by the same global name
        ctx2.with_root_scope(|mut s| {
            let back = JSValue::restore(&mut s, &blob).expect("deserialize");
            if !back.is_callable() {
                println!("[FAIL] {name}: restored value is not callable");
                return false;
            }
            let args = [JSValue::from(41)];
            match back.call(&mut s, JSValue::UNDEFINED, &args) {
                Ok(r) => match r.to_number(&mut s) {
                    Ok(n) => n.to_i32() == expect,
                    Err(_) => {
                        println!("[FAIL] {name}: call returned non-number");
                        false
                    }
                },
                Err(_) => {
                    let exc = s.take_exception();
                    println!("[FAIL] {name}: call threw: {:?}", exc.to_text(&mut s));
                    false
                }
            }
        })
    };
    println!(
        "[{}] {name} (closure calling native `myAdd`, expect {expect})",
        if ok { "OK " } else { "FAIL" }
    );
    if !ok {
        std::process::exit(1);
    }
}

/// Persist the native `myAdd` function VALUE directly (no wrapper closure),
/// reload in a fresh Runtime that re-exposes it under the same name, and call
/// it. Natives survive as a name reference, so restore yields the target
/// runtime's OWN function object. Returns true on success (non-fatal).
async fn check_native_direct(name: &str, expect: i32) -> bool {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        register_my_add(&mut ctx1);
        let native = ctx1.with_root_scope(|mut s| {
            let gobj = s.global_obj();
            gobj.get_property_str(&mut s, "myAdd").expect("read myAdd")
        });
        match ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, native)) {
            Ok(b) => b,
            Err(e) => {
                println!("[FAIL] {name}: serialize native: {e:?}");
                return false;
            }
        }
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let ok = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        register_my_add(&mut ctx2);
        ctx2.with_root_scope(|mut s| {
            let back = match JSValue::restore(&mut s, &blob) {
                Ok(v) => v,
                Err(e) => {
                    println!("[FAIL] {name}: restore native: {e:?}");
                    return false;
                }
            };
            if !back.is_callable() {
                println!("[FAIL] {name}: restored native is not callable");
                return false;
            }
            match back.call(
                &mut s,
                JSValue::UNDEFINED,
                &[JSValue::from(40), JSValue::from(2)],
            ) {
                Ok(r) => match r.to_number(&mut s) {
                    Ok(n) => n.to_i32() == expect,
                    Err(_) => {
                        println!("[FAIL] {name}: call returned non-number");
                        false
                    }
                },
                Err(_) => {
                    let exc = s.take_exception();
                    println!("[FAIL] {name}: call threw: {:?}", exc.to_text(&mut s));
                    false
                }
            }
        })
    };
    let _ = std::fs::remove_file(&path);
    println!(
        "[{}] {name} (native myAdd value round-trip, expect {expect})",
        if ok { "OK " } else { "FAIL" }
    );
    ok
}

/// Persist the native `myAdd` VALUE but reload into a runtime that does NOT
/// expose that name. Restore must fail with a clear error (natives are bound by
/// name at load time, never rebuilt). Returns true when restore is rejected.
async fn check_native_missing(name: &str) -> bool {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        register_my_add(&mut ctx1);
        let native = ctx1.with_root_scope(|mut s| {
            let gobj = s.global_obj();
            gobj.get_property_str(&mut s, "myAdd").expect("read myAdd")
        });
        ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, native))
            .expect("serialize native")
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let (ok, msg) = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2); // deliberately does NOT register myAdd
        ctx2.with_root_scope(|mut s| match JSValue::restore(&mut s, &blob) {
            Err(e) => (true, format!("restore rejected: {e:?}")),
            Ok(v) => (
                false,
                format!("restore unexpectedly succeeded: {}", repr(&mut s, v)),
            ),
        })
    };
    let _ = std::fs::remove_file(&path);
    println!("[{}] {name} ({msg})", if ok { "OK " } else { "FAIL" });
    ok
}

/// Persist a closure whose CAPTURED variable is a native function (`let g =
/// myAdd`), reload with the native re-exposed, and call. The native travels
/// through the global-lexical snapshot as a name reference.
async fn check_native_capture(name: &str, expect: i32) -> bool {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        register_my_add(&mut ctx1);
        let url = zuqe::path_to_url("repl.js").expect("repl.js url");
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                b"let g = myAdd; (function(x){ return g(x, 1) })",
                &EvalName::BaseUrl(url),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => {
                println!("[FAIL] {name}: eval failed: {other:?}");
                return false;
            }
        };
        match ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, val1)) {
            Ok(b) => b,
            Err(e) => {
                println!("[FAIL] {name}: serialize: {e:?}");
                return false;
            }
        }
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let ok = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2);
        register_my_add(&mut ctx2);
        ctx2.with_root_scope(|mut s| {
            let back = match JSValue::restore(&mut s, &blob) {
                Ok(v) => v,
                Err(e) => {
                    println!("[FAIL] {name}: restore: {e:?}");
                    return false;
                }
            };
            let args = [JSValue::from(41)];
            match back.call(&mut s, JSValue::UNDEFINED, &args) {
                Ok(r) => match r.to_number(&mut s) {
                    Ok(n) => n.to_i32() == expect,
                    Err(_) => {
                        println!("[FAIL] {name}: call returned non-number");
                        false
                    }
                },
                Err(_) => {
                    let exc = s.take_exception();
                    println!("[FAIL] {name}: call threw: {:?}", exc.to_text(&mut s));
                    false
                }
            }
        })
    };
    let _ = std::fs::remove_file(&path);
    println!(
        "[{}] {name} (closure capturing native `myAdd`, expect {expect})",
        if ok { "OK " } else { "FAIL" }
    );
    ok
}

/// Same as [`check_native_capture`] but the target runtime does NOT expose
/// `myAdd`; restoring the closure must fail because its captured native cannot
/// be resolved by name.
async fn check_native_capture_missing(name: &str) -> bool {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        register_my_add(&mut ctx1);
        let url = zuqe::path_to_url("repl.js").expect("repl.js url");
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                b"let g = myAdd; (function(x){ return g(x, 1) })",
                &EvalName::BaseUrl(url),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => {
                println!("[FAIL] {name}: eval failed: {other:?}");
                return false;
            }
        };
        ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, val1))
            .expect("serialize")
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let (ok, msg) = {
        let mut rt2 = new_runtime();
        let mut ctx2 = mk_ctx!(&mut rt2); // no myAdd on purpose
        ctx2.with_root_scope(|mut s| match JSValue::restore(&mut s, &blob) {
            Err(e) => (true, format!("restore rejected: {e:?}")),
            Ok(v) => (
                false,
                format!("restore unexpectedly succeeded: {}", repr(&mut s, v)),
            ),
        })
    };
    let _ = std::fs::remove_file(&path);
    println!("[{}] {name} ({msg})", if ok { "OK " } else { "FAIL" });
    ok
}

/// General cross-runtime helper: eval `script` in rt1, dump its value to a
/// file, restore it in a FRESH rt2 as `globalThis.__v`, then eval `check_js`
/// and compare its textual result with `expect`. Returns true on match
/// (non-fatal, reports [FAIL] but keeps going so every edge case is observed).
async fn check_roundtrip_js(name: &str, script: &str, check_js: &str, expect: &str) -> bool {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let repl = zuqe::path_to_url("repl.js").expect("repl.js url");
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        let val1 = match ctx1
            .eval_wait(
                &mut rt1,
                script.as_bytes(),
                &EvalName::BaseUrl(repl.clone()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => {
                println!("[FAIL] {name}: eval failed: {other:?}");
                return false;
            }
        };
        match ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, val1)) {
            Ok(b) => b,
            Err(e) => {
                println!("[FAIL] {name}: serialize: {e:?}");
                return false;
            }
        }
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let mut rt2 = new_runtime();
    let mut ctx2 = mk_ctx!(&mut rt2);
    let restore_ok = ctx2.with_root_scope(|mut s| {
        let back = match JSValue::restore(&mut s, &blob) {
            Ok(v) => v,
            Err(e) => {
                println!("[FAIL] {name}: restore: {e:?}");
                return false;
            }
        };
        s.global_obj()
            .define_property_value_str(
                &mut s,
                "__v",
                JSValue::from(back),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .is_ok()
    });
    if !restore_ok {
        let _ = std::fs::remove_file(&path);
        return false;
    }

    let got = match ctx2
        .eval_wait(
            &mut rt2,
            check_js.as_bytes(),
            &EvalName::BaseUrl(repl),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => ctx2.with_root_scope(|mut s| repr(&mut s, v)),
        other => {
            let msg = ctx2.with_root_scope(|mut s| {
                let exc = s.take_exception();
                format!("{:?}", exc.to_text(&mut s))
            });
            println!("[FAIL] {name}: check_js eval failed: {other:?}\n    exception: {msg}");
            let _ = std::fs::remove_file(&path);
            return false;
        }
    };
    let _ = std::fs::remove_file(&path);

    let ok = got == expect;
    println!(
        "[{}] {name}\n    expect: {expect}\n    got:    {got}",
        if ok { "OK " } else { "FAIL" }
    );
    ok
}

/// Cross-runtime module namespace helper. The module source must bridge its
/// exports out via `globalThis.__ns`. rt1 evals the module, dumps `__ns`, rt2
/// restores it as `globalThis.__v`, then `check_js` is evaluated and compared
/// textually with `expect`.
async fn check_module_roundtrip_js(
    name: &str,
    source: &str,
    base: Url,
    check_js: &str,
    expect: &str,
) -> bool {
    let path = std::env::temp_dir().join(format!(
        "zhuque_persist_{}.bin",
        name.replace([' ', '/', '\\', ':'], "_")
    ));
    let repl = zuqe::path_to_url("repl.js").expect("repl.js url");
    let blob = {
        let mut rt1 = new_runtime();
        let mut ctx1 = mk_ctx!(&mut rt1);
        match ctx1
            .eval_wait(
                &mut rt1,
                source.as_bytes(),
                &EvalName::BaseUrl(base),
                EvalType::Module,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(_) => {}
            other => {
                let msg = ctx1.with_root_scope(|mut s| {
                    let exc = s.take_exception();
                    format!("{:?}", exc.to_text(&mut s))
                });
                println!("[FAIL] {name}: module eval failed: {other:?}\n    exception: {msg}");
                return false;
            }
        }
        let ns = match ctx1
            .eval_wait(
                &mut rt1,
                b"globalThis.__ns",
                &EvalName::BaseUrl(repl.clone()),
                EvalType::Global,
                EvalFlags::empty(),
            )
            .await
        {
            ExitStatus::Value(v) => v,
            other => {
                println!("[FAIL] {name}: cannot read module ns: {other:?}");
                return false;
            }
        };
        match ctx1.with_root_scope(|mut s| JSValue::dump(&mut s, ns)) {
            Ok(b) => b,
            Err(e) => {
                println!("[FAIL] {name}: serialize ns: {e:?}");
                return false;
            }
        }
    };
    std::fs::write(&path, &blob).expect("write blob to file");

    let blob = std::fs::read(&path).expect("read blob from file");
    let mut rt2 = new_runtime();
    let mut ctx2 = mk_ctx!(&mut rt2);
    let restore_ok = ctx2.with_root_scope(|mut s| {
        let back = match JSValue::restore(&mut s, &blob) {
            Ok(v) => v,
            Err(e) => {
                println!("[FAIL] {name}: restore ns: {e:?}");
                return false;
            }
        };
        s.global_obj()
            .define_property_value_str(
                &mut s,
                "__v",
                JSValue::from(back),
                JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
            )
            .is_ok()
    });
    if !restore_ok {
        let _ = std::fs::remove_file(&path);
        return false;
    }

    let got = match ctx2
        .eval_wait(
            &mut rt2,
            check_js.as_bytes(),
            &EvalName::BaseUrl(repl),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .await
    {
        ExitStatus::Value(v) => ctx2.with_root_scope(|mut s| repr(&mut s, v)),
        other => {
            let msg = ctx2.with_root_scope(|mut s| {
                let exc = s.take_exception();
                format!("{:?}", exc.to_text(&mut s))
            });
            println!("[FAIL] {name}: check_js eval failed: {other:?}\n    exception: {msg}");
            let _ = std::fs::remove_file(&path);
            return false;
        }
    };
    let _ = std::fs::remove_file(&path);

    let ok = got == expect;
    println!(
        "[{}] {name}\n    expect: {expect}\n    got:    {got}",
        if ok { "OK " } else { "FAIL" }
    );
    ok
}

#[tokio::main]
async fn main() -> ExitCode {
    check("undefined", "undefined").await;
    check("null", "null").await;
    check("bool", "true").await;
    check("int", "42").await;
    check("negative int", "-7").await;
    check("float", "3.14159").await;
    check("nan", "NaN").await;
    check("infinity", "Infinity").await;
    check("string ascii", "'hello world'").await;
    check("string utf8", "'娴ｇ姴銈介敍灞肩瑯閻?棣冨'").await;
    check("string empty", "''").await;
    check("symbol global", "Symbol.for('k')").await;
    check("bigint", "12345678901234567890123456789n").await;
    check("array flat", "[1, 2, 3, 'x', true, null]").await;
    check("array nested", "[[1, [2, 3]], [4, 5]]").await;
    check("object flat", "({a: 1, b: 'two', c: true, d: null})").await;
    check("object nested", "({x: {y: {z: 42}}, arr: [1, 2]})").await;
    check(
        "object with all primitives",
        "({n: 1, s: 'str', b: false, u: undefined, nu: null, f: 1.5, bi: 10n})",
    )
    .await;

    check_call("closure no capture", "(function(x){return x+1})", &[41], 42).await;
    check_call(
        "closure with capture",
        "let base = 100; (function(x){return x + base})",
        &[5],
        105,
    )
    .await;
    check_call(
        "closure nested",
        "function make(a){return function(b){return a*b}}; make(3)",
        &[4],
        12,
    )
    .await;

    // CROSS-RUNTIME: serialize to a file, reload in a fresh Runtime + Context,
    // then invoke. Proves the persisted bytecode is fully self-contained.
    check_call_file(
        "closure file no capture",
        "(function(x){return x+1})",
        &[41],
        42,
    )
    .await;
    // Nested closure capturing a local lexical var of its enclosing function.
    check_call_file(
        "nested closure capture local",
        "function outer(){let base=100;return function(x){return x+base;};}outer()",
        &[5],
        105,
    )
    .await;

    // Top-level closure capturing a *global* lexical var (let at top level).
    check_call_file(
        "top closure capture global lexical",
        "let base=100;(function(x){return x+base})",
        &[5],
        105,
    )
    .await;

    // Method call on a built-in native object (`Math.abs`). The bytecode is
    // `LOAD Math` + `GET_FIELD "abs"` + `CALL_METHOD`: `abs` is only an atom
    // property name, the native function is resolved from the `Math` object at
    // runtime and never enters the constant pool, so no native dump is involved.
    check_call_file(
        "method call on native object (Math.abs)",
        "(function(x){ return Math.abs(x) })",
        &[-41],
        41,
    )
    .await;

    // Function calling native `Math.sin`. Float result needs f64 comparison.
    check_call_file_f64(
        "function calling native Math.sin(0)",
        "(function(x){ return Math.sin(x) })",
        &[0.0],
        0.0,
    )
    .await;
    check_call_file_f64(
        "function calling native Math.sin(PI/2)",
        "(function(x){ return Math.sin(x) })",
        &[std::f64::consts::FRAC_PI_2],
        1.0,
    )
    .await;

    let mod_src =
        "export const answer = 42;\nexport function inc(x){return x+1}\nexport let name = 'mod';\n";
    let (orig, back, ok) = roundtrip_module(mod_src).await;
    println!(
        "[{}] module\n    orig: {orig}\n    back: {back}",
        if ok { "OK " } else { "FAIL" }
    );
    if !ok {
        return ExitCode::FAILURE;
    }

    let (orig2, back2, ok2) = roundtrip_module_file(mod_src).await;
    println!(
        "[{}] module (CROSS-RUNTIME file reload)\n    orig: {orig2}\n    back: {back2}",
        if ok2 { "OK " } else { "FAIL" }
    );
    if !ok2 {
        return ExitCode::FAILURE;
    }

    // Prove the restored module is still *executable*: reload in a fresh
    // Runtime and call an exported function from its namespace.
    check_module_call_file(
        "module call export",
        "export const answer = 42;\nexport function inc(x){return x+1}\nexport let name = 'mod';\nglobalThis.__ns = { answer, inc, name };\n",
    )
    .await;

    // Prove a JS closure whose body calls a native function survives
    // round-trip: the native is recorded by name and re-resolved on the target
    // runtime's global object (QuickJS-aligned).
    check_native_call("closure calling native function", 42).await;

    // function edge cases (cross-runtime file reload)
    let mut failed = 0;
    failed += !check_roundtrip_js(
        "default params",
        "(function(a, b=10){ return a+b })",
        "__v(5)",
        "15",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "rest params",
        "(function(...args){ return args.length })",
        "__v(1,2,3)",
        "3",
    )
    .await as i32;
    failed +=
        !check_roundtrip_js("arrow function", "((x)=>{ return x*3 })", "__v(5)", "15").await as i32;
    failed += !check_roundtrip_js(
        "nested closure chain",
        "function f(){let a=1; return function(){let b=2; return function(){return a+b}};} f()()",
        "__v()",
        "3",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "string consts",
        "(function(){ return 'ab' + 'cd' })",
        "__v()",
        "abcd",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "global const capture",
        "const c = 42; (function(){ return c })",
        "__v()",
        "42",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "this binding",
        "(function(){ return this.v })",
        "__v.call({v: 99})",
        "99",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "name and length",
        "(function myFunc(a,b){ return a+b })",
        "__v.name + '/' + __v.length",
        "myFunc/2",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "global builtin JSON",
        "(function(){ return JSON.stringify({a: 1}) })",
        "__v()",
        r#"{"a":1}"#,
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "captured function value",
        "let add = (a,b)=>a+b; (function(){ return add(2,3) })",
        "__v()",
        "5",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "cyclic object capture",
        "let o = {}; o.self = o; (function(){ return o })",
        "__v().self === __v()",
        "true",
    )
    .await as i32;
    failed += !check_roundtrip_js(
        "generator function",
        "(function* g(){ yield 1; yield 2 })",
        "String(__v().next().value)",
        "1",
    )
    .await as i32;

    // module edge cases (cross-runtime namespace reload)
    let mod_dir = std::env::temp_dir().join("zhuque_persist_mod");
    std::fs::create_dir_all(&mod_dir).expect("create module temp dir");
    std::fs::write(
        mod_dir.join("dep.mjs"),
        "export function add(a,b){return a+b}\nexport const BASE = 10;\n",
    )
    .expect("write dep.mjs");
    let mod_base =
        zuqe::path_to_url(&mod_dir.join("mod.mjs").to_string_lossy()).expect("mod.mjs url");

    failed += !check_module_roundtrip_js(
        "module stateful closure",
        "export let count = 0;\nexport function inc(){ return ++count }\nglobalThis.__ns = { inc, count };\n",
        mod_base.clone(),
        "String(__v.inc()) + ',' + String(__v.count)",
        "1,0",
    )
    .await
        as i32;

    failed += !check_module_roundtrip_js(
        "module import + re-export",
        "import { add, BASE } from './dep.mjs';\n\
         export const sum = add(BASE, 5);\n\
         export function f(x){ return add(x, sum) }\n\
         globalThis.__ns = { f, sum };\n",
        mod_base.clone(),
        "String(__v.f(1)) + ',' + String(__v.sum)",
        "16,15",
    )
    .await as i32;
    failed += !check_module_roundtrip_js(
        "module re-export",
        "import { add, BASE } from './dep.mjs';\n\
         export { add as plus, BASE as base } from './dep.mjs';\n\
         globalThis.__ns = { f: add, base: BASE };\n",
        mod_base.clone(),
        "String(__v.f(2,3)) + ',' + String(__v.base)",
        "5,10",
    )
    .await as i32;
    let _ = std::fs::remove_dir_all(&mod_dir);

    // 閳ユ柡鈧柡鈧柡鈧柡鈧?native function persistence 閳ユ柡鈧柡鈧柡鈧柡鈧?    failed += !check_native_direct("native function value round-trip", 42).await as i32;
    failed += !check_native_missing("native function missing on target").await as i32;
    failed += !check_native_capture("closure capturing native", 42).await as i32;
    failed +=
        !check_native_capture_missing("closure capturing native missing on target").await as i32;

    if failed > 0 {
        println!("\n{failed} new edge-case checks FAILED");
        return ExitCode::FAILURE;
    }

    println!("\nall persistent round-trip checks passed");
    ExitCode::SUCCESS
}
