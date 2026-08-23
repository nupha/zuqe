// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_snake_case)]

mod agent;
mod presets;
mod report;
mod runner;

use std::{
    collections::{HashMap, HashSet},
    io::{Read, Write},
    path::PathBuf,
    sync::Mutex,
};

use clap::Parser;
use serde::Deserialize;

use crate::{
    presets::{
        Decisions, PresetDef, available_presets, build_feature_edition, enabled_features,
        is_known_preset, parse_preset_arg, resolve_decisions,
    },
    report::{NO_FEATURE_KEY, make_last_jsonl_path, write_report},
    runner::{TestStatus, run_test_file},
};

/// Relative path of the script currently being run. Set right before each
/// `run_test_file` call so the panic hook below can print it if the runner
/// crashes mid-run.
static CURRENT_SCRIPT: Mutex<Option<String>> = Mutex::new(None);

#[derive(Debug, Deserialize, Default)]
pub struct TestConfig {
    pub description: Option<String>,
    pub features: Option<Vec<String>>,
    pub negative: Option<Negative>,
    pub includes: Option<Vec<String>>,
    pub flags: Option<Vec<String>>,
    pub esid: Option<String>,
    pub info: Option<String>,
    pub locale: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct Negative {
    pub phase: Option<String>,
    pub type_: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct AppConfig {
    pub base_path: Option<String>,
    pub out_path: Option<String>,
    /// Test suite selector: `262` (ECMA-262 tests only; the default), `402`
    /// (intl402 only) or `all`. See `Suite`.
    pub suite: Option<String>,
    pub tests: Option<Vec<String>>,
    pub exclude_tests: Option<Vec<String>>,
    pub exclude_features: Option<Vec<String>>,
    /// Allowed ECMA-402 features for suite 402 (whitelist). Absent or empty =
    /// unrestricted; overridden by the `--intl` flag.
    pub intl: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    pub preset: Option<String>,
    /// Ordered edition presets; each entry inherits the features of every
    /// entry before it (see `PresetDef`).
    pub presets: Option<Vec<PresetDef>>,
    pub log: Option<String>,
}

/// Which specification's tests the default collection covers.
///
/// The suite only shapes the *default* file range; an explicit `tests:`
/// whitelist (config key or positional arguments) replaces it wholesale and
/// bypasses suite shaping. Feature gating (preset ladder + exclude_features)
/// is orthogonal and applies in every suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Suite {
    /// Everything, including `intl402`.
    All,
    /// ECMA-262 only: `intl402/**` and `staging/intl402/**` are excluded.
    Es262,
    /// ECMA-402 only: just `intl402/**` (staging/intl402 stays subject to the
    /// user's own `exclude_tests`).
    Es402,
}

impl Suite {
    /// Default glob list and extra exclude patterns (relative to `<base>/test`)
    /// used when no explicit whitelist is given.
    fn defaults(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            Suite::All => (&["**/*.js"], &[]),
            Suite::Es262 => (
                &["**/*.js"],
                &["intl402/**/*.js", "staging/intl402/**/*.js"],
            ),
            Suite::Es402 => (&["intl402/**/*.js"], &[]),
        }
    }

    /// Short label for reports.
    fn label(self) -> &'static str {
        match self {
            Suite::All => "all",
            Suite::Es262 => "262",
            Suite::Es402 => "402",
        }
    }
}

/// Parse a suite name as written in the config (`suite:`) or `--suite`.
fn parse_suite(s: &str) -> Option<Suite> {
    match s {
        "all" => Some(Suite::All),
        "262" => Some(Suite::Es262),
        "402" => Some(Suite::Es402),
        _ => None,
    }
}

#[derive(Debug, Parser)]
#[command(version, about = "Run test262 with edition presets")]
pub struct Args {
    /// Config file (default: test262.yml)
    #[arg(long = "config", value_name = "FILE", default_value_t = String::from("test262.yml"))]
    pub config_file: String,

    /// Edition preset to run; falls back to the config's `preset:`
    #[arg(long)]
    pub preset: Option<String>,

    /// Test suite: 262 (ECMA-262 only, default), 402 (intl402 only), all
    #[arg(long)]
    pub suite: Option<String>,

