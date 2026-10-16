// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Does the engine start at all? `scripts/ci-sanitize.sh` runs this under
//! AddressSanitizer first: `Runtime::create_context` must not fail before any
//! JIT code is reached, otherwise the sanitizer leg measures the environment
//! rather than the JIT.
#[test]
fn context_creates_under_sanitizer() {
    use zuqe::{EvalFlags, EvalName, EvalType, Runtime, ScopedContext};
    let mut rt = Runtime::create().expect("runtime");
    let mut ctx = rt
        .create_context(ScopedContext::ECMA_CONTEXT_SETUP)
        .expect("context");
    let v = ctx
        .eval(
            b"1 + 1",
            &EvalName::Name("probe.js".into()),
            EvalType::Global,
            EvalFlags::empty(),
        )
        .expect("eval");
    assert_eq!(v.as_int(), Some(2));
}
