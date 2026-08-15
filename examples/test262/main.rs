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
    pub tests: Option<Vec<String>>,
    pub exclude_tests: Option<Vec<String>>,
    pub exclude_features: Option<Vec<String>>,
    pub features: Option<Vec<String>>,
    pub preset: Option<String>,
    /// Ordered edition presets; each entry inherits the features of every
    /// entry before it (see `PresetDef`).
    pub presets: Option<Vec<PresetDef>>,
    pub log: Option<String>,
}

#[derive(Debug, Parser)]
#[command(version, about = "Run test262 with edition presets")]
pub struct Args {
    /// config file (default: test262.yml in the current directory). All example
    /// flags must follow `cargo run --example test262 --` so cargo does not
    /// intercept them.
    #[arg(long = "config", default_value_t = String::from("test262.yml"))]
    pub config_file: String,

    /// Edition preset to run; must name an entry in the config's `presets:`
    /// If `--preset` is omitted, the `preset:` setting from the config file is
    /// used; when a `features` whitelist is given (see `--features`) the preset
    /// is ignored and the run is reported as preset `USER`. If neither a preset
    /// nor a features whitelist is set, the runner errors out.
    #[arg(long)]
    pub preset: Option<String>,

    /// Feature(s) to test (repeatable or comma-separated), e.g.
    /// --features iterator-helpers,set-methods. Merged with the config's
    /// `features:` list. When given, only tests declaring at least one of these
    /// features run and the preset is reported as `USER`.
    #[arg(long = "features", value_delimiter = ',')]
    pub features: Vec<String>,

    /// Exclude an additional feature (repeatable), e.g. --exclude-feature Temporal
    #[arg(long = "exclude-feature")]
    pub exclude_features: Vec<String>,

    /// Specific test file(s) or glob(s) to run; overrides `tests` in the config.
    #[arg(trailing_var_arg = true)]
    pub test_files: Vec<String>,

    /// Path to a JSONL error log produced by a previous run. When given, only
    /// the scripts listed in that file are re-run (the config's `tests`/`preset`
    /// script filtering is ignored), and the file is rewritten in place with the
    /// current failures (the previous file is backed up to `<name>-last.jsonl`).
    #[arg(long = "error-file")]
    pub error_file: Option<String>,

    /// Log file that records the relative script path *before* each test runs.
    /// Useful when the runner crashes mid-run: because the line is written (and
    /// flushed) before the test executes, the last line of this file is exactly
    /// the script that was running when it crashed — no need to re-run to find
    /// it. Under `panic = "abort"` builds a panic hook cannot help (the process
    /// aborts with no unwinding), so this on-disk record is the reliable way.
    #[arg(long = "log-file")]
    pub log_file: Option<String>,
}

/// Merge CLI flags onto the config loaded from the file. A CLI flag takes
/// precedence over the matching file setting; file settings stay untouched
/// when the corresponding flag is absent.
pub fn apply_cli_overrides(cfg: &mut AppConfig, args: &Args) {
    if let Some(preset) = args.preset.clone() {
        cfg.preset = Some(preset);
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

    let dec = if forced.is_some() {
        // Re-run mode: run exactly the listed scripts, no preset/feature filtering.
        let (_, max_index) = build_feature_edition(&preset_list);
        Decisions {
            presets: preset_list,
            feature_edition: HashMap::new(),
            selected_index: max_index + 1,
            max_index,
            strict: false,
            exclude: HashSet::new(),
            user_features: None,
        }
    } else {
        resolve_decisions(&app_cfg, preset_list)
    };

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
                let fname = fname.unwrap();
                exclude_tests.insert(fname);
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
            vec!["**/*.js".to_string()]
        } else {
            tests
        }
    } else {
        vec!["**/*.js".to_string()]
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
        "USER".to_string()
    } else {
        app_cfg.preset.clone().unwrap_or_else(|| "N/A".to_string())
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
