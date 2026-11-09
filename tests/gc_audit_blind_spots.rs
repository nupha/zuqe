// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Runnable examples for the six static-audit blind spots closed by task #38.
//!
//! Each fixture is a tiny source tree written to a temp dir; the audit is run
//! through the public API only, exactly like `zuqe-gc-audit` / `gc_audit_test!`
//! do. Run with:
//!
//!   cargo test -p zuqe --test gc_audit_blind_spots -- --nocapture
//!
//! The printed lines are the real diagnostics a user sees (not assertions on
//! private helpers).

use std::path::{Path, PathBuf};

/// A throwaway crate root; the audit scans `<root>/src`.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(name: &str, source: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "zuqe-gc-audit-example-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("create temp src");
        std::fs::write(src.join("lib.rs"), source).expect("write fixture");
        Self { dir }
    }

    fn src(&self) -> PathBuf {
        self.dir.join("src")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

fn print_report(title: &str, dir: &Path) {
    println!("\n=== {title} ===");
    for t in zuqe::gc_audit::gc_leaf_types(dir) {
        for f in &t.fields {
            println!("  field: {}: {}", f.key(&t.name), f.ty);
        }
    }
    for line in zuqe::gc_audit::gc_field_violations(dir, &[]) {
        println!("  violation: {line}");
    }
    for line in zuqe::gc_audit::guard_construction_violations(dir) {
        println!("  guard: {line}");
    }
    for line in zuqe::gc_audit::payload_root_misses(dir) {
        println!("  root-miss: {line}");
    }
    for line in zuqe::gc_audit::unknown_field_type_warnings(dir) {
        println!("  warning: {line}");
    }
}

/// P2-5: `Default.pending_read` and `Byte.pending_read` no longer share a key.
#[test]
fn example_variant_keys_and_scoped_exemption() {
    let fixture = Fixture::new(
        "enum-keys",
        r#"
pub enum Kind {
    Default {
        pending_read: JSValue,
        controller: Option<JSValue>,
    },
    Byte {
        pending_read: JSValue,
    },
}

pub struct Payload {
    pub kind: Kind,
}

fn _root() {
    let _ = PayloadPtr::new(0, Payload::new());
}
"#,
    );
    let src = fixture.src();
    print_report("enum variant keys", &src);

    let verdicts = zuqe::gc_audit::field_verdicts(&src, &[]);
    for key in [
        "Kind::Default.pending_read",
        "Kind::Default.controller",
        "Kind::Byte.pending_read",
    ] {
        assert!(
            matches!(
                verdicts.get(key),
                Some(zuqe::gc_audit::FieldVerdict::Violation(_))
            ),
            "{key} must fail, got {:?}",
            verdicts.get(key)
        );
    }
    assert!(
        verdicts.get("Kind.pending_read").is_none(),
        "the old variant-blind key must not exist"
    );

    // Exempt only Byte; Default must still fail.
    let verdicts = zuqe::gc_audit::field_verdicts(&src, &["Kind::Byte.pending_read — reviewed"]);
    assert_eq!(
        verdicts.get("Kind::Byte.pending_read"),
        Some(&zuqe::gc_audit::FieldVerdict::Allowlisted)
    );
    assert!(matches!(
        verdicts.get("Kind::Default.pending_read"),
        Some(zuqe::gc_audit::FieldVerdict::Violation(_))
    ));
}

/// P1-4 + P3-7: a nested/indented payload becomes an explicit failure.
#[test]
fn example_nested_payload_root_miss() {
    let fixture = Fixture::new(
        "root-miss",
        r#"
mod outer {
    pub(crate) struct NestedPayload {
        pub(crate) v: JSValue,
    }
}

fn _root() {
    let _ = PayloadPtr::new(0, outer::NestedPayload::new());
}
"#,
    );
    let src = fixture.src();
    print_report("nested payload", &src);

    let misses = zuqe::gc_audit::payload_root_misses(&src);
    assert!(
        misses
            .iter()
            .any(|m| m.contains("payload type not parsed: NestedPayload")),
        "{misses:?}"
    );
    assert!(
        zuqe::gc_audit::test_failures(&src, &[], 0)
            .iter()
            .any(|l| l.contains("NestedPayload")),
        "root misses must fail the audit"
    );
}

/// P2-6: an unresolved CamelCase field type is a non-failing warning.
#[test]
fn example_unknown_field_type_warning() {
    let fixture = Fixture::new(
        "unknown-type",
        r#"
pub struct FooPayload {
    pub handle: ForeignHandle,
}

fn _root() {
    let _ = PayloadPtr::new(0, FooPayload::new());
}
"#,
    );
    let src = fixture.src();
    print_report("unknown field type", &src);

    let warnings = zuqe::gc_audit::unknown_field_type_warnings(&src);
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("unknown type `ForeignHandle`")),
        "{warnings:?}"
    );
    assert!(
        zuqe::gc_audit::all_violations(&src, &[]).is_empty(),
        "unknown types are warnings, not failures"
    );
}

/// P1-3: `*controller = WriteBarrier::new(..)` is a whole-field replacement.
#[test]
fn example_whole_field_replacement_through_deref() {
    let fixture = Fixture::new(
        "guard-deref",
        r#"
fn f() {
    *controller = WriteBarrier::new(Some(c));
    self.slot = WriteBarrier::new(v);
    let ok = WriteBarrier::new(v);
    mem::replace(&mut p.slot, WriteBarrier::new(v));
}
"#,
    );
    let src = fixture.src();
    print_report("whole-field replacement", &src);

    let found = zuqe::gc_audit::guard_construction_violations(&src);
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found.iter().any(|l| l.contains("*controller =")));
    assert!(!found.iter().any(|l| l.contains("let ok")));
}
