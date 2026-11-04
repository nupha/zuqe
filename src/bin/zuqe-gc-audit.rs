// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! `zuqe-gc-audit` — scan a crate's source tree for GC-leaf payload fields
//! (type name, `file:line`, per-field wrap/container status) and audit it
//! against the L2b rules: bare fields, container elements without `GcBind`,
//! whole-field guard replacement, stale allowlist entries.
//!
//! Build the distributable binary with `cargo build --release --bin
//! zuqe-gc-audit` (or run it in-tree with `cargo run --bin zuqe-gc-audit`).

use std::path::PathBuf;

const USAGE: &str = "\
zuqe-gc-audit — scan a source tree for GC-leaf payload fields and audit them.

usage: zuqe-gc-audit [--allowlist FILE] [SRC_DIR]

  SRC_DIR         crate source directory to scan (default: ./src)
  --allowlist F   file with one `Type.field — reason` exemption per line
                  (# comments and blank lines are skipped)
  -h, --help      this text

exit status: 0 = no violations, 1 = violations found, 2 = usage error.
build: `cargo build --release --bin zuqe-gc-audit`, or in-tree
       `cargo run --bin zuqe-gc-audit -- [SRC_DIR]`.";

fn main() {
    let mut args = std::env::args().skip(1);
    let mut allow_path: Option<PathBuf> = None;
    let mut src: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "--allowlist" => match args.next() {
                Some(f) => allow_path = Some(f.into()),
                None => fail("--allowlist needs a file path"),
            },
            a if a.starts_with('-') => fail(&format!("unknown flag: {a}")),
            a => {
                if src.is_some() {
                    fail("at most one SRC_DIR");
                }
                src = Some(a.into());
            }
        }
    }
    let src = src.unwrap_or_else(|| PathBuf::from("src"));
    if !src.is_dir() {
        fail(&format!("not a directory: {}", src.display()));
    }

    let allow: Vec<String> = match &allow_path {
        Some(p) => {
            let text = std::fs::read_to_string(p)
                .unwrap_or_else(|e| fail(&format!("cannot read allowlist {}: {e}", p.display())));
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect()
        }
        None => Vec::new(),
    };
    let allow_refs: Vec<&str> = allow.iter().map(String::as_str).collect();

    // Inventory: every struct/enum directly declaring a GC-leaf field.
    let types = zuqe::gc_audit::gc_leaf_types(&src);
    println!(
        "=== GC-leaf types: {} (source: {}) ===",
        types.len(),
        src.display()
    );
    for t in &types {
        println!("{} ({}:{})", t.name, t.file, t.line);
        for f in &t.fields {
            let tag = if f.wrapped {
                "wrapped"
            } else if f.container {
                "container"
            } else {
                "bare"
            };
            println!("  {}: {} [{tag}]", f.name, f.ty);
        }
    }

    // Audit: what still needs a wrap, a GcBind, or an allowlist entry.
    let roots = zuqe::gc_audit::payload_root_types(&src);
    let allow_desc = allow_path
        .as_ref()
        .map(|p| format!("{} ({} entries)", p.display(), allow.len()))
        .unwrap_or_else(|| "empty".to_string());
    let violations = zuqe::gc_audit::all_violations(&src, &allow_refs);
    println!(
        "\n=== Audit — allowlist: {allow_desc}; root set: {} payload types ===",
        roots.len()
    );
    if violations.is_empty() {
        println!("clean: no violations");
        return;
    }
    println!("{} violation(s):", violations.len());
    for line in &violations {
        println!("{line}");
    }
    std::process::exit(1);
}

fn fail(msg: &str) -> ! {
    eprintln!("zuqe-gc-audit: {msg}");
    eprintln!("try `zuqe-gc-audit --help`");
    std::process::exit(2);
}
