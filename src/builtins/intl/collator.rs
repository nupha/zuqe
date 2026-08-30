// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.Collator` (§9).

use icu::locale::{
    Locale,
    extensions::unicode::{Key, Value},
};

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
    value::JSValue,
};

use super::common::{
    locale_to_string, make_bound_method, make_str, resolve_locale, to_utf8_string,
};

#[derive(Debug)]
pub struct CollatorData {
    pub(crate) locale: Locale,
    pub(crate) sensitivity: CollationSensitivity,
    pub(crate) case_first: CaseFirst,
    pub(crate) numeric: bool,
    pub(crate) ignore_punctuation: bool,
    pub(crate) usage: &'static str,
    pub(crate) collation: &'static str,
    pub(crate) collator: icu::collator::CollatorBorrowed<'static>,
}

impl GcTrace for CollatorData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Debug)]
pub(crate) enum CollationSensitivity {
    Base,
    Accent,
    Case,
    Variant,
}

#[derive(Debug)]
pub(crate) enum CaseFirst {
    False,
    Upper,
    Lower,
}

pub(crate) static CLASS_DEF_COLLATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_Collator,
    js_collator_proto_funcs,
    Some(ClassCtorDef::new(js_collator_constructor, 0, true)),
);

/// Build a `CollatorData` from `(locales, options)`, used by both the
/// `Intl.Collator` constructor and `String.prototype.localeCompare`.
pub(crate) fn build_collator_data(
    ctx: &mut ScopedContext,
    locales: JSValue,
    options: JSValue,
) -> Result<CollatorData, i32> {
    let mut locale = resolve_locale(ctx, locales)?;

    // Defaults: variant sensitivity, caseFirst "false", numeric false.
    let mut sensitivity = CollationSensitivity::Variant;
    let mut sensitivity_provided = false;
    let mut case_first = CaseFirst::False;
    let mut numeric = false;
    // Thai collation ignores punctuation by default.
    let lang = locale.id.language.to_string();
    let mut ignore_punctuation = lang == "th";
    let mut usage = "sort";
    // Effective explicit option values for the Collator relevant extension
    // keys (None when the option was not provided). Used by ResolveLocale to
    // strip a Unicode extension key from the resolved locale.
    let mut collation_option: Option<&'static str> = None;
    let mut case_first_option: Option<&'static str> = None;
    let mut numeric_option: Option<&'static str> = None;

    // Read the unicode extension (`u-co`, `u-kn`, `u-kf`) resolved into the
    // canonical locale, plus the explicit options which take precedence.
    for (k, v) in locale.extensions.unicode.keywords.iter() {
        let key = k.to_string();
        let val = v.to_string();
        match key.as_str() {
            "kn" => {
                // `-u-kn` (no value) and `-u-kn-true` mean true; `-u-kn-false` means false.
                numeric = !(val == "false");
            }
            "kf" => {
                case_first = match val.as_str() {
                    "upper" => CaseFirst::Upper,
                    "lower" => CaseFirst::Lower,
                    _ => CaseFirst::False,
                };
            }
            _ => {}
        }
    }

    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let sensitivity_val = obj.get_property_str(ctx, "sensitivity")?;
        if !sensitivity_val.is_undefined() {
            let s = to_utf8_string(ctx, sensitivity_val)?;
            sensitivity_provided = true;
            sensitivity = match s.as_str() {
                "base" => CollationSensitivity::Base,
                "accent" => CollationSensitivity::Accent,
                "case" => CollationSensitivity::Case,
                "variant" => CollationSensitivity::Variant,
                _ => return Err(ctx.throw_range_error("Invalid sensitivity value")),
            };
        }
        let case_first_val = obj.get_property_str(ctx, "caseFirst")?;
        if !case_first_val.is_undefined() {
            let s = to_utf8_string(ctx, case_first_val)?;
            case_first_option = Some(match s.as_str() {
                "upper" => "upper",
                "lower" => "lower",
                "false" => "false",
                _ => return Err(ctx.throw_range_error("Invalid caseFirst value")),
            });
            case_first = match case_first_option {
                Some("upper") => CaseFirst::Upper,
                Some("lower") => CaseFirst::Lower,
                _ => CaseFirst::False,
            };
        }
        let numeric_val = obj.get_property_str(ctx, "numeric")?;
        if !numeric_val.is_undefined() {
            numeric = numeric_val.to_bool()?;
            numeric_option = Some(if numeric { "true" } else { "false" });
        }
        let ignore_val = obj.get_property_str(ctx, "ignorePunctuation")?;
        if !ignore_val.is_undefined() {
            ignore_punctuation = ignore_val.to_bool()?;
        }
        let usage_val = obj.get_property_str(ctx, "usage")?;
        if !usage_val.is_undefined() {
            let s = to_utf8_string(ctx, usage_val)?;
            match s.as_str() {
                "sort" => usage = "sort",
                "search" => usage = "search",
                _ => return Err(ctx.throw_range_error("Invalid usage value")),
            }
        }
        let matcher = obj.get_property_str(ctx, "localeMatcher")?;
        if !matcher.is_undefined() {
            let s = to_utf8_string(ctx, matcher)?;
            if s != "lookup" && s != "best fit" {
                return Err(ctx.throw_range_error("Invalid localeMatcher value"));
            }
        }
        // collation is validated (so its getter is read) but its value is not
        // otherwise used in this implementation.
        let collation_val = obj.get_property_str(ctx, "collation")?;
        if !collation_val.is_undefined() {
            let s = to_utf8_string(ctx, collation_val)?;
            if s.is_empty() {
                return Err(ctx.throw_range_error("Invalid collation value"));
            }
            collation_option = Some(s.leak());
        }
    }

    // ECMA-402 §15.2.2 step 23: if sensitivity is undefined, default to
    // "variant" for sort and "base" for search/other usages.
    if !sensitivity_provided && usage != "sort" {
        sensitivity = CollationSensitivity::Base;
    }

    // Build the ICU4X collator from the resolved locale + options.
    use icu::collator::CollatorPreferences;
    use icu::collator::options::{AlternateHandling, CaseLevel, MaxVariable, Strength};
    use icu::collator::{CollatorBorrowed, options::CollatorOptions};
    use icu::locale::preferences::extensions::unicode::keywords::{
        CollationCaseFirst, CollationNumericOrdering,
    };

    let mut opts = CollatorOptions::default();
    // strength: base -> Primary, accent -> Secondary, case -> Primary+CaseLevel, variant -> Tertiary
    opts.strength = Some(match sensitivity {
        CollationSensitivity::Base => Strength::Primary,
        CollationSensitivity::Accent => Strength::Secondary,
        CollationSensitivity::Case => Strength::Primary,
        CollationSensitivity::Variant => Strength::Tertiary,
    });
    if matches!(sensitivity, CollationSensitivity::Case) {
        opts.case_level = Some(CaseLevel::On);
    }
    if ignore_punctuation {
        opts.alternate_handling = Some(AlternateHandling::Shifted);
        opts.max_variable = Some(MaxVariable::Punctuation);
    } else {
        // Explicitly disable punctuation ignoring (locale tailoring might
        // otherwise enable Shifted handling).
        opts.alternate_handling = Some(AlternateHandling::NonIgnorable);
    }
    // Original `-u-co` extension value, captured before the search-usage
    // emulation below may overwrite it for the ICU preferences.
    let ext_collation = "co"
        .parse::<Key>()
        .ok()
        .and_then(|k| locale.extensions.unicode.keywords.get(&k))
        .map(|v| v.to_string());
    // Start from the locale's own collation preferences (language tailoring,
    // `-u-co` collation type, `-u-kn`/`-u-kf`), then override with the explicit
    // JS options which take precedence.
    if usage == "search" {
        // CLDR's German search collation follows DIN 5007-1 (phonebook)
        // matching; ICU4X ships no `de+search` tailoring, so emulate it via
        // phonebk. Other languages use the plain search type.
        let co_val = if lang == "de" { "phonebk" } else { "search" };
        if let Ok(co_key) = "co".parse::<Key>() {
            if let Ok(co_val) = Value::try_from_str(co_val) {
                locale.extensions.unicode.keywords.set(co_key, co_val);
            }
        }
    }
    let mut prefs = CollatorPreferences::from(&locale);
    prefs.case_first = Some(match case_first {
        CaseFirst::Upper => CollationCaseFirst::Upper,
        CaseFirst::Lower => CollationCaseFirst::Lower,
        CaseFirst::False => CollationCaseFirst::False,
    });
    prefs.numeric_ordering = Some(if numeric {
        CollationNumericOrdering::True
    } else {
        CollationNumericOrdering::False
    });

    let icu_collator = CollatorBorrowed::try_new(prefs, opts)
        .map_err(|_| ctx.throw_range_error("Unable to load collation data"))?;

    // Effective collation for `resolvedOptions.collation`: the `-u-co`
    // extension value if present and supported, else the explicit `collation`
    // option if supported, else "default". "search"/"standard" are reserved
    // types and never reported.
    let lang = locale.id.language.to_string();
    let collation = if let Some(opt) = collation_option
        && supported_collation(&lang, opt)
        && opt != "standard"
    {
        opt.to_string()
    } else if let Some(ext) = &ext_collation
        && supported_collation(&lang, ext)
        && ext != "standard"
    {
        ext.clone()
    } else {
        "default".to_string()
    };
    // `collation` is a static string (either a leaked `collation_option` or a
    // leaked extension value).
    let collation = Box::leak(collation.into_boxed_str()) as &'static str;

    // ECMA-402 ResolveLocale: when an option is set and differs from the
    // corresponding Unicode extension value, or the extension value is
    // unsupported, the extension key is stripped from the resolved locale.
    let locale = super::common::resolve_locale_keys(
        locale,
        &[
            ("co", collation_option),
            ("kf", case_first_option),
            ("kn", numeric_option),
        ],
    );

    Ok(CollatorData {
        locale,
        sensitivity,
        case_first,
        numeric,
        ignore_punctuation,
        usage,
        collation,
        collator: icu_collator,
    })
}

