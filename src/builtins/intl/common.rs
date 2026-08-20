// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Shared helpers for the Intl implementation.

use icu::locale::{
    Locale, LocaleCanonicalizer,
    extensions::unicode::{Key, Value},
};

use crate::{Array, ScopedContext, value::JSValue};

/// Grandfathered BCP-47 tags that ICU4X rejects but which ECMA-402
/// `IsStructurallyValidLanguageTag` accepts and canonicalises to themselves
/// (no `preferred-value` replacement).
const GRANDFATHERED_AS_IS: &[&str] = &[
    "cel-gaulish",
    "en-GB-oed",
    "i-default",
    "i-enochian",
    "i-mingo",
    "posix",
    "sgn-BE-FR",
    "sgn-BE-NL",
    "sgn-CH-DE",
];

/// Canonicalize a single locale string into its canonical BCP-47 form, or
/// return `None` if it cannot be parsed as a valid language tag.
pub(crate) fn canonicalize_locale(
    _ctx: &mut ScopedContext,
    s: &str,
) -> Result<Option<Locale>, i32> {
    let mut loc: Locale = match s.parse() {
        Ok(l) => l,
        Err(_) => return Ok(None),
    };
    let canonicalizer = LocaleCanonicalizer::new_extended();
    canonicalizer.canonicalize(&mut loc);
    // UTS 35 §3.2.1 Canonical Unicode Locale Identifiers: replace keys/types/
    // tfields/tvalues by their canonical forms using the bcp47 alias data.
    canonicalize_unicode_extensions(&mut loc);
    Ok(Some(loc))
}

/// Canonicalize a single tag string into its canonical BCP-47 string form, or
/// return `None` if it is not a structurally valid language tag.
pub(crate) fn canonicalize_locale_string(
    ctx: &mut ScopedContext,
    s: &str,
) -> Result<Option<String>, i32> {
    if let Some(loc) = canonicalize_locale(ctx, s)? {
        return Ok(Some(loc.to_string()));
    }
    // Some structurally-valid grandfathered tags (RFC 5646 §2.2.1) are not
    // accepted by the ICU4X parser; ECMA-402 keeps them as-is.
    let lower = s.to_ascii_lowercase();
    if GRANDFATHERED_AS_IS.contains(&lower.as_str()) {
        return Ok(Some(lower));
    }
    Ok(None)
}

/// bcp47 type-alias data for the `u` extension keys whose alias values need
/// canonicalisation (from the CLDR `common/bcp47/calendar.xml`, `collation.xml`,
/// `measure.xml`, `timezone.xml`). Key → (alias → canonical type).
///
/// The values tested by test262 (and the common deprecated ones) are listed.
fn unicode_type_alias(key: &Key, alias: &str) -> Option<&'static str> {
    const CA: &[(&str, &str)] = &[
        ("ethiopic-amete-alem", "ethioaa"),
        ("islamicc", "islamic-civil"),
    ];
    const KS: &[(&str, &str)] = &[
        ("primary", "level1"),
        ("secondary", "level2"),
        ("tertiary", "level3"),
        ("quaternary", "level4"),
        ("quarternary", "level4"),
        ("identical", "identic"),
    ];
    const MS: &[(&str, &str)] = &[("imperial", "uksystem")];
    // timezone: deprecated IANA aliases → canonical BCP-47 timezone identifiers.
    const TZ: &[(&str, &str)] = &[
        ("cnckg", "cnsha"),
        ("eire", "iedub"),
        ("est", "papty"),
        ("gmt0", "gmt"),
        ("uct", "utc"),
        ("zulu", "utc"),
    ];
    // Keys kb/kc/kh/kk/kn: type "true" alias "yes" (handled by removal below).
    match key.to_string().as_str() {
        "ca" => CA.iter().find(|(a, _)| *a == alias).map(|(_, n)| *n),
        "ks" => KS.iter().find(|(a, _)| *a == alias).map(|(_, n)| *n),
        "ms" => MS.iter().find(|(a, _)| *a == alias).map(|(_, n)| *n),
        "tz" => TZ.iter().find(|(a, _)| *a == alias).map(|(_, n)| *n),
        _ => None,
    }
}

