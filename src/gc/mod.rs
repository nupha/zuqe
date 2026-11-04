// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

// Static field audit (L2b): scans source trees at test/tool time; it is never
// called at runtime.
pub mod audit;
pub(crate) mod gc;
pub(crate) mod guard;

pub(crate) use gc::GcHeapExt;
pub use guard::{GcBind, WriteBarrier, WriteBarrierVec};

/// The standard GC payload field audit test for any crate that depends on
/// zuqe. Expands to `gc_audit_payload_fields`: rules 1–3 plus a root-set floor
/// (`min_roots`) over the *caller's* `src` tree, so a crate's
/// `tests/gc_audit.rs` carries nothing but its own allowlist.
///
/// ```ignore
/// // no exemptions yet (red until every line is fixed or allowlisted):
/// zuqe::gc_audit_test!(30);
///
/// // with exemptions — `Type.field — reason` entries:
/// const EXEMPT: &[&str] = &["MyPayload.cache — construction only"];
/// zuqe::gc_audit_test!(EXEMPT, 30);
/// ```
///
/// See `docs/gc.md`, section "Auditing GC payload fields from your own crate".
#[macro_export]
macro_rules! gc_audit_test {
    ($allowlist:expr, $min_roots:expr) => {
        #[test]
        fn gc_audit_payload_fields() {
            let src = ::std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
            let v = $crate::gc_audit::test_failures(&src, $allowlist, $min_roots);
            ::std::assert!(v.is_empty(), "GC payload audit failed:\n{}", v.join("\n"));
        }
    };
    ($min_roots:expr) => {
        $crate::gc_audit_test!(&[], $min_roots);
    };
}
