// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! `zuqe-gc-audit` — scan a crate's source tree for GC-leaf payload fields
//! (type name, `file:line`, per-field wrap/container status) and audit it
//! against the L2b rules: bare fields, container elements without `GcBind`,
//! whole-field guard replacement, stale allowlist entries.
//!
//! Build the distributable binary with `cargo build --release --bin
//! zuqe-gc-audit` (or run it in-tree with `cargo run --bin zuqe-gc-audit`).

use std::path::{Path, PathBuf};

const USAGE: &str = "\
zuqe-gc-audit — scan a source tree for GC-leaf payload fields and audit them.

usage: zuqe-gc-audit [--allowlist FILE] [SRC_DIR]

  SRC_DIR         crate source directory to scan (default: ./src)
  --allowlist F   exemption file (default: gc_audit_exemption.txt in the
                  nearest directory at or above SRC_DIR with a Cargo.toml)
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

    // The standard exemption file lives next to `Cargo.toml` (a sibling of
    // `src/`) — the same file the crate's `tests/gc_audit.rs` reads. An
    // explicit `--allowlist` overrides it.
    let allow_path = match allow_path {
        Some(p) => {
            if !p.is_file() {
                fail(&format!("cannot read allowlist {}", p.display()));
            }
            Some(p)
        }
        None => default_exemption_path(&src),
    };
    let allow: Vec<String> = allow_path
        .as_ref()
        .map(|p| zuqe::gc_audit::read_exemptions(p))
        .unwrap_or_default();
    let allow_refs: Vec<&str> = allow.iter().map(String::as_str).collect();

    // Inventory of the payload root set (what the audit checks), with each
    // field's verdict from the same walk: ✔️ pass, 🔵 allowlisted, ❌ violation.
    let all_types = zuqe::gc_audit::gc_leaf_types(&src);
    let verdicts = zuqe::gc_audit::field_verdicts(&src, &allow_refs);
    let mut rows = Vec::new();
    for t in &all_types {
        let fields: Vec<_> = t
            .fields
            .iter()
            .filter_map(|f| {
                verdicts
                    .get(&format!("{}.{}", t.name, f.name))
                    .map(|v| (f, *v))
            })
            .collect();
        if !fields.is_empty() {
            rows.push((t, fields));
        }
    }
    println!(
        "=== GC-leaf types: {} of {} (source: {}; payload root set) ===",
        rows.len(),
        all_types.len(),
        src.display()
    );
    println!("    ✔️ pass   🔵 allowlisted   ❌ violation");
    let (mut total, mut failed) = (0usize, 0usize);
    for (t, fields) in &rows {
        println!("{} ({}:{})", t.name, t.file, t.line);
        for (f, verdict) in fields {
            total += 1;
            let mark = match verdict {
                zuqe::gc_audit::FieldVerdict::Violation(_) => {
                    failed += 1;
                    "❌"
                }
                zuqe::gc_audit::FieldVerdict::Allowlisted => "🔵",
                zuqe::gc_audit::FieldVerdict::Pass => "✔️",
            };
            let tag = if f.wrapped {
                "wrapped"
            } else if f.container {
                "container"
            } else {
                "bare"
            };
            println!("  {mark} {}: {} [{tag}]", f.name, f.ty);
        }
    }

    // Audit: what still needs a wrap, a GcBind, or an allowlist entry.
    let roots = zuqe::gc_audit::payload_root_types(&src);
    let allow_desc = allow_path
        .as_ref()
        .map(|p| format!("{} ({} entries)", p.display(), allow.len()))
        .unwrap_or_else(|| "none (no gc_audit_exemption.txt)".to_string());
    let violations = zuqe::gc_audit::all_violations(&src, &allow_refs);
    println!(
        "\n=== Audit — allowlist: {allow_desc}; root set: {} payload types ===",
        roots.len()
    );
    if violations.is_empty() {
        println!("clean: no violations");
    } else {
        println!("{} violation(s):", violations.len());
        for line in &violations {
            println!("{line}");
        }
    }

    println!(
        "\n{} {} of {total} GC-leaf field(s) fail audit",
        if failed == 0 { "✔️" } else { "❌" },
        failed
    );
    if !violations.is_empty() {
        std::process::exit(1);
    }
}

/// The standard exemption file for a crate: `gc_audit_exemption.txt` in the
/// nearest directory at or above `src` that contains a `Cargo.toml`.
fn default_exemption_path(src: &Path) -> Option<PathBuf> {
    let mut dir = src.canonicalize().ok()?;
    loop {
        if dir.join("Cargo.toml").is_file() {
            let file = dir.join(zuqe::gc_audit::EXEMPTION_FILE);
            return file.is_file().then_some(file);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("zuqe-gc-audit: {msg}");
    eprintln!("try `zuqe-gc-audit --help`");
    std::process::exit(2);
}
