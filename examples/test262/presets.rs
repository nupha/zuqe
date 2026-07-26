// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::{HashMap, HashSet};

use crate::AppConfig;

/// Built-in edition presets.
///
/// Each entry lists the `features.txt` flags that were *newly* introduced in
/// that edition. The set of features "covered" by an edition is the union of
/// every entry up to and including it. A test is skipped when it requires a
/// feature introduced in a strictly newer edition than the selected preset.
///
/// Feature flags not listed here (stage-3 proposals, Intl features, …) are
/// treated as belonging to the *latest* edition, so they only run when the
/// latest preset is selected and never run under older presets.
const BUILTIN_PRESETS: &[(&str, &[&str])] = &[
    (
        "ES5",
        &[
            "IsHTMLDDA",
            "host-gc-required",
            "cross-realm",
            "__proto__",
            "__getter__",
            "__setter__",
        ],
    ),
    (
        "ES2015",
        &[
            "let",
            "const",
            "arrow-function",
            "class",
            "generators",
            "template",
            "for-of",
            "super",
            "new.target",
            "default-parameters",
            "rest-parameters",
            "destructuring-binding",
            "destructuring-assignment",
            "computed-property-names",
            "Symbol",
            "Symbol.iterator",
            "Symbol.hasInstance",
            "Symbol.isConcatSpreadable",
            "Symbol.match",
            "Symbol.replace",
            "Symbol.search",
            "Symbol.species",
            "Symbol.split",
            "Symbol.toPrimitive",
            "Symbol.toStringTag",
            "Symbol.unscopables",
            "Map",
            "Set",
            "WeakMap",
            "WeakSet",
            "Proxy",
            "Reflect",
            "Reflect.construct",
            "Reflect.set",
            "Reflect.setPrototypeOf",
            "Promise",
            "ArrayBuffer",
            "Int8Array",
            "Uint8Array",
            "Uint8ClampedArray",
            "Int16Array",
            "Uint16Array",
            "Int32Array",
            "Uint32Array",
            "Float32Array",
            "Float64Array",
            "TypedArray",
            "DataView",
            "DataView.prototype.getFloat32",
            "DataView.prototype.getFloat64",
            "DataView.prototype.getInt16",
            "DataView.prototype.getInt32",
            "DataView.prototype.getInt8",
            "DataView.prototype.getUint16",
            "DataView.prototype.getUint32",
            "DataView.prototype.setUint8",
            "Array.prototype.values",
            "Object.is",
            "String.fromCodePoint",
            "String.prototype.includes",
            "String.prototype.endsWith",
            "tail-call-optimization",
        ],
    ),
    ("ES2016", &["Array.prototype.includes", "exponentiation"]),
    (
        "ES2017",
        &["async-functions", "SharedArrayBuffer", "Atomics"],
    ),
    (
        "ES2018",
        &[
            "async-iteration",
            "object-rest",
            "object-spread",
            "Promise.prototype.finally",
            "regexp-dotall",
            "regexp-lookbehind",
            "regexp-named-groups",
            "regexp-unicode-property-escapes",
            "Symbol.asyncIterator",
        ],
    ),
    (
        "ES2019",
        &[
            "Array.prototype.flat",
            "Array.prototype.flatMap",
            "Object.fromEntries",
            "json-superset",
            "optional-catch-binding",
            "string-trimming",
            "String.prototype.trimEnd",
            "String.prototype.trimStart",
            "Symbol.prototype.description",
            "stable-array-sort",
            "stable-typedarray-sort",
            "well-formed-json-stringify",
        ],
    ),
    (
        "ES2020",
        &[
            "BigInt",
            "coalesce-expression",
            "dynamic-import",
            "globalThis",
            "export-star-as-namespace-from-module",
            "import.meta",
            "optional-chaining",
            "Promise.allSettled",
            "String.prototype.matchAll",
            "Symbol.matchAll",
        ],
    ),
    (
        "ES2021",
        &[
            "AggregateError",
            "FinalizationRegistry",
            "logical-assignment-operators",
            "numeric-separator-literal",
            "Promise.any",
            "String.prototype.replaceAll",
            "WeakRef",
        ],
    ),
    (
        "ES2022",
        &[
            "arbitrary-module-namespace-names",
            "Array.prototype.at",
            "class-fields-private",
            "class-fields-private-in",
            "class-fields-public",
            "class-methods-private",
            "class-static-block",
            "class-static-fields-private",
            "class-static-fields-public",
            "class-static-methods-private",
            "error-cause",
            "Object.hasOwn",
            "regexp-match-indices",
            "String.prototype.at",
            "top-level-await",
            "TypedArray.prototype.at",
        ],
    ),
    (
        "ES2023",
        &[
            "array-find-from-last",
            "Atomics.waitAsync",
            "change-array-by-copy",
            "hashbang",
            "String.prototype.isWellFormed",
            "String.prototype.toWellFormed",
            "symbols-as-weakmap-keys",
        ],
    ),
    (
        "ES2024",
        &[
            "Array.fromAsync",
            "array-grouping",
            "arraybuffer-transfer",
            "Float16Array",
            "promise-with-resolvers",
            "regexp-v-flag",
            "resizable-arraybuffer",
            "set-methods",
            "uint8array-base64",
        ],
    ),
    (
        "ES2025",
        &[
            "iterator-helpers",
            "json-modules",
            "json-parse-with-source",
            "promise-try",
            "RegExp.escape",
            "regexp-modifiers",
        ],
    ),
    (
        "ES2026",
        &[
            "Error.isError",
            "iterator-sequencing",
            "joint-iteration",
            "Math.sumPrecise",
        ],
    ),
];