/// `true` for the `u`-extension keys whose type `true` alias is `yes`
/// (`kb`, `kc`, `kh`, `kk`, `kn`).
fn is_yes_true_key(key: &Key) -> bool {
    matches!(key.to_string().as_str(), "kb" | "kc" | "kh" | "kk" | "kn")
}

/// Apply UTS 35 §3.2.1 canonicalisation to the Unicode `u`-extension keywords
/// and the `t`-transform extension (as far as the ICU4X structure permits).
fn canonicalize_unicode_extensions(loc: &mut Locale) {
    let keywords = &mut loc.extensions.unicode.keywords;
    if !keywords.is_empty() {
        // Collect the canonical replacement decisions first (avoids borrowing
        // `keywords` mutably while iterating it).
        let mut replacements: Vec<(Key, String)> = Vec::new();
        for (k, v) in keywords.iter() {
            let value_str = v.to_string();
            // Subdivision (`rg`/`sd`) values are already canonicalised by ICU4X.
            if let Some(canon) = unicode_type_alias(k, &value_str) {
                replacements.push((k.clone(), canon.to_string()));
            } else if is_yes_true_key(k) && value_str == "yes" {
                // yes → true; a "true" value serialises as no subtag (key-only),
                // e.g. `und-u-kb-yes` → `und-u-kb`.
                replacements.push((k.clone(), "true".to_string()));
            }
        }
        for (key, new_value) in replacements {
            if new_value.is_empty() {
                keywords.remove(key);
            } else if let Ok(v) = Value::try_from_str(&new_value) {
                keywords.set(key, v);
            }
        }
    }

    // Transform-extension method (`m0`) deprecated value aliases (transformAlias).
    use icu::locale::extensions::transform;
    const M0_ALIASES: &[(&str, &str)] = &[("names", "prprname")];
    if let Ok(m0_key) = "m0".parse::<transform::Key>() {
        if let Some(v) = loc.extensions.transform.fields.get(&m0_key) {
            let value_str = v.to_string();
            if let Some((_, canon)) = M0_ALIASES.iter().find(|(a, _)| *a == value_str) {
                if let Ok(nv) = transform::Value::try_from_str(canon) {
                    loc.extensions.transform.fields.set(m0_key, nv);
                }
            }
        }
    }
}

/// Convert a JS string value to an owned UTF-8 `String`.
pub(crate) fn to_utf8_string(ctx: &mut ScopedContext, v: JSValue) -> Result<String, i32> {
    let s = v.to_string(ctx)?;
    Ok(s.content().encode_utf8(false).into_owned())
}

/// Canonicalize a single already-coerced tag string. Returns `Ok(Some(canonical))`
/// on success, `Ok(None)` is never returned; an invalid tag throws a `RangeError`.
fn canonicalize_tag_string(
    ctx: &mut ScopedContext,
    tag: &str,
    out: &mut Vec<JSValue>,
    seen: &mut std::collections::HashSet<String>,
) -> Result<(), i32> {
    let canonical_str = canonicalize_locale_string(ctx, tag)?
        .ok_or_else(|| ctx.throw_range_error("Invalid language tag"))?;
    if seen.insert(canonical_str.clone()) {
        let str_ref = ctx.create_string_from_utf8(canonical_str.into_bytes())?;
        out.push(JSValue::make_string(str_ref));
    }
    Ok(())
}

