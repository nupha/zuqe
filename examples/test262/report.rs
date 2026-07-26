// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    time::SystemTime,
};

use zuqe::ExceptionText;

/// Synthetic feature key for tests that declare no `@features` frontmatter.
/// Such tests are attributed here so their (sometimes failing) results are
/// still visible in the feature table, instead of making the table look
/// all-green while the overall fail rate is non-zero.
pub const NO_FEATURE_KEY: &str = "(no @features)";

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

/// Format the current UTC time as `YYYY-MM-DD HH:MM:SS UTC`.
///
/// Rust's `std` has no function that renders a `SystemTime` as a calendar date,
/// so the conversion is done manually here (no `chrono`/`time` in the deps).
fn now_timestamp() -> String {
    let total = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    const DAY: i64 = 86_400;
    let mut days = total / DAY;
    let sec_of_day = total % DAY;

    let mut year = 1970;
    loop {
        let ydays = if is_leap(year) { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        year += 1;
    }

    let mlen = month_lengths(year);
    let mut month = 1;
    let mut d = days;
    while month <= 12 && d >= mlen[month - 1] {
        d -= mlen[month - 1];
        month += 1;
    }
    let day = d + 1;

    let hour = sec_of_day / 3600;
    let minute = (sec_of_day % 3600) / 60;
    let second = sec_of_day % 60;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        year, month, day, hour, minute, second
    )
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn month_lengths(y: i64) -> [i64; 12] {
    if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    }
}

