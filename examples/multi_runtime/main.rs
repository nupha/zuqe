// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Demonstrates `join_async` and `join_next` on multiple Runtimes.
//!
//! ```sh
//! cargo run --example multi_runtime --features async_support
//! ```

use std::process::ExitCode;

use futures_util::StreamExt;
use zuqe::{
    Callback, EvalFlags, EvalName, EvalType, ExitStatus, JSResult, JSValue, Runtime, ScopedContext,
};

fn main() -> ExitCode {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async_main())
}

fn val_to_string(_ctx: &mut ScopedContext, v: JSValue) -> String {
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
    if v.as_symbol(false).is_some() {
        return "Symbol(...)".to_string();
    }
    "[object]".to_string()
}

fn add_print(ctx: &mut ScopedContext) {
    let print = ctx
        .create_native_function(
            Callback::Func(|ctx, cc| -> JSResult {
                let mut line = String::new();
                for i in 0..cc.length() {
                    if i > 0 {
                        line.push(' ');
                    }
                    line.push_str(&val_to_string(ctx, cc.arg(i)));
                }
                println!("{line}");
                Ok(JSValue::UNDEFINED)
            }),
            "print",
            1,
            0,
        )
        .unwrap();
    ctx.global_obj()
        .set_property_str(ctx, "print", print.into())
        .unwrap();
}

fn make_and_eval(script: &str, name: &str) -> (Runtime, zuqe::Context<'static>) {
    let mut rt = Runtime::create().unwrap();
    rt.install_panic_handler();
    let mut ctx = rt
        .create_context(|cx| {
            ScopedContext::ECMA_CONTEXT_SETUP(cx)?;
            add_print(cx);
            Ok(())
        })
        .unwrap();
    ctx.eval(
        script.as_bytes(),
        &EvalName::Name(format!("{name}.js")),
        EvalType::Global,
        EvalFlags::empty(),
    )
    .unwrap();
    (rt, ctx)
}

async fn async_main() -> ExitCode {
    // ── Part 1: join_async — drive all to idle ─────────────────────────

    println!("=== join_async ===\n");

    let scripts = [
        (
            "fast",
            r#"
                print("[rt1] immediate value 42");
                print("[rt1] finished!");
                42
            "#,
        ),
        (
            "medium",
            r#"
                print("[rt2] starting short chain...");
                Promise.resolve(1)
                    .then(v => { print("[rt2] step 1"); return v + 1; })
                    .then(v => { print("[rt2] step 2"); return v + 1; })
                    .then(v => { print("[rt2] done, result =", v); print("[rt2] finished!"); return v; })
            "#,
        ),
        (
            "slow",
            r#"
                print("[rt3] starting deep chain (20 levels)...");
                (function chain(n) {
                    if (n <= 0) {
                        print("[rt3] chain complete!");
                        print("[rt3] finished!");
                        return Promise.resolve("done");
                    }
                    return Promise.resolve(n).then(v => chain(v - 1));
                })(20)
            "#,
        ),
    ];

    // Create runtimes (they register in RUNTIMES automatically).
    let _runtimes: Vec<_> = scripts
        .iter()
        .map(|(name, src)| make_and_eval(src, name))
        .collect();

    // Drive all to idle.
    Runtime::join_async().await;
    println!("\n--- join_async: all idle ---\n");

    // Drop runtimes to remove them from RUNTIMES.
    drop(_runtimes);

    // ── Part 2: join_next — yield each as it completes ─────────────────

    println!("=== join_each ===\n");

    let scripts2 = [
        (
            "fast2",
            r#"
                print("[rt4] immediate");
                print("[rt4] finished!");
            "#,
        ),
        (
            "medium2",
            r#"
                print("[rt5] starting...");
                Promise.resolve(1)
                    .then(v => { print("[rt5] step 1"); return v + 1; })
                    .then(v => { print("[rt5] done"); print("[rt5] finished!"); return v; })
            "#,
        ),
        (
            "slow2",
            r#"
                print("[rt6] starting deep chain...");
                (function chain(n) {
                    if (n <= 0) {
                        print("[rt6] chain complete!");
                        print("[rt6] finished!");
                        return Promise.resolve("done");
                    }
                    return Promise.resolve(n).then(v => chain(v - 1));
                })(15)
            "#,
        ),
    ];

    let _runtimes2: Vec<_> = scripts2
        .iter()
        .map(|(name, src)| make_and_eval(src, name))
        .collect();

    let mut runs = Runtime::join_each();
    while let Some((rt, status)) = runs.next().await {
        let label = if Runtime::ptr_eq(&rt, &_runtimes2[0].0) {
            "fast2"
        } else if Runtime::ptr_eq(&rt, &_runtimes2[1].0) {
            "medium2"
        } else {
            "slow2"
        };
        match status {
            ExitStatus::Value(_) => println!("  >> [{label}] idle (OK)"),
            ExitStatus::Interrupted(code) => println!("  >> [{label}] interrupted: {code:?}"),
            ExitStatus::Exception(e) => println!("  >> [{label}] exception: {e:?}"),
        }
    }

    println!("\n--- all done ---");
    ExitCode::SUCCESS
}
