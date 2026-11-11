// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

// Static field audit (L2b): scans source trees at test/tool time; it is never
// called at runtime.
pub mod audit;
pub(crate) mod gc;
pub(crate) mod guard;

pub(crate) use gc::GcHeapExt;
pub use guard::{GcBind, WriteBarrier, WriteBarrierMap, WriteBarrierVec};

/// The standard GC payload field audit test for any crate that depends on
/// zuqe. Expands to `gc_audit_payload_fields`: rules 1–3 plus a root-set floor
/// (`min_roots`) over the *caller's* `src` tree, so a crate's
/// `tests/gc_audit.rs` carries nothing but its own allowlist.
///
/// The one-argument form reads `gc_audit_exemption.txt` next to `Cargo.toml` —
/// the standard exemption file, the single source of truth shared with
/// `zuqe-gc-audit`; a missing file means "no exemptions". The two-argument
/// form takes an inline slice for one-off use.
///
/// ```ignore
/// // standard: exemptions live in gc_audit_exemption.txt (`Type.field — reason`)
/// zuqe::gc_audit_test!(30);
///
/// // escape hatch: inline slice, no file
/// zuqe::gc_audit_test!(&["MyPayload.cache — construction only"], 30);
/// ```
///
/// See `skills/references/gc-write-barrier.md`, section "Static audit".
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
        #[test]
        fn gc_audit_payload_fields() {
            let manifest = ::std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            // The standard exemption file is the single source of truth; a
            // missing file means "no exemptions" (the test runs red until the
            // fields are fixed or the file is created).
            let exemptions =
                $crate::gc_audit::read_exemptions(&manifest.join($crate::gc_audit::EXEMPTION_FILE));
            let allow: ::std::vec::Vec<&str> = exemptions
                .iter()
                .map(::std::string::String::as_str)
                .collect();
            let v = $crate::gc_audit::test_failures(&manifest.join("src"), &allow, $min_roots);
            ::std::assert!(v.is_empty(), "GC payload audit failed:\n{}", v.join("\n"));
        }
    };
}