/// Build the Markdown run report and write it to `test262-report.md` in the
/// current directory.
///
/// * `preset_name` — preset label (e.g. `ES2023`, or `ERROR UPDATE` for an
///   error-file re-run).
/// * `feature_list` — the enabled features (used to compute the "untested
///   features" note). `None` (error-file mode) skips that note.
/// * `feat_stats` — `feature -> (total, pass)` for exercised features; only
///   these appear in the feature table.
pub fn write_report(
    preset_name: &str,
    feature_list: Option<&[String]>,
    total: usize,
    failed_count: usize,
    feat_stats: &HashMap<String, (usize, usize)>,
    failed_scripts: &[String],
) {
    let success_count = total.saturating_sub(failed_count);
    let rate = if total > 0 {
        (success_count as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    // Stable (baseline) pass rate: tests that declare no `@features` frontmatter.
    // These are core-language tests always expected to pass; mainstream conformance
    // dashboards surface this number separately from the overall (all-features) rate.
    let (baseline_total, baseline_pass) = feat_stats
        .get(NO_FEATURE_KEY)
        .copied()
        .unwrap_or((0, 0));
    let baseline_rate = if baseline_total > 0 {
        (baseline_pass as f64 / baseline_total as f64) * 100.0
    } else {
        0.0
    };

    let mut md = String::new();

    // Title carries the timestamp.
    md.push_str(&format!("# Test262 Report at {}\n\n", now_timestamp()));

    // Preset label, large, shown above the summary blocks.
    md.push_str(&format!(
        "<div align=\"center\" style=\"font-size:30px;font-weight:bold;margin:16px 0;\">{preset_name}</div>\n\n"
    ));

    // Prominent centered colored summary blocks. `border-spacing` keeps the
    // cells apart so they read as distinct blocks.
    md.push_str("<div align=\"center\">\n\n");
    md.push_str(
        "<table align=\"center\" style=\"border-collapse:separate;border-spacing:12px 0;\">\n<tr>\n",
    );
    md.push_str(&format!(
        "<td align=\"center\" style=\"background:#1565C0;color:#FFFFFF;padding:14px 26px;border-radius:10px;\"><div style=\"font-size:12px;letter-spacing:2px;\">TOTAL</div><div style=\"font-size:30px;font-weight:bold;\">{total}</div></td>\n"
    ));
    md.push_str(&format!(
        "<td align=\"center\" style=\"background:#2E7D32;color:#FFFFFF;padding:14px 26px;border-radius:10px;\"><div style=\"font-size:12px;letter-spacing:2px;\">PASS</div><div style=\"font-size:30px;font-weight:bold;\">{success_count}</div></td>\n"
    ));
    md.push_str(&format!(
        "<td align=\"center\" style=\"background:#C62828;color:#FFFFFF;padding:14px 26px;border-radius:10px;\"><div style=\"font-size:12px;letter-spacing:2px;\">FAIL</div><div style=\"font-size:30px;font-weight:bold;\">{failed_count}</div></td>\n"
    ));
    md.push_str(&format!(
        "<td align=\"center\" style=\"background:#EF6C00;color:#FFFFFF;padding:14px 26px;border-radius:10px;\"><div style=\"font-size:12px;letter-spacing:2px;\">RATE</div><div style=\"font-size:30px;font-weight:bold;\">{rate:.2}%</div></td>\n"
    ));
    md.push_str("</tr>\n</table>\n\n</div>\n\n");

    // Summary: distinguish the overall rate (all tests) from the stable/baseline
    // rate (core-language tests with no `@features`), mirroring mainstream
    // conformance dashboards.
    md.push_str("## Summary\n\n");
    md.push_str(&format!("- **Preset**: {preset_name}\n"));
    md.push_str(&format!(
        "- **Overall pass rate**: {rate:.2}% ({success_count}/{total})\n"
    ));
    if baseline_total > 0 {
        md.push_str(&format!(
            "- **Stable (baseline) pass rate**: {baseline_rate:.2}% ({baseline_pass}/{baseline_total})\n"
        ));
    }

    // Feature table: only features that were actually exercised.
    md.push_str("## Features tested\n\n");
    md.push_str("| Feature | Total | Pass | Fail | Pass rate |\n");
    md.push_str("|---------|-------|------|------|-----------|\n");

    let mut rows: Vec<(String, (usize, usize))> = match feature_list {
        Some(list) => {
            let mut v: Vec<(String, (usize, usize))> = list
                .iter()
                .cloned()
                .filter_map(|f| {
                    feat_stats
                        .get(&f)
                        .copied()
                        .filter(|(t, _)| *t > 0)
                        .map(|st| (f, st))
                })
                .collect();
            // Always surface feature-less tests so their failures are visible,
            // even though they are not part of the enabled-feature list.
            if let Some(st) = feat_stats.get(NO_FEATURE_KEY) {
                if st.0 > 0 {
                    v.push((NO_FEATURE_KEY.to_string(), *st));
                }
            }
            v
        }
        None => feat_stats
            .iter()
            .filter(|(_, (t, _))| *t > 0)
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
    };
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    if rows.is_empty() {
        md.push_str("| (none) | | | | |\n");
    } else {
        for (fname, (t, p)) in &rows {
            let label = if fname == NO_FEATURE_KEY {
                "(baseline — no @features)"
            } else {
                fname
            };
            let fail = t - p;
            let fr = (*p as f64 / *t as f64) * 100.0;
            if fail > 0 {
                md.push_str(&format!(
                    "| {label} | {t} | {p} | <span style=\"color:#C62828;font-weight:bold;\">{fail}</span> | {fr:.2}% |\n"
                ));
            } else {
                md.push_str(&format!("| {label} | {t} | {p} | {fail} | {fr:.2}% |\n"));
            }
        }
    }

    // Note the enabled features that were not exercised (only when we know the
    // full enabled list).
    if let Some(list) = feature_list {
        let tested: HashSet<&str> = rows.iter().map(|(f, _)| f.as_str()).collect();
        let untested: Vec<&String> = list
            .iter()
            .filter(|f| !tested.contains(f.as_str()))
            .collect();
        if !untested.is_empty() {
            let joined = untested
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            md.push_str(&format!("\n## Features not tested\n\n{joined}\n"));
        }
    }

    md.push_str(&format!("\n## Failed scripts ({})\n\n", failed_count));
    if failed_scripts.is_empty() {
        md.push_str("(none)\n");
    } else {
        for s in failed_scripts {
            md.push_str(&format!("- {s}\n"));
        }
    }

    std::fs::write("test262-report.md", md).unwrap();
    eprintln!("Report written to test262-report.md");
}
