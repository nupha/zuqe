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

/// Escape `<`, `>`, `&` so error messages with these characters render safely
/// inside the Markdown/HTML report instead of being parsed as tags.
fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
    }
    out
}

/// Build a stable HTML anchor id for a feature name, used to link the feature
/// table to the matching group in the Failed scripts section.
fn anchor_for(name: &str) -> String {
    if name == NO_FEATURE_KEY {
        return "baseline-no-features".to_string();
    }
    let mut s = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            s.push(c);
        } else {
            s.push('-');
        }
    }
    format!("feat-{s}")
}

/// Build the run report as a self-contained HTML file (`test262-report.html`)
/// in the current directory, with custom CSS for a clean, lively look.
///
/// * `preset_name` — preset label (e.g. `ES2023`, or `ERROR UPDATE` for an
///   error-file re-run).
/// * `feature_list` — the enabled features (used to compute the "untested
///   features" note). `None` (error-file mode) skips that note.
/// * `test_filter` — when `Some`, an explicit `tests:` whitelist was applied;
///   it is shown in the Summary. `None` means "run everything" (no filter line).
/// * `feat_stats` — `feature -> (total, pass)` for exercised features; only
///   these appear in the feature table.
pub fn write_report(
    preset_name: &str,
    feature_list: Option<&[String]>,
    test_filter: Option<&str>,
    total: usize,
    failed_count: usize,
    feat_stats: &HashMap<String, (usize, usize)>,
    failed_scripts: &[(String, String, Vec<String>)],
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
    let (baseline_total, baseline_pass) = feat_stats.get(NO_FEATURE_KEY).copied().unwrap_or((0, 0));
    let baseline_rate = if baseline_total > 0 {
        (baseline_pass as f64 / baseline_total as f64) * 100.0
    } else {
        0.0
    };

    // Total number of distinct *real* features exercised in this run. The
    // synthetic `NO_FEATURE_KEY` bucket is not a feature, so it is excluded.
    let features_count = feat_stats.keys().filter(|k| *k != NO_FEATURE_KEY).count();

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
            if let Some(st) = feat_stats.get(NO_FEATURE_KEY)
                && st.0 > 0
            {
                v.push((NO_FEATURE_KEY.to_string(), *st));
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

    // Group failed scripts by each feature they declare (a script with several
    // `@features` appears under each), or under the baseline bucket when it has
    // no `@features`. Used both for the table anchors and the grouped listing.
    let mut fail_by_feature: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (s, msg, feats) in failed_scripts {
        if feats.is_empty() {
            fail_by_feature
                .entry(NO_FEATURE_KEY.to_string())
                .or_default()
                .push((s.clone(), msg.clone()));
        } else {
            for f in feats {
                fail_by_feature
                    .entry(f.clone())
                    .or_default()
                    .push((s.clone(), msg.clone()));
            }
        }
    }

    let mut html = String::new();
    html.push_str(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>Test262 Report</title>\n<style>\n",
    );
    html.push_str(REPORT_CSS);
    html.push_str("</style>\n</head>\n<body>\n<div class=\"container\">\n");

    // Header with timestamp: title left, timestamp right.
    html.push_str(&format!(
        "<h1><span class=\"ttl\">Test262 Report</span><span class=\"ts\">{}</span></h1>\n",
        escape_html(&now_timestamp())
    ));

    // Preset label, large, shown above the summary blocks.
    html.push_str(&format!(
        "<div class=\"preset\">{}</div>\n",
        escape_html(preset_name)
    ));

    // Prominent colored summary blocks (spaced apart).
    html.push_str("<div class=\"blocks\">\n");
    html.push_str(&block_html("TOTAL", &total.to_string(), "#1565C0"));
    html.push_str(&block_html("PASS", &success_count.to_string(), "#2E7D32"));
    html.push_str(&block_html("FAIL", &failed_count.to_string(), "#C62828"));
    html.push_str(&block_html("RATE", &format!("{rate:.2}%"), "#EF6C00"));
    html.push_str("</div>\n");

    // Summary.
    html.push_str("<section class=\"summary\">\n<h2>Summary</h2>\n<ul>\n");
    html.push_str(&format!(
        "<li><span class=\"k\">Overall pass rate</span><span class=\"v\">{rate:.2}% ({success_count}/{total})</span></li>\n"
    ));
    if baseline_total > 0 {
        html.push_str(&format!(
            "<li><span class=\"k\">Baseline pass rate</span><span class=\"v\">{baseline_rate:.2}% ({baseline_pass}/{baseline_total})</span></li>\n"
        ));
    }
    html.push_str(&format!(
        "<li><span class=\"k\">Features count</span><span class=\"v\">{}</span></li>\n",
        features_count
    ));
    if let Some(tf) = test_filter {
        let tf_html = escape_html(tf).replace('\n', "<br>\n");
        html.push_str(&format!(
            "<li><span class=\"k\">Test filter</span><span class=\"v\">{}</span></li>\n",
            tf_html
        ));
    }
    html.push_str("</ul>\n</section>\n");

    // Feature table.
    html.push_str("<section>\n<h2>Features tested</h2>\n");
    html.push_str(
        "<table class=\"feat\">\n<thead><tr><th>Feature</th><th>Total</th>\
         <th>Pass</th><th>Fail</th></tr></thead>\n<tbody>\n",
    );
    if rows.is_empty() {
        html.push_str("<tr><td colspan=\"4\" class=\"muted\">none</td></tr>\n");
    } else {
        for (fname, (t, p)) in &rows {
            let label = if fname == NO_FEATURE_KEY {
                "(baseline — no @features)"
            } else {
                fname
            };
            let fail = t - p;
            // Features with failures get an anchor link to their group below.
            let name_cell = if fail > 0 {
                format!(
                    "<a href=\"#{}\">{}</a>",
                    anchor_for(fname),
                    escape_html(label)
                )
            } else {
                escape_html(label)
            };
            if fail > 0 {
                html.push_str(&format!(
                    "<tr><td>{name_cell}</td><td>{t}</td><td>{p}</td>\
                     <td class=\"fail\">{fail}</td></tr>\n"
                ));
            } else {
                html.push_str(&format!(
                    "<tr><td>{name_cell}</td><td>{t}</td><td>{p}</td>\
                     <td>{fail}</td></tr>\n"
                ));
            }
        }
    }
    html.push_str("</tbody>\n</table>\n</section>\n");

    // Failed scripts, grouped by feature.
    html.push_str(&format!(
        "<section>\n<h2>Failed scripts ({failed_count})</h2>\n"
    ));
    if failed_scripts.is_empty() {
        html.push_str("<p class=\"muted\">none</p>\n");
    } else {
        for (fname, (t, _p)) in &rows {
            let fail = t - _p;
            if fail == 0 {
                continue;
            }
            let label = if fname == NO_FEATURE_KEY {
                "(baseline — no @features)"
            } else {
                fname
            };
            let anchor = anchor_for(fname);
            html.push_str(&format!("<h3 id=\"{anchor}\">{label}</h3>\n"));
            html.push_str("<ul class=\"failed\">\n");
            if let Some(scripts) = fail_by_feature.get(fname) {
                for (s, msg) in scripts {
                    html.push_str(&format!(
                        "<li><span class=\"sf\">{}</span><span class=\"em\">— {}</span></li>\n",
                        escape_html(s),
                        escape_html(msg)
                    ));
                }
            }
            html.push_str("</ul>\n");
        }
    }
    html.push_str("</section>\n");

    // Features not tested (placed last).
    if let Some(list) = feature_list {
        let tested: HashSet<&str> = rows.iter().map(|(f, _)| f.as_str()).collect();
        let untested: Vec<&str> = list
            .iter()
            .map(|s| s.as_str())
            .filter(|f| !tested.contains(f))
            .collect();
        if !untested.is_empty() {
            let joined = untested
                .iter()
                .map(|f| escape_html(f))
                .collect::<Vec<_>>()
                .join(", ");
            html.push_str(&format!(
                "<section>\n<h2>Features not tested</h2>\n<p class=\"muted\">{joined}</p>\n</section>\n"
            ));
        }
    }

    html.push_str("</div>\n</body>\n</html>\n");

    std::fs::write("test262-report.html", html).unwrap();
    eprintln!("Report written to test262-report.html");
}

/// One colored summary block (TOTAL / PASS / FAIL / RATE).
fn block_html(title: &str, value: &str, color: &str) -> String {
    format!(
        "<div class=\"block\" style=\"background:{color}\">\
         <div class=\"bt\">{title}</div><div class=\"bv\">{value}</div></div>\n"
    )
}

/// Custom stylesheet for the HTML report: clean container, lively color blocks,
/// subtle shadows, anchored feature links.
const REPORT_CSS: &str = r#"
* { box-sizing: border-box; }
body {
    margin: 0;
    padding: 36px 16px;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif;
    background: linear-gradient(135deg, #eef2f7 0%, #f7f9fc 100%);
    color: #1f2933;
}
.container {
    max-width: 940px;
    margin: 0 auto;
    background: #ffffff;
    border-radius: 16px;
    box-shadow: 0 12px 34px rgba(15, 23, 42, 0.10);
    padding: 34px 40px;
}
h1 { font-size: 28px; margin: 0 0 12px; font-weight: 700; color: #102a43; display: flex; align-items: baseline; justify-content: space-between; gap: 16px; }
h1 .ttl { text-align: left; }
h1 .ts { font-size: 16px; font-weight: 400; color: #7b8794; text-align: right; white-space: nowrap; }
.preset {
    text-align: center;
    font-size: 36px;
    font-weight: 700;
    color: #102a43;
    margin: 26px 0 30px;
    letter-spacing: 2px;
}
.blocks { display: flex; gap: 14px; justify-content: center; flex-wrap: wrap; margin-bottom: 48px; }
.block {
    border-radius: 14px;
    padding: 18px 26px;
    color: #fff;
    text-align: center;
    min-width: 132px;
    box-shadow: 0 6px 16px rgba(0, 0, 0, 0.14);
}
.bt { font-size: 12px; letter-spacing: 2px; text-transform: uppercase; opacity: 0.92; }
.bv { font-size: 32px; font-weight: 800; margin-top: 6px; }
section { margin-top: 44px; }
h2 {
    font-size: 20px;
    padding-bottom: 8px;
    margin-bottom: 14px;
    color: #243b53;
}
h3 { font-size: 16px; color: #334e68; margin: 20px 0 8px; }
.summary ul { list-style: none; padding: 0; margin: 0; }
.summary li {
    display: flex;
    align-items: flex-start;
    padding: 7px 0;
    border-bottom: 1px dashed #edf0f3;
}
.summary .k { font-weight: 600; color: #486581; min-width: 220px; padding-top: 2px; }
.summary .v { color: #1f2933; flex: 1; line-height: 1.7; word-break: break-all; }
table.feat { width: 100%; border-collapse: collapse; }
table.feat th, table.feat td { padding: 9px 12px; text-align: left; border-bottom: 1px solid #eef1f4; }
table.feat th {
    background: #f0f4f8;
    color: #486581;
    font-size: 13px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
}
table.feat tr:hover { background: #f7fafc; }
td.fail { color: #c62828; font-weight: 700; }
.muted { color: #9aa5b1; }
ul.failed { margin: 6px 0; padding-left: 22px; }
ul.failed li { margin: 6px 0; }
.failed .sf {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 13px;
    color: #243b53;
}
.failed .em { color: #9aa5b1; font-size: 12px; margin-left: 8px; }
a { color: #2b6cb0; text-decoration: none; font-weight: 600; }
a:hover { text-decoration: underline; }
"#;