/// Whether a collation type is supported for the given language.
fn supported_collation(lang: &str, collation: &str) -> bool {
    match lang {
        "de" => matches!(collation, "phonebk" | "phonebook" | "eor"),
        "en" => matches!(collation, "eor" | "emoji" | "stroke" | "unicode"),
        "zh" => matches!(collation, "pinyin" | "stroke" | "gb2312" | "zhuyin"),
        "fr" => collation == "reformed",
        _ => false,
    }
}

pub(crate) fn js_collator_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let data = build_collator_data(ctx, cc.arg(0), cc.arg(1))?;
    let instance = ctx
        .ordinary_create_from_constructor_class(
            new_target,
            crate::JS_CLASS_INTL_COLLATOR,
            data,
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// Compare two strings using an ICU4X collator. Exposed for
/// `String.prototype.localeCompare`.
pub(crate) fn compare_collator_data(data: &CollatorData, a: &str, b: &str) -> std::cmp::Ordering {
    data.collator.compare(a, b)
}

/// Compares two strings using the ICU4X collator (locale + options aware).
fn compare_strings(data: &CollatorData, a: &str, b: &str) -> std::cmp::Ordering {
    data.collator.compare(a, b)
}

fn js_collator_compare_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    if !is_collator(&this_val) {
        return Err(ctx.throw_type_error("not a Collator"));
    }
    // The bound `compare` function is anonymous (name is the empty string).
    make_bound_method(ctx, this_val, js_collator_compare_bound, "", 2)
}

