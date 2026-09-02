// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.Locale` (搂10).

use icu::locale::Locale as IcuLocale;

use crate::{
    ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_CONFIGURABLE, PayloadPtr},
    value::JSValue,
};

use super::common::{
    canonicalize_locale, get_options_object, get_string_option, locale_to_string, make_str,
    require_new_target, to_utf8_string,
};

/// Opaque payload holding the ICU4X locale for a `Locale` instance. Tags that
/// are valid but unrepresentable in ICU4X (grandfathered like `posix`) keep
/// their raw string for `toString`/`baseName`.
#[derive(Debug)]
pub struct LocaleData {
    pub(crate) locale: IcuLocale,
    pub(crate) raw: Option<String>,
}

impl GcTrace for LocaleData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

crate::def_native_object!(
    builtin,
    "Locale",
    CLASS_DEF_LOCALE,
    class_id: (crate::JS_CLASS_INTL_LOCALE, crate::Atom::CONST_Locale),
    crate::builtins::intl::locale::LocaleData,
    js_locale_proto_funcs,
    Some(crate::class::ClassCtorDef::new(js_locale_constructor, 1, true))
);

/// UN M.49 numeric region codes mapped to canonical alpha-2 (UTS 35
/// territoryAlias; partial table covering common codes).
// ponytail: partial alias table, extend when more numeric regions are exercised
const NUMERIC_REGION_ALIAS: &[(&str, &str)] = &[
    ("036", "AU"),
    ("040", "AT"),
    ("056", "BE"),
    ("076", "BR"),
    ("124", "CA"),
    ("156", "CN"),
    ("158", "TW"),
    ("203", "CZ"),
    ("208", "DK"),
    ("246", "FI"),
    ("250", "FR"),
    ("276", "DE"),
    ("300", "GR"),
    ("344", "HK"),
    ("356", "IN"),
    ("360", "ID"),
    ("372", "IE"),
    ("380", "IT"),
    ("392", "JP"),
    ("408", "KP"),
    ("410", "KR"),
    ("442", "LU"),
    ("458", "MY"),
    ("484", "MX"),
    ("528", "NL"),
    ("554", "NZ"),
    ("578", "NO"),
    ("608", "PH"),
    ("616", "PL"),
    ("620", "PT"),
    ("643", "RU"),
    ("702", "SG"),
    ("704", "VN"),
    ("705", "SI"),
    ("724", "ES"),
    ("752", "SE"),
    ("756", "CH"),
    ("764", "TH"),
    ("792", "TR"),
    ("804", "UA"),
    ("818", "EG"),
    ("826", "GB"),
    ("840", "US"),
];

pub(crate) fn js_locale_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    require_new_target(ctx, cc.this())?;

    // The tag must be a String or an Object; anything else is a TypeError.
    let tag_val = cc.arg(0);
    if !tag_val.is_string() && !tag_val.is_object() {
        return Err(ctx.throw_type_error("tag must be a string or object"));
    }
    let tag = to_utf8_string(ctx, tag_val)?;
    let data = build_locale_data(ctx, &tag)?;

    let options = get_options_object(ctx, cc.arg(1))?;
    let mut data = match options {
        Some(obj) => apply_locale_options(ctx, data, obj)?,
        None => data,
    };
    // The `cel-gaulish` grandfathered alias also covers `cel` + the variant
    // `gaulish` (CLDR languageAlias), collapsing to plain `xtg`.
    if data.locale.id.language.to_string() == "cel"
        && data
            .locale
            .id
            .variants
            .iter()
            .any(|v| v.to_string() == "gaulish")
        && let Ok(xtg) = "xtg".parse::<icu::locale::LanguageIdentifier>()
    {
        data.locale.id = xtg;
    }

    ctx.ordinary_create_from_constructor_payload(
        cc.this(),
        PayloadPtr::new(crate::JS_CLASS_INTL_LOCALE, data),
    )
    .map(JSValue::from)
}