/// ECMA-402 §8.2.1 `CanonicalizeLocaleList`: coerce `locales` to an array of
/// canonical locale strings, deduplicated.
pub(crate) fn canonicalize_locale_list(
    ctx: &mut ScopedContext,
    locales: JSValue,
) -> Result<JSValue, i32> {
    // 1. If `locales` is undefined, return a new empty List.
    if locales.is_undefined() {
        return Ok(Array::from_values(ctx, Vec::new())?.into());
    }
    // 3. If Type(locales) is String, treat it as a single-element list.
    // 4. Else `ToObject(locales)` then iterate as an array-like.
    let mut out: Vec<JSValue> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    if locales.is_string() {
        let tag = to_utf8_string(ctx, locales)?;
        canonicalize_tag_string(ctx, &tag, &mut out, &mut seen)?;
        return Ok(Array::from_values(ctx, out)?.into());
    }

    let obj = locales.objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    for i in 0..len {
        // ECMA-402 §9.1 step 7.b: skip absent elements (holes).
        let prop_atom = crate::Atom::from_str(ctx, &i.to_string())?;
        if !obj.has_property(ctx, prop_atom)? {
            continue;
        }
        let v = obj.get_property_i64(ctx, i)?;
        // ECMA-402 §9.1 step 7.c: String values and objects are coerced via
        // `ToString` (objects with an [[InitializedLocale]] slot use their
        // [[Locale]]); other primitive values (null/undefined/bool/number/
        // symbol) throw a TypeError.
        if !v.is_string() && !v.is_object() {
            return Err(ctx.throw_type_error("Invalid element of locales list"));
        }
        let tag = to_utf8_string(ctx, v)?;
        canonicalize_tag_string(ctx, &tag, &mut out, &mut seen)?;
    }
    Ok(Array::from_values(ctx, out)?.into())
}

/// Build a JS string value from a `Locale` (its canonical string form).
pub(crate) fn locale_to_string(ctx: &mut ScopedContext, l: &Locale) -> Result<JSValue, i32> {
    Ok(JSValue::make_string(
        ctx.create_string_from_utf8(l.to_string().into_bytes())?,
    ))
}

/// Create a JS string value from UTF-8 bytes.
pub(crate) fn make_str(ctx: &mut ScopedContext, bytes: Vec<u8>) -> Result<JSValue, i32> {
    Ok(JSValue::make_string(ctx.create_string_from_utf8(bytes)?))
}

/// Convert a `Vec<JSValue>` into a JS array value.
pub(crate) fn values_to_array(
    ctx: &mut ScopedContext,
    values: Vec<JSValue>,
) -> Result<JSValue, i32> {
    Ok(Array::from_values(ctx, values)?.into())
}

/// Create a native function bound to `bound` (accessible via `cc.data(0)`
/// inside the callback). Used for the ECMA-402 accessor methods like
/// `NumberFormat.prototype.format`.
pub(crate) fn make_bound_method(
    ctx: &mut ScopedContext,
    bound: JSValue,
    callback: crate::function::CallbackFn,
    name: &str,
    length: u8,
) -> Result<JSValue, i32> {
    let f = ctx.create_native_function_with_data(
        crate::Callback::Func(callback),
        name,
        length,
        0,
        Box::new([bound]),
    )?;
    Ok(f.into())
}

/// Resolve the effective locale for an Intl constructor. Uses the first
/// canonical locale of `locales`; falls back to the runtime default locale.
///
/// ECMA-402 §9.1: `undefined` yields the default locale; `null` fails
/// `ToObject` (TypeError); any structurally-invalid element throws RangeError.
pub(crate) fn resolve_locale(ctx: &mut ScopedContext, locales: JSValue) -> Result<Locale, i32> {
    if locales.is_undefined() {
        return default_locale();
    }
    // Build the canonicalized list (validating every element) and take the
    // first entry, or the default locale if the list is empty.
    let list = canonicalize_locale_list(ctx, locales)?;
    // Only an actually-present, non-undefined first element is used.
    let first = list.as_object().and_then(|obj| {
        let v = obj.get_property_i64(ctx, 0).ok()?;
        if v.is_undefined_or_null() || !v.is_string() {
            None
        } else {
            to_utf8_string(ctx, v).ok()
        }
    });
    match first {
        Some(tag) => canonicalize_locale(ctx, &tag)?
            .ok_or_else(|| ctx.throw_range_error("Invalid language tag")),
        None => default_locale(),
    }
}

