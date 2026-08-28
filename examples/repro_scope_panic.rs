use zuqe::{Callback, JSResult, JSValue, Runtime, ScopedContext};

fn main() {
    let mut rt = Runtime::create().unwrap();
    rt.install_panic_handler();

    let ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .unwrap();

    let mut scope = ctx.root_scope();
    let mut global = JSValue::from(scope.global_obj());

    // $262 object with createRealm
    let mut obj262 = scope.create_base_object().unwrap();
    let create_realm = scope
        .create_native_function(
            Callback::Func(|ctx, _| -> JSResult {
                let ctx2 = ctx
                    .rt_mut()
                    .create_context(ScopedContext::ECMA_CONTEXT_SETUP)?;
                Ok(JSValue::from(ctx2.global_obj()))
            }),
            "createRealm",
            1,
            0,
        )
        .unwrap();
    obj262
        .set_property_str(&mut scope, "createRealm", create_realm.into())
        .unwrap();
    global
        .set_property_str(&mut scope, "$262", obj262.into())
        .unwrap();

    // Minimal Test262Error
    let err_cls = scope
        .create_native_function(
            Callback::Func(|_ctx, _| -> JSResult { Ok(JSValue::UNDEFINED) }),
            "Test262Error",
            1,
            0,
        )
        .unwrap();
    global
        .set_property_str(&mut scope, "Test262Error", err_cls.into())
        .unwrap();

    // The core reproduction: create a realm, use its constructor to create an
    // object in that realm's scope stack, then throw it in the original realm.
    //
    // Before the fix this panics inside `throw()` because the LOCAL GC node
    // was allocated on ctx2's scope stack but `throw()` only walks the caller's
    // (ctx1) scope chain — the node is never found.
    //
    // After the fix the object is thrown as a normal JS exception; the try/catch
    // handles it. If any Rust-level panic occurs, the process aborts.
    let js = r#"
var realmGlobal = $262.createRealm().global;
try {
  throw new realmGlobal.TypeError();
} catch (e) {
  // cross-realm TypeError caught — this is the expected path
}
"#;

    scope
        .eval(
            js.as_bytes(),
            &zuqe::EvalName::Name("repro.js".into()),
            zuqe::EvalType::Global,
            zuqe::EvalFlags::empty(),
        )
        .unwrap();

    eprintln!("OK: no panic");
}