/// Canonicalize `tag` into a `LocaleData`; RangeError when invalid.
fn build_locale_data(ctx: &mut ScopedContext, tag: &str) -> Result<LocaleData, i32> {
    // Structural validity comes first: irregular grandfathered tags without a
    // regular interpretation (`i-default`, `zh-min`) throw even though
    // CanonicalizeLocaleList accepts them elsewhere.
    let lower = tag.to_ascii_lowercase();
    if !super::common::is_structurally_valid_language_tag(&lower) {
        return Err(ctx.throw_range_error("Invalid language tag"));
    }
    // Preferred-value mappings (`cel-gaulish` 鈫?`xtg`, ...) apply next.
    if let Some(loc) = canonicalize_locale(ctx, tag)? {
        return Ok(LocaleData {
            locale: loc,
            raw: None,
        });
    }
    // Structurally valid but unrepresentable in ICU4X (`posix`) stays raw.
    Ok(LocaleData {
        locale: "und".parse().unwrap(),
        raw: Some(lower),
    })
}

fn js_locale_getters(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let this = this_obj
        .payload_intl_locale()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let raw = this.raw.clone();
    let l = &this.locale;
    Ok(match cc.magic() {
        0 => match &raw {
            Some(raw) => make_str(ctx, raw.as_bytes().to_vec())?, // toString
            None => locale_to_string(ctx, l)?,
        },
        1 => make_str(ctx, l.id.language.to_string().into_bytes())?, // language
        2 => match l.id.script {
            // script
            Some(s) => make_str(ctx, s.to_string().into_bytes())?,
            None => JSValue::UNDEFINED,
        },
        3 => match l.id.region {
            // region
            Some(r) => make_str(ctx, r.to_string().into_bytes())?,
            None => JSValue::UNDEFINED,
        },
        4 => match &raw {
            // baseName
            Some(raw) => make_str(ctx, raw.as_bytes().to_vec())?,
            None => make_str(ctx, l.id.to_string().into_bytes())?,
        },
        5 => keyword_getter(ctx, l, "ca")?,  // calendar
        6 => keyword_getter(ctx, l, "co")?,  // collation
        7 => keyword_getter(ctx, l, "nu")?,  // numberingSystem
        8 => variants_getter(ctx, l)?,       // variants
        9 => keyword_getter(ctx, l, "kf")?,  // caseFirst
        10 => keyword_getter(ctx, l, "hc")?, // hourCycle
        11 => JSValue::from(
            // numeric: boolean, false when absent
            super::common::locale_extension_raw(l, "kn").is_some_and(|v| v.is_empty()),
        ),
        _ => unreachable!(),
    })
}

/// Read a Unicode extension keyword from the locale (`None` 锟?undefined).
/// Key-only keywords serialize as "".
fn keyword_getter(ctx: &mut ScopedContext, l: &IcuLocale, key: &str) -> Result<JSValue, i32> {
    Ok(match super::common::locale_extension_raw(l, key) {
        Some(v) => make_str(ctx, v.into_bytes())?,
        None => JSValue::UNDEFINED,
    })
}

/// The variant subtags joined by "-", or undefined when there are none.
fn variants_getter(ctx: &mut ScopedContext, l: &IcuLocale) -> Result<JSValue, i32> {
    if l.id.variants.is_empty() {
        return Ok(JSValue::UNDEFINED);
    }
    let joined =
        l.id.variants
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("-");
    make_str(ctx, joined.into_bytes())
}

const js_locale_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_locale_getters, 0),
    FieldDef::GETSET("language", Some(js_locale_getters), None, 1),
    FieldDef::GETSET("script", Some(js_locale_getters), None, 2),
    FieldDef::GETSET("region", Some(js_locale_getters), None, 3),
    FieldDef::GETSET("baseName", Some(js_locale_getters), None, 4),
    FieldDef::GETSET("calendar", Some(js_locale_getters), None, 5),
    FieldDef::GETSET("collation", Some(js_locale_getters), None, 6),
    FieldDef::GETSET("numberingSystem", Some(js_locale_getters), None, 7),
    FieldDef::GETSET("variants", Some(js_locale_getters), None, 8),
    FieldDef::GETSET("caseFirst", Some(js_locale_getters), None, 9),
    FieldDef::GETSET("hourCycle", Some(js_locale_getters), None, 10),
    FieldDef::GETSET("numeric", Some(js_locale_getters), None, 11),
    FieldDef::FUNC("maximize", 0, js_locale_maximize, 0),
    FieldDef::FUNC("minimize", 0, js_locale_minimize, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Intl.Locale", JS_PROP_CONFIGURABLE),
];