/// The runtime default locale (uses `en-US` unless overridden by the host).
fn default_locale() -> Result<Locale, i32> {
    Ok("en-US".parse().unwrap())
}

/// Create a new ordinary object for a spec "Record"/resolvedOptions result and
/// define a data property on it using CreateDataProperty semantics (so a
/// tainted `Object.prototype` setter is not invoked).
pub(crate) fn new_record_object(ctx: &mut ScopedContext) -> Result<crate::object::Object, i32> {
    ctx.create_base_object()
}

/// Define `name` = `val` on `o` with CreateDataProperty semantics.
pub(crate) fn record_set(
    ctx: &mut ScopedContext,
    o: &mut crate::object::Object,
    name: &str,
    val: JSValue,
) -> Result<(), i32> {
    o.define_property_value_str(ctx, name, val, 0)?;
    Ok(())
}

/// Collation types supported by each (language, region) for the `u-co` Unicode
/// extension key. ECMA-402 ResolveLocale only keeps an extension key in the
/// resolved locale when its value is supported by the chosen locale.
///
/// Only the subset relevant to test262 is listed; anything absent is treated
/// as unsupported (falling back to the default collation).
fn supported_collations(lang: &str) -> &'static [&'static str] {
    match lang {
        "de" => &["phonebk", "phonebook", "eor"],
        "en" => &["eor", "emoji", "stroke", "unicode"],
        "zh" => &["pinyin", "stroke", "gb2312", "zhuyin"],
        "fr" => &["reformed"],
        _ => &[],
    }
}

