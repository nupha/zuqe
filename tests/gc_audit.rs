// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! zuqe's own run of the standard GC payload field audit (L2b of
//! `memo/gc_bind_guard.md`). The checks live in `src/gc/audit.rs` and reach
//! this file through `zuqe::gc_audit_test!` — the same one-liner any
//! embedding crate writes. Exemptions live only in `gc_audit_exemption.txt`
//! at the crate root (the standard's single source of truth, shared with
//! `zuqe-gc-audit`); see `skills/gc-write-barrier.md`, "静态审计".
//! This test also pins zuqe's payload set.

use std::path::{Path, PathBuf};

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

// The standard test: reads `gc_audit_exemption.txt`, runs rules 1–3 plus the
// root-set floor (zuqe has 47 payloads).
zuqe::gc_audit_test!(40);

/// The audit must be wired to zuqe's real payload set — it cannot pass by
/// scanning nothing. The floor itself is enforced above; these three assert
/// that each root-set detection path still works.
#[test]
fn the_audit_sees_the_payload_set() {
    let roots = zuqe::gc_audit::payload_root_types(&src_dir());
    for pilot in ["PromisePayload", "AGenPayload", "FinalizationRegistryData"] {
        assert!(
            roots.iter().any(|r| r == pilot),
            "root set lost the pilot payload {pilot}: {roots:?}"
        );
    }
}