/// Maximize `id` (Add Likely Subtags), patching the root-language rows that
/// ICU4X 2.3 compiled data leaves unchanged.
// ponytail: partial fallback table for und rows; extend when more are hit
fn maximize_id(id: &mut icu::locale::LanguageIdentifier) {
    let expander = icu::locale::LocaleExpander::new_extended();
    if id.language.to_string() == "und" {
        expander.maximize(id);
        if id.language.to_string() != "und" {
            return;
        }
        let region = id.region.map(|r| r.to_string());
        let script = id.script.map(|s| s.to_string());
        // (script, region): likely language / script / region subtags.
        let (lang, def_script, def_region): (&str, Option<&str>, Option<&str>) =
            match (script.as_deref(), region.as_deref()) {
                (None, None) => ("en", Some("Latn"), Some("US")),
                (None, Some("150")) => ("en", Some("Latn"), Some("150")),
                (s, Some("AQ")) if s.is_none() || s == Some("Latn") => {
                    ("en", Some("Latn"), Some("AQ"))
                }
                _ => return,
            };
        if let Ok(l) = lang.parse() {
            id.language = l;
        }

        if id.script.is_none()
            && def_script.is_some()
            && let Ok(s) = def_script.unwrap().parse()
        {
            id.script = Some(s);
        }

        if id.region.is_none()
            && def_region.is_some()
            && let Ok(r) = def_region.unwrap().parse()
        {
            id.region = Some(r);
        }
    } else {
        expander.maximize(id);
    }
}

fn js_locale_maximize(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let this = this_obj
        .payload_intl_locale()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let mut l = this.locale.clone();
    maximize_id(&mut l.id);
    let data = LocaleData {
        locale: l,
        raw: this.raw.clone(),
    };
    ctx.ordinary_create_from_constructor_payload(
        cc.this(),
        PayloadPtr::new(crate::JS_CLASS_INTL_LOCALE, data),
    )
    .map(JSValue::from)
}

fn js_locale_minimize(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let this = this_obj
        .payload_intl_locale()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let mut l = this.locale.clone();
    // RemoveLikelySubtags is defined as remove(add(x)).
    maximize_id(&mut l.id);
    icu::locale::LocaleExpander::new_extended().minimize(&mut l.id);
    let data = LocaleData {
        locale: l,
        raw: this.raw.clone(),
    };
    ctx.ordinary_create_from_constructor_payload(
        cc.this(),
        PayloadPtr::new(crate::JS_CLASS_INTL_LOCALE, data),
    )
    .map(JSValue::from)
}

