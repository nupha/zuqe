// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use zuqe::ExceptionText;

pub fn dump_error(
    exc_text: &ExceptionText,
    strict: bool,
    is_module: bool,
    is_async: bool,
    outf: &mut dyn Write,
) {
    writeln!(
        outf,
        "[ENV] strict={strict}, module={is_module}, async={is_async}"
    )
    .unwrap();

    writeln!(outf, "{}", exc_text.message).unwrap();

    if let Some(tb) = exc_text.traceback.as_ref() {
        writeln!(outf, "{tb}").unwrap();
    }
}

/// Write one JSONL error record with an explicit message and stack string.
pub fn write_jsonl_raw(w: &mut File, script_rel: &str, scenario: &str, message: &str, stack: &str) {
    let entry = serde_json::json!({
        "script": script_rel,
        "error": message,
        "stack": stack,
        "scenario": scenario,
    });
    let _ = writeln!(w, "{}", entry);
}

/// Write one JSONL error record from an `ExceptionText` (message + raw stack).
pub fn write_error_jsonl(w: &mut File, script_rel: &str, scenario: &str, exc: &ExceptionText) {
    write_jsonl_raw(
        w,
        script_rel,
        scenario,
        &exc.message,
        exc.traceback.as_deref().unwrap_or(""),
    );
}

/// Given an error-file path `foo.jsonl`, return the backup path `foo-last.jsonl`.
pub fn make_last_jsonl_path(path: &str) -> PathBuf {
    let p = Path::new(path);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = p.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}-last.jsonl"))
}