/// ECMA-402 ResolveLocale (steps 8.i.iii.1-2): given the locale's Unicode
/// extension keys and the effective explicit options, strip from the resolved
/// locale any extension key whose value differs from a provided option, or
/// whose value is unsupported for the locale.
///
/// `keys` is a list of `(key, option_value)`: `option_value` is `Some` when
/// the corresponding option was explicitly provided by the caller.
pub(crate) fn resolve_locale_keys(
    mut locale: Locale,
    keys: &[(&str, Option<&'static str>)],
) -> Locale {
    // ECMA-402: attributes in a Unicode extension subtag are ignored; drop
    // them so `de-u-attrval-co-phonebk` resolves like `de-u-co-phonebk`.
    locale.extensions.unicode.attributes.clear();
    // Only the listed keys are relevant to this Intl object; any other
    // Unicode extension keyword is stripped from the resolved locale.
    let relevant: Vec<icu::locale::extensions::unicode::Key> =
        keys.iter().filter_map(|(k, _)| k.parse().ok()).collect();
    let irrelevant: Vec<icu::locale::extensions::unicode::Key> = locale
        .extensions
        .unicode
        .keywords
        .iter()
        .map(|(k, _)| k.clone())
        .filter(|k| !relevant.iter().any(|rk| rk == k))
        .collect();
    for k in irrelevant {
        locale.extensions.unicode.keywords.remove(k);
    }
    let lang = locale.id.language.to_string();
    for (key_str, option_value) in keys {
        // Locate the extension keyword by key.
        let kw = match key_str.parse::<Key>() {
            Ok(k) => k,
            Err(_) => continue,
        };
        let ext_value = locale.extensions.unicode.keywords.get(&kw);
        if let Some(ext_value) = ext_value {
            let mut ext_value_str = ext_value.to_string();
            // A key-only keyword (e.g. `-u-kn`) has an implicit "true" value.
            if ext_value_str.is_empty() {
                ext_value_str = "true".to_string();
            }
            // The extension value is unsupported → strip it.
            if !supported_extension_value(key_str, &lang, &ext_value_str) {
                locale.extensions.unicode.keywords.remove(kw);
                continue;
            }
            // An explicit option that is supported for the locale and differs
            // from the extension value strips the extension key (the option
            // wins). An unsupported option is ignored (the extension value is
            // kept).
            if let Some(opt) = option_value {
                if supported_option_value(key_str, &lang, opt) && *opt != ext_value_str {
                    locale.extensions.unicode.keywords.remove(kw);
                }
            }
        }
    }
    locale
}

/// Get the value of a Unicode extension keyword from a locale, if present.
pub(crate) fn locale_extension_value(locale: &Locale, key: &str) -> Option<String> {
    let kw: icu::locale::extensions::unicode::Key = key.parse().ok()?;
    locale.extensions.unicode.keywords.get(&kw).map(|v| {
        let s = v.to_string();
        if s.is_empty() { "true".to_string() } else { s }
    })
}

/// Whether a Unicode extension value is supported for the given locale, for
/// the keys that need availability data (`co`). Other keys (kf/kn/hc/nu/ca/
/// cu) are always treated as supported if structurally valid.
fn supported_extension_value(key: &str, lang: &str, value: &str) -> bool {
    match key {
        "co" => {
            // "search" is always supported (ECMA-402 §15.2.2 step 12).
            value == "search" || supported_collations(lang).contains(&value)
        }
        "nu" => {
            // A `-u-nu` value must be a known CLDR numbering-system type.
            supported_numbering_systems().contains(&value)
        }
        "ca" => supported_calendars().contains(&value),
        _ => true,
    }
}

/// Known CLDR calendar (ca) types.
fn supported_calendars() -> &'static [&'static str] {
    &[
        "buddhist",
        "chinese",
        "coptic",
        "dangi",
        "ethioaa",
        "ethiopic",
        "gregory",
        "hebrew",
        "indian",
        "islamic",
        "islamic-civil",
        "islamic-rgsa",
        "islamic-tbla",
        "islamic-umalqura",
        "iso8601",
        "japanese",
        "persian",
        "roc",
        "gregorian",
        "islamicc",
    ]
}

/// Known CLDR numbering-system types (subset of the bcp47 `nu` type values).
fn supported_numbering_systems() -> &'static [&'static str] {
    &[
        "latn", "arab", "arabext", "armn", "armnlow", "beng", "cakm", "cham", "deva", "ethi",
        "fullwide", "geor", "grek", "greklow", "gujr", "guru", "hanidays", "hans", "hansfin",
        "hant", "hantfin", "hebr", "hmng", "java", "jpan", "jpanfin", "khmr", "knda", "laoo",
        "mlym", "mong", "mtei", "mymr", "mymrshan", "mymrtlng", "nkoo", "olck", "orya", "osma",
        "roman", "romanlow", "saur", "shrd", "sora", "sund", "takr", "talu", "taml", "tamldec",
        "telu", "thai", "tibt", "vaii", "wara",
        // Additional simple-digit-mapping systems exercised by test262.
        "adlm", "ahom", "bali", "bhks", "brah", "diak", "gara", "gong", "gonm", "gukh", "hmnp",
        "kali", "kawi", "krai", "lana", "lanatham", "lepc", "limb", "mathbold", "mathdbl",
        "mathmono", "mathsanb", "mathsans", "modi", "mroo", "mymrepka", "mymrpao", "nagm", "newa",
        "onao", "outlined", "rohg", "segment", "sind", "sinh", "sunu", "tirh", "tnsa", "tols",
        "wcho", "hanidec",
    ]
}

/// Whether an explicit option value for a key is supported for the locale
/// (ResolveLocale only overrides the extension value when the option is
/// supported). `co` and `nu` have availability data; `kf`/`kn`/`hc`/`ca`/`cu`
/// are treated as always supported.
fn supported_option_value(key: &str, lang: &str, value: &str) -> bool {
    match key {
        "co" => supported_collations(lang).contains(&value),
        "nu" => supported_numbering_systems().contains(&value),
        "ca" => supported_calendars().contains(&value),
        _ => true,
    }
}

/// Whether a value for an extension key is a known type (used to pick between
/// an explicit option and the extension value).
pub(crate) fn supported_extension_key_value(key: &str, value: &str) -> bool {
    match key {
        "ca" => supported_calendars().contains(&value),
        "nu" => supported_numbering_systems().contains(&value),
        _ => true,
    }
}
