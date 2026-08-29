// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

mod build_atoms;
mod build_opcodes;
mod build_stdclass;
mod build_tokens;

use std::{env, path::PathBuf};

use build_atoms::gen_atoms;
use build_opcodes::gen_opcodes;
use build_stdclass::{gen_payload_accessors, gen_object_casts, gen_std_class_defs};
use build_tokens::gen_tokens;

/// TEMP MITIGATION GUARD (remove when the LLVM issue is resolved — full
/// diagnosis in memo/lto-ub-handover.md §17-§18).
///
/// `release` builds (lto="fat" + panic="abort" + opt-level>=2) hit an LLVM
/// MemCpyOpt/MemDep miscompile at the LTO backend: the promise reaction-job
/// tracer capture memcpys get corrupted, a GC root is lost, the resolve
/// function node is swept while its job is still queued, and the job then
/// calls the dead node -> SEGV (test262 Promise tests; memo §12.2/§13.1).
///
/// `-Cllvm-args=-memdep-block-scan-limit=1` caps MemoryDependenceAnalysis's
/// backward scan and inert-izes the wrong transform. Empirically (memo
/// §18.1): limits 1/4/8 are clean (window 0/16 vs baseline 2/8), limit 16
/// crashes again; JumpThreading and vectorization were excluded as carriers.
/// Verified end-to-end: full test262 suite, 47940 tests, 0 failures.
///
/// cargo cannot inject `-Cllvm-args` from build.rs or `[profile]`, so the
/// flag must arrive via RUSTFLAGS; this guard refuses to build an
/// unprotected release so a broken binary can never be produced silently.
/// Resolve a `[profile.<name>]` setting with cargo's precedence:
/// `CARGO_PROFILE_<NAME>_<KEY>` env override first, then Cargo.toml
/// (following one `inherits` level, whose env override also applies).
/// Build scripts cannot see the effective panic/lto directly, so the guard
/// reconstructs it. Limitation: `PROFILE` reports the base kind ("release"
/// even for custom profiles inheriting it), so a custom profile that changes
/// panic/lto cannot be distinguished here.
fn profile_setting(profile: &str, key: &str) -> Option<String> {
    let env_key = format!(
        "CARGO_PROFILE_{}_{}",
        profile.to_uppercase().replace(['-', '.'], "_"),
        key.to_uppercase()
    );
    if let Ok(v) = env::var(&env_key) {
        return Some(v);
    }

    let text = std::fs::read_to_string("Cargo.toml").ok()?;
    let table = format!("profile.{profile}");
    if let Some(v) = cargo_toml_table_key(&text, &table, key) {
        return Some(v);
    }
    let inherits = cargo_toml_table_key(&text, &table, "inherits")?;
    let env_key = format!(
        "CARGO_PROFILE_{}_{}",
        inherits.to_uppercase().replace(['-', '.'], "_"),
        key.to_uppercase()
    );
    if let Ok(v) = env::var(&env_key) {
        return Some(v);
    }
    cargo_toml_table_key(&text, &format!("profile.{inherits}"), key)
}

/// Naive `[section]` / `key = value` scan of Cargo.toml. Good enough for the
/// hand-maintained profile tables in this repo.
fn cargo_toml_table_key(text: &str, table: &str, key: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l.trim_start_matches('[').trim_end_matches(']').trim() == table;
            continue;
        }
        if in_section
            && let Some((k, v)) = l.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn guard_memdep_mitigation() {
    if env::var("ZUQE_SKIP_MEMDEP_GUARD").as_deref() == Ok("1") {
        return;
    }

    let profile = env::var("PROFILE").unwrap_or_default();
    if profile == "debug" {
        return; // dev: no LTO, unwind
    }
    let opt = env::var("OPT_LEVEL").unwrap_or_default();
    if !matches!(opt.as_str(), "2" | "3" | "s" | "z") {
        return; // the O2+ pipeline passes are required for the miscompile
    }

    // Risky triple: lto="fat" + panic="abort" + opt-level >= 2.
    let lto = profile_setting(&profile, "lto").unwrap_or_default();
    let panic_mode = profile_setting(&profile, "panic").unwrap_or_else(|| "unwind".into());
    if !(lto == "fat" || lto == "true") || panic_mode != "abort" {
        return;
    }

    let mut flags = env::var("RUSTFLAGS").unwrap_or_default();
    flags.push('\u{1}');
    flags.push_str(&env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default());
    if flags.contains("memdep-block-scan-limit") {
        return;
    }

    panic!(
        "\nzuqe release LTO build is missing the MemCpyOpt mitigation flag.\n\
         Build with:\n\n\
         RUSTFLAGS=\"-Cllvm-args=-memdep-block-scan-limit=1\" cargo build --release\n\n\
         Why: fat-LTO + panic=abort + O2+ miscompiles the promise reaction-job\n\
         tracer captures (GC root loss -> SEGV); LLVM MemCpyOpt at the LTO\n\
         backend is the identified carrier, and a MemoryDependenceAnalysis\n\
         scan limit <= 8 avoids it (verified: full test262, 47940 tests,\n\
         0 failures). Details: memo/lto-ub-handover.md §17-§18.\n\
         Intentionally different flags (LTO off, O1 diagnostics, etc.)?\n\
         Set ZUQE_SKIP_MEMDEP_GUARD=1 to bypass this check.\n"
    );
}

fn main() {
    guard_memdep_mitigation();

    // Regenerate whenever the build-script sources change.
    println!("cargo:rerun-if-changed=build_atoms.rs");
    println!("cargo:rerun-if-changed=build_opcodes.rs");
    println!("cargo:rerun-if-changed=build_stdclass.rs");
    println!("cargo:rerun-if-changed=build_tokens.rs");

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    gen_atoms(&out_path.join("atoms.rs"));
    gen_opcodes(&out_path.join("opcodes.rs"));
    gen_tokens(&out_path.join("tokens.rs"));
    gen_std_class_defs(&out_path.join("std_class_def.rs"));
    gen_payload_accessors(&out_path.join("payload_accessors.rs"));
    gen_object_casts(&out_path.join("object_casts.rs"));
}