    /// Only run tests using these features (comma-separated); reports as USER
    #[arg(long = "features", value_name = "LIST", value_delimiter = ',')]
    pub features: Vec<String>,

    /// Skip tests requiring this feature (repeatable)
    #[arg(long = "exclude-feature", value_name = "FEATURE")]
    pub exclude_features: Vec<String>,

    /// Allowed ECMA-402 features for suite 402 (overrides config `intl:`)
    #[arg(long, value_name = "LIST", value_delimiter = ',')]
    pub intl: Vec<String>,

    /// Test files or globs to run; overrides the config's `tests:`
    #[arg(trailing_var_arg = true)]
    pub test_files: Vec<String>,

    /// Re-run only the failures listed in this JSONL file; rewrites it in place
    #[arg(long = "error-file", value_name = "FILE")]
    pub error_file: Option<String>,

    /// Record each script path before it runs; pinpoints the culprit on crash
    #[arg(long = "log-file", value_name = "FILE")]
    pub log_file: Option<String>,
}

/// Merge CLI flags onto the config loaded from the file. A CLI flag takes
/// precedence over the matching file setting; file settings stay untouched
/// when the corresponding flag is absent.
pub fn apply_cli_overrides(cfg: &mut AppConfig, args: &Args) {
    if let Some(preset) = args.preset.clone() {
        cfg.preset = Some(preset);
    }
    if let Some(suite) = args.suite.clone() {
        cfg.suite = Some(suite);
    }
    if !args.test_files.is_empty() {
        cfg.tests = Some(args.test_files.clone());
    }
    if !args.exclude_features.is_empty() {
        match cfg.exclude_features.as_mut() {
            Some(list) => list.extend(args.exclude_features.iter().cloned()),
            None => cfg.exclude_features = Some(args.exclude_features.clone()),
        }
    }
    if !args.intl.is_empty() {
        cfg.intl = Some(args.intl.clone());
    }
    if !args.features.is_empty() {
        match cfg.features.as_mut() {
            Some(list) => list.extend(args.features.iter().cloned()),
            None => cfg.features = Some(args.features.clone()),
        }
    }
}

/// Validate the (merged) config in one place, right before running. Any
/// missing or invalid setting surfaces here instead of mid-run.
/// `require_preset` is false in `--error-file` mode, where the preset no
/// longer selects which scripts run.
pub fn validate_config(cfg: &AppConfig, presets: &[PresetDef], require_preset: bool) {
    // A non-empty `features` whitelist (from the file or `--features`) selects
    // what runs on its own (USER preset mode), so `preset` becomes optional.
    let has_features = cfg.features.as_ref().is_some_and(|l| !l.is_empty());

    // `preset` is required unless a features whitelist is given: either from
    // the file or from `--preset`. Skipped when re-running from an errors
    // list, since preset-based filtering is off.
    if let Some(preset) = cfg.preset.as_deref() {
        if !has_features {
            // Resolving a preset name requires the config's `presets:` list.
            if presets.is_empty() {
                eprintln!(
                    "error: config file defines no 'presets:' list; cannot resolve preset \
                     '{preset}'. Add a 'presets:' section to the config file."
                );
                std::process::exit(1);
            }

            // A trailing `!` selects strict mode; strip it before matching the
            // preset name (the original argument is kept for error messages).
            let (preset_name, _) = parse_preset_arg(preset);
            if !is_known_preset(presets, preset_name) {
                eprintln!(
                    "error: unknown preset '{preset}'. available presets: {}",
                    available_presets(presets)
                );
                std::process::exit(1);
            }
        }
    } else if require_preset && !has_features {
        eprintln!(
            "error: no preset or features specified. Set 'preset:'/'features:' in the config file \
             or pass --preset <NAME> / --features <LIST>.\n\
             available presets: {}",
            available_presets(presets)
        );
        std::process::exit(1);
    }

    if let Some(suite) = cfg.suite.as_deref() {
        if parse_suite(suite).is_none() {
            eprintln!("error: unknown suite '{suite}'. valid suites: all, 262, 402");
            std::process::exit(1);
        }
    }

    // `base_path` / `out_path`, if set, must be existing directories.
    for (key, p) in [
        ("base_path", cfg.base_path.as_deref()),
        ("out_path", cfg.out_path.as_deref()),
    ] {
        if let Some(p) = p
            && !std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false)
        {
            eprintln!("error: invalid directory for '{key}': {p}");
            std::process::exit(1);
        }
    }
}

