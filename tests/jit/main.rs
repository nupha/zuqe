// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! The JIT integration suites. Cargo only auto-discovers top-level
//! `tests/*.rs` (plus `tests/<dir>/main.rs`), so this file is the single entry
//! point for everything in this directory — no `[[test]]` wiring in
//! `Cargo.toml` to keep in sync:
//!
//! ```text
//! cargo test                                            # lib + every suite
//! cargo test --test jit                                 # every suite here
//! cargo test --test jit jit_differential                # one suite (name filter)
//! cargo test --features jit,dump_bytecode --test jit bytecode_dump_audit \
//!     -- --nocapture                                    # bytecode claim audit
//! ```
//!
//! Each suite keeps its own `#![cfg(feature = "jit")]`: with the feature off
//! they compile to nothing and only `sanitizer_probe` runs.

// The shared harness, declared once (rustc warns when the same file is loaded
// as a module more than once). Gated like the suites so an off build never
// compiles it.
#[cfg(feature = "jit")]
mod common;

mod jit_array;
mod jit_background;
mod jit_cost;
mod jit_differential;
mod jit_feedback;
mod jit_interrupt;
mod jit_opcodes;
mod jit_queue;
mod jit_semantics;
mod jit_verify;
mod sanitizer_probe;

/// Every suite file must have a `mod` line here, and every `mod` line must have
/// a file (or a directory module). A file that is never registered does not
/// fail to build — it just never runs, so this test is what makes the
/// hand-written wiring loud.
#[test]
fn every_suite_file_is_registered() {
    use std::path::Path;

    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/jit");
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .expect("read tests/jit")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "main.rs")
        .map(|name| name.trim_end_matches(".rs").to_string())
        .collect();
    files.sort();

    let main = std::fs::read_to_string(dir.join("main.rs")).expect("read tests/jit/main.rs");
    let mut declared: Vec<String> = main
        .lines()
        .filter_map(|line| line.trim().strip_prefix("mod "))
        .filter_map(|rest| rest.strip_suffix(';').map(str::to_owned))
        .collect();
    declared.sort();

    // `common` is a directory module (`common/mod.rs`), so it is declared
    // without a `common.rs` next to main.rs.
    let is_dir_module = |name: &str| dir.join(name).join("mod.rs").exists();

    let unregistered: Vec<&String> = files.iter().filter(|f| !declared.contains(f)).collect();
    assert!(
        unregistered.is_empty(),
        "these suite files have no `mod` line in tests/jit/main.rs: {unregistered:?}"
    );
    let stale: Vec<&String> = declared
        .iter()
        .filter(|d| !files.contains(d) && !is_dir_module(d))
        .collect();
    assert!(
        stale.is_empty(),
        "these modules are declared but have no tests/jit/<name>.rs: {stale:?}"
    );

    println!("registered suites: {declared:?}");
}
