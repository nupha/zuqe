// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Deserializer, de::Error as _};

use crate::AppConfig;

/// One edition preset from the config file's ordered `presets:` list. Each
/// list entry is written as a single-key map: `- Name: [feature, ...]`.
///
/// Each entry lists the `features.txt` flags that were *newly* introduced in
/// that edition; a preset inherits every feature of the presets listed before
/// it, so the set of features "covered" by a preset is the union of every
/// entry up to and including it. A test is skipped when it requires a feature
/// introduced in a strictly newer edition than the selected preset.
///
/// Feature flags not listed in any preset (stage-3 proposals, Intl features,
/// …) are treated as belonging to the *latest* preset, so they only run when
/// the latest preset is selected and never run under older presets.
#[derive(Debug, Clone)]
pub struct PresetDef {
    pub name: String,
    pub features: Vec<String>,
}

impl<'de> Deserialize<'de> for PresetDef {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // A preset entry is a single-key map: `Name: [feature, ...]`.
        let map = HashMap::<String, Vec<String>>::deserialize(d)?;
        let mut it = map.into_iter();
        match (it.next(), it.next()) {
            (Some((name, features)), None) => Ok(PresetDef { name, features }),
            _ => Err(D::Error::custom(
                "each preset must be a single `Name: [feature, ...]` entry",
            )),
        }
    }
}

/// Resolved feature-selection decisions for a single run.
pub struct Decisions {
    /// Ordered preset list loaded from the config's `presets:` section.
    pub presets: Vec<PresetDef>,
    /// Maps a feature flag to the index of the edition that introduced it.
    /// Features absent from this map are treated as the latest edition.
    pub feature_edition: HashMap<String, usize>,
    /// Index of the selected preset in `presets`.
    pub selected_index: usize,
    /// Index of the latest preset.
    pub max_index: usize,
    /// Features that are always skipped.
    pub exclude: HashSet<String>,
    /// Explicit feature whitelist (`features:` in the config or `--features`).
    /// When set, only tests declaring at least one of these features run, the
    /// edition gate is bypassed, and the run is reported as preset `USER`.
    pub user_features: Option<HashSet<String>>,
}

/// Build the `feature -> edition index` map from the config's preset list.
pub fn build_feature_edition(presets: &[PresetDef]) -> (HashMap<String, usize>, usize) {
    let mut map = HashMap::new();
    for (idx, p) in presets.iter().enumerate() {
        for f in &p.features {
            map.insert(f.clone(), idx);
        }
    }
    (map, presets.len().saturating_sub(1))
}

/// Comma-separated list of all selectable presets (for error messages).
pub fn available_presets(presets: &[PresetDef]) -> String {
    presets
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Whether `name` names an entry in the config's `presets:` list.
pub fn is_known_preset(presets: &[PresetDef], name: &str) -> bool {
    presets.iter().any(|p| p.name == name)
}

/// Resolve the preset, edition index, and exclude set for a run. Assumes the
/// config has already been merged with CLI flags and validated. Takes
/// ownership of the preset list, which lives on in `Decisions`.
pub fn resolve_decisions(cfg: &AppConfig, presets: Vec<PresetDef>) -> Decisions {
    let (feature_edition, max_index) = build_feature_edition(&presets);

    let mut exclude = HashSet::new();
    if let Some(list) = &cfg.exclude_features {
        for f in list {
            exclude.insert(f.clone());
        }
    }

    // Explicit `features:` / `--features` whitelist (USER preset mode). An
    // explicitly requested feature overrides the exclude list: asking for a
    // feature means the user wants it tested even if it is excluded by default.
    let user_features: Option<HashSet<String>> = cfg
        .features
        .as_ref()
        .filter(|list| !list.is_empty())
        .map(|list| {
            let set: HashSet<String> = list.iter().cloned().collect();
            for f in &set {
                exclude.remove(f);
            }
            set
        });

    let selected_index = if user_features.is_some() {
        // USER mode bypasses the edition gate entirely; sit past every edition
        // so only the whitelist (and the exclude set) decides what runs.
        max_index + 1
    } else {
        let preset_name = cfg.preset.as_deref().expect("preset validated");
        presets
            .iter()
            .position(|p| p.name == preset_name)
            .expect("preset validated")
    };

    Decisions {
        presets,
        feature_edition,
        selected_index,
        max_index,
        exclude,
        user_features,
    }
}

/// Decide whether a single test should be skipped.
///
/// In USER mode (an explicit `features` whitelist) a test runs only when it
/// declares at least one whitelisted feature and none of its features is in
/// the exclude set; baseline tests without a `features` annotation are skipped
/// because they exercise no targeted feature.
///
/// Otherwise (preset mode) a test is skipped when:
/// * it requires a feature listed in the exclude set, or
/// * it requires a feature introduced in a strictly newer edition than the
///   selected preset.
///
/// Tests without any `features` annotation (baseline tests) are always run in
/// preset mode.
pub fn should_skip_test(features: Option<&[String]>, dec: &Decisions) -> bool {
    if let Some(user) = &dec.user_features {
        let Some(features) = features else {
            return true;
        };
        // Any co-required excluded feature still skips the test; otherwise run
        // it when at least one declared feature is whitelisted.
        return features.iter().any(|f| dec.exclude.contains(f))
            || !features.iter().any(|f| user.contains(f));
    }

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

/// Sorted list of feature flags actually exercised by a run. In USER mode this
/// is exactly the whitelist; otherwise it is every configured feature whose
/// introducing edition is at or before `selected_index`, minus the exclude set.
pub fn enabled_features(dec: &Decisions) -> Vec<String> {
    if let Some(user) = &dec.user_features {
        let mut feats: Vec<String> = user.iter().cloned().collect();
        feats.sort();
        return feats;
    }

    let mut feats: Vec<String> = dec
        .presets
        .iter()
        .enumerate()
        .filter(|(idx, _)| *idx <= dec.selected_index)
        .flat_map(|(_, p)| p.features.iter().cloned())
        .filter(|f| !dec.exclude.contains(f))
        .collect();

    feats.sort();
    feats.dedup();
    feats
}