async fn main_impl() {
    let args = Args::parse();

    // Install a panic hook that prints the script currently being run. This is
    // the zero-cost crash reporter in `panic = "unwind"` builds (debug and
    // release-with-debug): on a panic it runs before the process aborts, so we
    // see the offending script without the per-iteration log writes that
    // `--log-file` does. In `panic = "abort"` builds (release) the hook never
    // runs, so `--log-file` is the fallback there.
    // Chain the previously-installed hook (the default, or the engine's
    // panic-time GC hook) so the usual panic location + backtrace still print.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        match CURRENT_SCRIPT.lock() {
            Ok(g) => match g.as_ref() {
                Some(s) => eprintln!("!!! test262 CRASH: current script = {s}"),
                None => eprintln!("!!! test262 CRASH: (no script recorded)"),
            },
            Err(_) => eprintln!("!!! test262 CRASH: (script lock poisoned)"),
        }
        prev_hook(info);
    }));

    // Config is given via `--config` (default: test262.yml). Positional
    // arguments are test scripts/globs to run (matching mainstream test262
    // runners such as test262-harness, QuickJS, and Boa).
    let config_file = &args.config_file;

    let mut app_cfg = {
        let mut fp = std::fs::OpenOptions::new()
            .read(true)
            .open(config_file)
            .unwrap();
        let mut txt = String::new();

        fp.read_to_string(&mut txt).unwrap();
        serde_yaml::from_str::<AppConfig>(txt.as_str()).unwrap()
    };

    // If `--error-file` is given, read the previous error log and back it up.
    // Only the listed scripts are re-run afterwards; the config's preset/tests
    // filtering is ignored in that mode.
    let (errjson_path, forced): (PathBuf, Option<Vec<String>>) =
        if let Some(path) = args.error_file.clone() {
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let mut seen = HashSet::new();
            let mut scripts = Vec::new();
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
                    && let Some(s) = v.get("script").and_then(|x| x.as_str())
                    && seen.insert(s.to_string())
                {
                    scripts.push(s.to_string());
                }
            }

            let backup = make_last_jsonl_path(&path);
            let _ = std::fs::copy(&path, &backup);
            log::info!(
                "error-file: re-running {} script(s); backup -> {}",
                scripts.len(),
                backup.display()
            );
            (PathBuf::from(path), Some(scripts))
        } else {
            (PathBuf::from("test262-errors.jsonl"), None)
        };

    // `--log-file`: open (truncate) a file that will record the relative path
    // of each script *before* it runs, so a mid-run crash leaves the crashing
    // script on the last line. Only meaningful in `panic = "abort"` builds,
    // where the panic hook above cannot run. In `panic = "unwind"` builds the
    // panic hook already prints the crashing script for free, so we skip the
    // per-iteration log writes (they are pure overhead there) and ignore
    // `--log-file`.
    let mut logf: Option<std::fs::File> = if let Some(p) = &args.log_file {
        // Open in all builds. In `panic = "abort"` builds the panic hook cannot
        // run, so this on-disk heartbeat is the only reliable crash reporter. In
        // `panic = "unwind"` builds the panic hook prints the crashing script for
        // free on a Rust panic, but a hard crash such as a SIGSEGV bypasses the
        // panic hook entirely, so the heartbeat log is still needed to recover the
        // offending script. The write only happens when `--log-file` is passed, so
        // there is no overhead in normal runs.
        match std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(p)
        {
            Ok(f) => Some(f),
            Err(e) => {
                eprintln!("error: cannot open --log-file {p}: {e}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // Merge CLI flags over the file config, then validate everything up front.
    // The ordered preset list comes from the config's `presets:` section.
    apply_cli_overrides(&mut app_cfg, &args);
    let preset_list: Vec<PresetDef> = app_cfg.presets.take().unwrap_or_default();
    validate_config(&app_cfg, &preset_list, forced.is_none());

    // Resolve the suite (CLI > config > default `262`). It shapes the default
    // collection below (an explicit `tests:` whitelist bypasses it) and, for
    // suite 402, pins the edition gate open.
    let suite = match app_cfg.suite.as_deref() {
        Some(s) => parse_suite(s).expect("suite validated"),
        None => Suite::Es262,
    };

    let mut dec = if forced.is_some() {
        // Re-run mode: run exactly the listed scripts, no preset/feature filtering.
        let (_, max_index) = build_feature_edition(&preset_list);
        Decisions {
            presets: preset_list,
            feature_edition: HashMap::new(),
            selected_index: max_index + 1,
            max_index,
            strict: false,
            exclude: HashSet::new(),
            intl_allowed: None,
            user_features: None,
        }
    } else {
        resolve_decisions(&app_cfg, preset_list)
    };

    // Suite 402: ECMA-402 features belong to no ECMA-262 edition, so the
    // edition gate is pinned fully open and the `intl:` whitelist governs
    // them instead; the preset ladder only governs the 262/all sweeps.
    if suite == Suite::Es402 {
        dec.selected_index = dec.max_index;
        dec.strict = false;
        dec.intl_allowed = app_cfg
            .intl
            .as_ref()
            .filter(|l| !l.is_empty())
            .map(|l| l.iter().cloned().collect());
    }

    // `base_path` was already validated as an existing directory in
    // `validate_config`; here we just resolve it (or fall back to CWD).
    let mut base_path: PathBuf = if let Some(txt) = app_cfg.base_path.take() {
        PathBuf::from(txt)
    } else {
        std::env::current_dir().unwrap()
    };
    if !base_path
        .to_str()
        .unwrap()
        .ends_with(std::path::MAIN_SEPARATOR)
    {
        let mut s = base_path.to_string_lossy().into_owned();
        s.push(std::path::MAIN_SEPARATOR);
        base_path = s.into()
    }

    let harness_path = base_path.join("harness");

    let mut errjson = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&errjson_path)
        .unwrap();

    let mut exclude_tests = HashSet::<PathBuf>::with_capacity(128);
    if let Some(skip_tests) = app_cfg.exclude_tests.take() {
        for item in skip_tests {
            let pat = base_path.join(format!("./test/{item}"));
            for fname in glob::glob(pat.to_str().unwrap()).unwrap() {
                exclude_tests.insert(fname.unwrap());
            }
        }
    }

    let (default_globs, suite_excludes) = suite.defaults();

    // An explicit whitelist exists when the config's `tests:` key or the CLI
    // positional arguments name any script; then the suite stays a no-op.
    let explicit_tests =
        !args.test_files.is_empty() || app_cfg.tests.as_ref().is_some_and(|t| !t.is_empty());
    if !explicit_tests {
        for item in suite_excludes.iter() {
            let pat = base_path.join(format!("./test/{item}"));
            for fname in glob::glob(pat.to_str().unwrap()).unwrap() {
                exclude_tests.insert(fname.unwrap());
            }
        }
    }

    // A non-default `tests` filter (from CLI args or the config's `tests:`
    // key) acts as a whitelist; the default `**/*.js` means "run everything"
    // and is not reported as a filter.
    let test_filter: Option<String> = if !args.test_files.is_empty() {
        Some(args.test_files.join("\n"))
    } else if let Some(tests) = &app_cfg.tests {
        if tests.is_empty() {
            None
        } else {
            Some(tests.join("\n"))
        }
    } else {
        None
    };

    let test_globs: Vec<String> = if !args.test_files.is_empty() {
        args.test_files.clone()
    } else if let Some(tests) = app_cfg.tests.take() {
        if tests.is_empty() {
            default_globs.iter().map(|s| s.to_string()).collect()
        } else {
            tests
        }
    } else {
        default_globs.iter().map(|s| s.to_string()).collect()
    };

    // Build the list of (full_path, relative_path) scripts to run.
    let run_list: Vec<(PathBuf, String)> = if let Some(scripts) = &forced {
        // Re-run mode: the listed scripts are relative to `<base>/test`.
        scripts
            .iter()
            .map(|rel| (base_path.join("test").join(rel), rel.clone()))
            .collect()
    } else {
        let mut list = Vec::new();
        for item in test_globs {
            for path in
                glob::glob(base_path.join(format!("test/{item}")).to_str().unwrap()).unwrap()
            {
                let file_name = path.unwrap();
                if exclude_tests.contains(&file_name) {
                    continue;
                }
                let rel = file_name
                    .strip_prefix(base_path.join("test"))
                    .map(|p| {
                        p.to_string_lossy()
                            .trim_start_matches(std::path::MAIN_SEPARATOR)
                            .to_string()
                    })
                    .unwrap_or_else(|_| file_name.to_string_lossy().to_string());
                list.push((file_name, rel));
            }
        }
        list
    };

    let total = run_list.len();
    let mut done: usize = 0;
    let mut failed_count: usize = 0;
    // Failed scripts, each paired with its error message and the `@features`
    // it declares (empty when the test has no `@features`), for grouped reporting.
    let mut failed_scripts: Vec<(String, String, Vec<String>)> = Vec::new();
    // per-feature statistics: feature -> (total, pass)
    let mut feat_stats: HashMap<String, (usize, usize)> = HashMap::new();

    eprint!("Test 0 / {}, Fail 0", total);

    for (file_name, rel) in run_list {
        // Record the script we are about to run so the panic hook can print it
        // on a mid-run crash. In `panic = "abort"` builds we additionally write
        // it to `--log-file` (and flush) before running, since the hook cannot
        // run there.
        *CURRENT_SCRIPT.lock().unwrap() = Some(rel.clone());
        if let Some(f) = logf.as_mut()
            && writeln!(f, "{rel}").is_ok()
        {
            let _ = f.flush();
        }

        let (status, feats, err_msg) =
            run_test_file(&file_name, &harness_path, &dec, &rel, &mut errjson).await;
        match status {
            TestStatus::Fail => {
                failed_count += 1;
                failed_scripts.push((
                    rel.clone(),
                    err_msg.unwrap_or_else(|| "(no error message)".to_string()),
                    feats.clone(),
                ));
            }
            TestStatus::Pass | TestStatus::Skip => {}
        }
        // attribute the test to each of its features (skipped tests are not counted).
        // Tests with no `@features` frontmatter are attributed to a synthetic key so
        // their results (including failures) still appear in the feature table.
        if matches!(status, TestStatus::Pass | TestStatus::Fail) {
            let keys: Vec<&str> = if feats.is_empty() {
                vec![NO_FEATURE_KEY]
            } else {
                feats.iter().map(|s| s.as_str()).collect()
            };
            for f in keys {
                let e = feat_stats.entry(f.to_string()).or_insert((0, 0));
                e.0 += 1;
                if matches!(status, TestStatus::Pass) {
                    e.1 += 1;
                }
            }
        }
        done += 1;
        eprint!("\rTest {} / {}, Fail {}", done, total, failed_count);

        errjson.flush().unwrap();
    }

    eprintln!();
    eprintln!("Done: {} tests, {} failed", total, failed_count);

    // Generate the Markdown run report.
    let is_error_update = forced.is_some();
    let preset_name = if is_error_update {
        "ERROR UPDATE".to_string()
    } else if dec.user_features.is_some() {
        // Explicit features whitelist: the run is driven by the user's list,
        // not by an edition preset.
        format!("USER [suite {}]", suite.label())
    } else {
        format!(
            "{} [suite {}]",
            app_cfg.preset.clone().unwrap_or_else(|| "N/A".to_string()),
            suite.label()
        )
    };
    let feature_list: Option<Vec<String>> = if is_error_update {
        None
    } else {
        Some(enabled_features(&dec))
    };

    write_report(
        &preset_name,
        feature_list.as_deref(),
        test_filter.as_deref(),
        total,
        failed_count,
        &feat_stats,
        &failed_scripts,
    );
}

#[tokio::main]
async fn main() {
    main_impl().await;
}