/// Features excluded by default: stage-3 proposals and separate specs that are
/// not part of any published ECMAScript edition. They are always skipped
/// unless the user removes them from `exclude_features`.
const DEFAULT_EXCLUDE_FEATURES: &[&str] = &[
    "Intl.Era-monthcode",
    "Intl.Locale-info",
    "Intl.NumberFormat-v3",
    "legacy-regexp",
    "ShadowRealm",
    "decorators",
    "source-phase-imports",
    "source-phase-imports-module-source",
    "import-defer",
    "export-defer",
    "import-text",
    "immutable-arraybuffer",
    "import-bytes",
    "nonextensible-applies-to-private",
    "await-dictionary",
    "error-stack-accessor",
    "Temporal",
];

/// Resolved feature-selection decisions for a single run.
pub struct Decisions {
    /// Maps a feature flag to the index of the edition that introduced it.
    /// Features absent from this map are treated as the latest edition.
    pub feature_edition: HashMap<String, usize>,
    /// Index of the selected preset in `BUILTIN_PRESETS`.
    pub selected_index: usize,
    /// Index of the latest preset.
    pub max_index: usize,
    /// Features that are always skipped.
    pub exclude: HashSet<String>,
}

/// Build the `feature -> edition index` map from `BUILTIN_PRESETS`.
pub fn build_feature_edition() -> (HashMap<String, usize>, usize) {
    let mut map = HashMap::new();
    for (idx, (_, feats)) in BUILTIN_PRESETS.iter().enumerate() {
        for f in *feats {
            map.insert((*f).to_string(), idx);
        }
    }
    (map, BUILTIN_PRESETS.len() - 1)
}

/// Comma-separated list of all selectable presets (for error messages).
pub fn available_presets() -> String {
    BUILTIN_PRESETS
        .iter()
        .map(|(n, _)| *n)
        .chain(std::iter::once("ALL"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `name` names a valid preset (`ALL` or a built-in edition).
pub fn is_known_preset(name: &str) -> bool {
    name == "ALL" || BUILTIN_PRESETS.iter().any(|(n, _)| *n == name)
}

/// Resolve the preset, edition index, and exclude set for a run. Assumes the
/// config has already been merged with CLI flags and validated.
pub fn resolve_decisions(cfg: &AppConfig) -> Decisions {
    let (feature_edition, max_index) = build_feature_edition();

    let preset_name = cfg.preset.as_deref().expect("preset validated");

    let selected_index = if preset_name == "ALL" {
        // Sit past every edition so the edition gate never fires: a test is
        // skipped only by `exclude` (this is the "ALL" preset).
        max_index + 1
    } else {
        BUILTIN_PRESETS
            .iter()
            .position(|(name, _)| *name == preset_name)
            .expect("preset validated")
    };

    let mut exclude = HashSet::new();
    for f in DEFAULT_EXCLUDE_FEATURES {
        exclude.insert((*f).to_string());
    }
    if let Some(list) = &cfg.exclude_features {
        for f in list {
            exclude.insert(f.clone());
        }
    }

    Decisions {
        feature_edition,
        selected_index,
        max_index,
        exclude,
    }
}

/// Decide whether a single test should be skipped.
///
/// A test is skipped when:
/// * it requires a feature listed in the exclude set, or
/// * it requires a feature introduced in a strictly newer edition than the
///   selected preset (for the "ALL" preset `selected_index` is raised past every
///   edition, so this gate never fires and only `exclude` applies).
///
/// Tests without any `features` annotation (baseline tests) are always run.
pub fn should_skip_test(features: Option<&[String]>, dec: &Decisions) -> bool {
    let Some(features) = features else {
        return false;
    };
    for f in features {
        if dec.exclude.contains(f) {
            return true;
        }
        let edition = dec.feature_edition.get(f).copied().unwrap_or(dec.max_index);
        if edition > dec.selected_index {
            return true;
        }
    }
    false
}

/// Sorted list of feature flags actually exercised by a run: every built-in
/// feature whose introducing edition is at or before `selected_index`, minus the
/// exclude set. For the `ALL` preset `selected_index` sits past every edition, so
/// all non-excluded built-in features are listed.
pub fn enabled_features(dec: &Decisions) -> Vec<String> {
    let mut feats: Vec<String> = BUILTIN_PRESETS
        .iter()
        .enumerate()
        .flat_map(|(idx, (_, fs))| {
            fs.iter()
                .filter(move |_| idx <= dec.selected_index)
                .map(|f| (*f).to_string())
        })
        .filter(|f| !dec.exclude.contains(f))
        .collect();
    feats.sort();
    feats.dedup();
    feats
}