fn is_collator(v: &JSValue) -> bool {
    v.as_object()
        .is_some_and(|o| o.payload_intl_collator().is_some())
}

fn js_collator_compare_bound(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = cc.data(0);
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let this = this_obj
        .payload_intl_collator()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let a = to_utf8_string(ctx, cc.arg(0))?;
    let b = to_utf8_string(ctx, cc.arg(1))?;
    let ord = compare_strings(this, &a, &b);
    Ok(JSValue::from(match ord {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

fn js_collator_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let this = this_obj
        .payload_intl_collator()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let mut o = ctx.create_base_object()?;
    // Property order must be: locale, usage, sensitivity, ignorePunctuation,
    // collation, numeric, caseFirst. Use CreateDataProperty semantics so a
    // tainted `Object.prototype` setter is not invoked.
    let v_locale = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", v_locale, JS_PROP_C_W_E)?;
    let v_usage = make_str(ctx, this.usage.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "usage", v_usage, JS_PROP_C_W_E)?;
    let v_sens = make_str(
        ctx,
        match this.sensitivity {
            CollationSensitivity::Base => b"base".to_vec(),
            CollationSensitivity::Accent => b"accent".to_vec(),
            CollationSensitivity::Case => b"case".to_vec(),
            CollationSensitivity::Variant => b"variant".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "sensitivity", v_sens, JS_PROP_C_W_E)?;
    o.define_property_value_str(
        ctx,
        "ignorePunctuation",
        JSValue::from(this.ignore_punctuation),
        JS_PROP_C_W_E,
    )?;
    let v_coll = make_str(ctx, this.collation.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "collation", v_coll, JS_PROP_C_W_E)?;
    o.define_property_value_str(ctx, "numeric", JSValue::from(this.numeric), JS_PROP_C_W_E)?;
    let v_cf = make_str(
        ctx,
        match this.case_first {
            CaseFirst::False => b"false".to_vec(),
            CaseFirst::Upper => b"upper".to_vec(),
            CaseFirst::Lower => b"lower".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "caseFirst", v_cf, JS_PROP_C_W_E)?;
    Ok(o.into())
}

const js_collator_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("compare", Some(js_collator_compare_getter), None, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_collator_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.Collator",
        JS_PROP_CONFIGURABLE,
    ),
];