fn apply_locale_options(
    ctx: &mut ScopedContext,
    mut data: LocaleData,
    obj: crate::object::Object,
) -> Result<LocaleData, i32> {
    data.raw = None;

    // Spec read order: language, script, region, variants, then the keywords
    // calendar, collation, hourCycle, caseFirst, numeric, numberingSystem.
    if let Some(s) = get_string_option(ctx, &obj, "language", &[])? {
        let b = s.as_bytes();
        // unicode_language_subtag: alpha{2,3} | 5*8alphanum starting with alpha.
        let valid = ((b.len() == 2 || b.len() == 3) && b.iter().all(|c| c.is_ascii_alphabetic()))
            || ((5..=8).contains(&b.len())
                && b[0].is_ascii_alphabetic()
                && b.iter().all(|c| c.is_ascii_alphanumeric()));
        if !valid {
            return Err(ctx.throw_range_error("Invalid language option"));
        }
        if let Ok(l) = s.to_ascii_lowercase().parse() {
            data.locale.id.language = l;
        }
    }
    if let Some(s) = get_string_option(ctx, &obj, "script", &[])? {
        if s.len() != 4 || !s.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(ctx.throw_range_error("Invalid script option"));
        }
        if let Ok(sc) = s.to_ascii_lowercase().parse() {
            data.locale.id.script = Some(sc);
        }
    }
    if let Some(s) = get_string_option(ctx, &obj, "region", &[])? {
        let ok = (s.len() == 2 && s.bytes().all(|b| b.is_ascii_alphabetic()))
            || (s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()));
        if !ok {
            return Err(ctx.throw_range_error("Invalid region option"));
        }
        let canon: String = if s.len() == 3 {
            NUMERIC_REGION_ALIAS
                .iter()
                .find(|(n, _)| *n == s)
                .map(|(_, a)| a.to_string())
                .unwrap_or_else(|| s.clone())
        } else {
            s.to_uppercase()
        };
        if let Ok(r) = canon.parse() {
            data.locale.id.region = Some(r);
        }
    }
    if let Some(s) = get_string_option(ctx, &obj, "variants", &[])? {
        // Replace all variant subtags; each must be 5-8 alphanumerics or four
        // characters starting with a digit.
        let lowered = s.to_ascii_lowercase();
        for v in lowered.split('-') {
            let b = v.as_bytes();
            let ok = ((5..=8).contains(&b.len()) && b.iter().all(|c| c.is_ascii_alphanumeric()))
                || (b.len() == 4 && b[0].is_ascii_digit());
            if !ok {
                return Err(ctx.throw_range_error("Invalid variants option"));
            }
        }
        // Parse through a synthetic tag so ICU4X normalizes/sorts the variants.
        match format!("und-{}", lowered).parse::<icu::locale::LanguageIdentifier>() {
            Ok(mut li) => std::mem::swap(&mut data.locale.id.variants, &mut li.variants),
            Err(_) => return Err(ctx.throw_range_error("Invalid variants option")),
        }
    }

    // Unicode extension keywords: per ECMA-402, Locale option values are
    // validated against the BCP-47 `type` grammar (availability filtering is
    // left to the individual constructors).
    if let Some(s) = get_string_option(ctx, &obj, "calendar", &[])? {
        if !super::number_format::is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid calendar"));
        }
        let canon = super::common::canonicalize_calendar_id(&s);
        set_unicode_keyword(&mut data.locale, "ca", canon)?;
    }
    if let Some(s) = get_string_option(ctx, &obj, "collation", &[])? {
        if !super::number_format::is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid collation"));
        }
        set_unicode_keyword(&mut data.locale, "co", &s)?;
    }
    if let Some(s) = get_string_option(ctx, &obj, "hourCycle", &["h11", "h12", "h23", "h24"])? {
        set_unicode_keyword(&mut data.locale, "hc", &s)?;
    }
    if let Some(s) = get_string_option(ctx, &obj, "caseFirst", &["upper", "lower", "false"])? {
        set_unicode_keyword(&mut data.locale, "kf", &s)?;
    }
    let numeric = obj.get_property_str(ctx, "numeric")?;
    if !numeric.is_undefined() {
        // ToBoolean (no ToString); `true` serializes as a key-only keyword.
        let b = numeric.to_bool()?;
        set_unicode_keyword(&mut data.locale, "kn", if b { "" } else { "false" })?;
    }
    if let Some(s) = get_string_option(ctx, &obj, "numberingSystem", &[])? {
        if !super::number_format::is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid numberingSystem"));
        }
        set_unicode_keyword(&mut data.locale, "nu", &s)?;
    }

    Ok(data)
}

/// Set a Unicode extension keyword on the locale (e.g. `ca` 锟?`-u-ca-...`).
/// An empty `value` marks a key-only keyword (`-u-kn`).
fn set_unicode_keyword(locale: &mut IcuLocale, key: &str, value: &str) -> Result<(), i32> {
    use icu::locale::extensions::unicode::{Key, Value};
    let val = Value::try_from_str(value).map_err(|_| -1)?;
    match key.parse::<Key>() {
        Ok(k) => {
            locale.extensions.unicode.keywords.set(k, val);
            Ok(())
        }
        Err(_) => Err(-1),
    }
}
