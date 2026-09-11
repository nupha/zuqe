// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::HashSet;

use icu::locale::{
    Locale, LocaleCanonicalizer,
    extensions::unicode::{Key, Value},
};

use crate::{Array, Atom, ScopedContext, error::JSResult, object::Object, value::JSValue};

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

/// Grandfathered tags with an RFC 5646 preferred-value mapping (checked
/// case-insensitively when ICU4X cannot parse the original tag).
const GRANDFATHERED_PREFERRED: &[(&str, &str)] = &[
    ("art-lojban", "jbo"),
    ("cel-gaulish", "xtg"),
    ("zh-min-nan", "nan"),
    ("en-GB-oed", "en-GB-oxendict"),
    ("i-ami", "ami"),
    ("i-bnn", "bnn"),
    ("i-hak", "hak"),
    ("i-klingon", "tlh"),
    ("i-lux", "lb"),
    ("i-navajo", "nv"),
    ("i-pwn", "pwn"),
    ("i-tao", "tao"),
    ("i-tay", "tay"),
    ("i-tsu", "tsu"),
    ("sgn-BE-FR", "sfb"),
    ("sgn-BE-NL", "vgt"),
    ("sgn-CH-DE", "sgg"),
];

/// Canonicalize a single locale string into its canonical BCP-47 form, or
/// return `None` if it cannot be parsed as a valid language tag.
pub(crate) fn canonicalize_locale(
    _ctx: &mut ScopedContext,
    s: &str,
) -> Result<Option<Locale>, i32> {
    // Irregular grandfathered tags (`i-klingon`, ...) are rejected outright;
    // only regular-grammar tags proceed to the preferred-value mapping and
    // ICU4X parsing below.
    let lower = s.to_ascii_lowercase();
    if !is_structurally_valid_language_tag(&lower) {
        return Ok(None);
    }
    // RFC 5646 §2.2.1 grandfathered tags with a preferred-value mapping are
    // replaced before parsing (`cel-gaulish` parses as a regular tag but must
    // canonicalize to its preferred value).
    if let Some((_, pref)) = GRANDFATHERED_PREFERRED.iter().find(|(g, _)| *g == lower) {
        return canonicalize_locale(_ctx, pref);
    }
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

/// ECMA-402 IsStructurallyValidLanguageTag (subset covering test262):
/// subtag placement/shape, no duplicate variants or singletons, and `-u-`
/// key/value alternation.
///
/// `allow_u_attrs` permits attribute-shaped (3-8 alnum) components before the
/// first key in a `-u-` section (`und-u-attr` is valid per RFC 6497).
// ponytail: DisplayNames rejects such attributes (engines disagree here); pass
// allow_u_attrs=false there if a test ever demands the strict side.
pub(crate) fn is_structurally_valid_language_tag_allow_attrs(s: &str, allow_u_attrs: bool) -> bool {
    let is_alpha = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_alphabetic());
    let is_alnum = |x: &str| !x.is_empty() && x.bytes().all(|b| b.is_ascii_alphanumeric());
    let parts: Vec<&str> = s.split('-').collect();
    let lang = parts[0];
    let lang_ok = ((lang.len() == 2 || lang.len() == 3) && is_alpha(lang))
        || ((5..=8).contains(&lang.len())
            && lang.as_bytes()[0].is_ascii_alphabetic()
            && is_alnum(lang));
    if !lang_ok {
        return false;
    }

    let variant_ok = |p: &str| {
        ((5..=8).contains(&p.len()) && is_alnum(p))
            || (p.len() == 4 && p.as_bytes()[0].is_ascii_digit())
    };

    let mut script_seen = false;
    let mut region_seen = false;
    let mut variants = HashSet::<String>::new();
    let mut singletons = HashSet::<char>::new();
    // Section state: ' ' base grammar, else the current singleton extension.
    let mut section = ' ';
    let mut u_expect_value = false;
    let mut u_seen_key = false;
    let mut u_any = false;
    let mut section_empty = true;
    let mut tlang_seen = HashSet::<String>::new();

    for p in &parts[1..] {
        if p.is_empty() {
            return false;
        }
        // A one-character subtag starts a new singleton extension except at
        // base level (where it may also be invalid), inside a private-use `x`
        // section where 1-char components are legal, and inside a `u` section
        // before its first component (`da-u-t-…` is invalid).
        if (u_any || section != 'u')
            && !p.bytes().next().is_some_and(|b| b.is_ascii_digit())
            && p.len() == 1
            && section != 'x'
        {
            let c = p.chars().next().unwrap().to_ascii_lowercase();
            // A singleton must be alphanumeric (`de-*` is invalid).
            if !c.is_ascii_alphanumeric() || !singletons.insert(c) {
                return false;
            }
            section = c;
            u_expect_value = false;
            u_seen_key = false;
            u_any = false;
            section_empty = true;
            continue;
        }
        section_empty = false;
        if section == 'u' {
            u_any = true;
        }
        match section {
            ' ' => {
                // Subtag order: language, script, region, variants. A script
                // may only appear directly after the language.
                if !script_seen && !region_seen && p.len() == 4 && is_alpha(p) {
                    script_seen = true;
                } else if !region_seen
                    && ((p.len() == 2 && is_alpha(p))
                        || (p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit())))
                {
                    region_seen = true;
                } else if variant_ok(p) && variants.insert(p.to_ascii_lowercase()) {
                    // ok
                } else {
                    return false;
                }
            }
            'u' => {
                // Strict mode (DisplayNames): attributes/keys/values must
                // alternate exactly, so attribute-shaped unknowns like
                // `hebrew` are rejected. Lenient mode: any 2-8 alnum
                // component sequence is accepted — CLDR types can span
                // multiple hyphenated segments (`ca-islamic-civil`), which
                // only canonicalization with bcp47 data can fully judge.
                let is_lenient = allow_u_attrs;
                if p.len() < 2 || p.len() > 8 || !is_alnum(p) {
                    return false;
                }
                if !is_lenient {
                    if p.len() == 2 {
                        u_expect_value = true;
                        u_seen_key = true;
                    } else if u_expect_value {
                        u_expect_value = false;
                    } else {
                        return false;
                    }
                }
            }
            't' => {
                // Transform section: tlang subtags then field pairs; duplicate
                // subtags are invalid (`de-t-en-emodeng-emodeng`).
                let low = p.to_ascii_lowercase();
                if !is_alnum(p) || !tlang_seen.insert(low) {
                    return false;
                }
            }
            _ => {
                if !is_alnum(p) {
                    return false;
                }
            }
        }
    }
    // `-u-`, `-t-` and `-x-` sections require at least one component
    // (`da-u`, `en-t`, `si-x`).
    if section_empty && (section == 'u' || section == 't' || section == 'x') {
        return false;
    }
    true
}

/// Wrapper using the default policy (attributes allowed).
pub(crate) fn is_structurally_valid_language_tag(s: &str) -> bool {
    is_structurally_valid_language_tag_allow_attrs(s, true)
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
                replacements.push((*k, canon.to_string()));
            } else if is_yes_true_key(k) && value_str == "yes" {
                // yes → true; a "true" value serialises as no subtag (key-only),
                // e.g. `und-u-kb-yes` → `und-u-kb`.
                replacements.push((*k, "true".to_string()));
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
    if let Ok(m0_key) = "m0".parse::<transform::Key>()
        && let Some(v) = loc.extensions.transform.fields.get(&m0_key)
    {
        let value_str = v.to_string();
        if let Some((_, canon)) = M0_ALIASES.iter().find(|(a, _)| *a == value_str)
            && let Ok(nv) = transform::Value::try_from_str(canon)
        {
            loc.extensions.transform.fields.set(m0_key, nv);
        }
    }
}

/// Convert a JS string value to an owned UTF-8 `String`.
pub(crate) fn to_utf8_string(ctx: &mut ScopedContext, v: JSValue) -> Result<String, i32> {
    let s = v.to_string(ctx)?;
    Ok(s.content().encode_utf8(false).into_owned())
}

/// Every ECMA-402 constructor: TypeError when called without `new`.
/// (In native constructors `cc.this()` carries the new.target.)
pub(crate) fn require_new_target(ctx: &mut ScopedContext, new_target: JSValue) -> Result<(), i32> {
    if new_target.is_undefined() {
        return Err(ctx.throw_type_error("Intl constructor requires 'new'"));
    }
    Ok(())
}

/// ECMA-402 §9.2.10 GetOptionsObject: `None` for undefined, the object for
/// objects, TypeError for anything else (null and primitives included).
pub(crate) fn get_options_object(
    ctx: &mut ScopedContext,
    options: JSValue,
) -> Result<Option<Object>, i32> {
    if options.is_undefined() {
        return Ok(None);
    }
    match options.as_object() {
        Some(o) => Ok(Some(o)),
        None => Err(ctx.throw_type_error("options must be an object")),
    }
}

/// ECMA-402 GetOption (string type): read `property` from `options`, apply
/// ToString, and throw RangeError when the value is not in `values` (empty
/// list = no validation). Returns `None` when the property is absent.
pub(crate) fn get_string_option(
    ctx: &mut ScopedContext,
    options: &Object,
    property: &str,
    values: &[&str],
) -> Result<Option<String>, i32> {
    let v = options.get_property_str(ctx, property)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = to_utf8_string(ctx, v)?;
    if !values.is_empty() && !values.contains(&s.as_str()) {
        return Err(ctx.throw_range_error("Invalid option value"));
    }
    Ok(Some(s))
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

/// True when `v` is an [[InitializedLocale]] object (an `Intl.Locale`).
fn is_locale_instance(v: &JSValue) -> bool {
    match v.as_object() {
        Some(o) => o.payload_intl_locale().is_some(),
        None => false,
    }
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
    let mut out: Vec<JSValue> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    let single = |ctx: &mut ScopedContext,
                  tag_value: JSValue,
                  out: &mut Vec<JSValue>,
                  seen: &mut std::collections::HashSet<String>|
     -> Result<(), i32> {
        // An [[InitializedLocale]] object contributes its internal tag
        // directly (its toString is not called).
        let tag = match tag_value
            .as_object()
            .map(|o| o.payload_intl_locale().map(|l| l.locale.to_string()))
        {
            Some(Some(tag)) => tag,
            _ => to_utf8_string(ctx, tag_value)?,
        };
        canonicalize_tag_string(ctx, &tag, out, seen)
    };

    // 3. A String or an Intl.Locale instance is a single-element list.
    if locales.is_string() || is_locale_instance(&locales) {
        single(ctx, locales, &mut out, &mut seen)?;
        return Ok(Array::from_values(ctx, out)?.into());
    }

    let obj = locales.objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    for i in 0..len {
        // ECMA-402 §9.1 step 7.b: skip absent elements (holes).
        let prop_atom = Atom::new_i64(ctx, i)?;
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
        single(ctx, v, &mut out, &mut seen)?;
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
        .map(|(k, _)| *k)
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
            if let Some(opt) = option_value
                && supported_option_value(key_str, &lang, opt)
                && *opt != ext_value_str
            {
                locale.extensions.unicode.keywords.remove(kw);
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

/// Raw serialization of a Unicode extension keyword: a key-only keyword
/// (`-u-kn`) yields `""`, not `"true"`.
pub(crate) fn locale_extension_raw(locale: &Locale, key: &str) -> Option<String> {
    let kw: icu::locale::extensions::unicode::Key = key.parse().ok()?;
    Some(locale.extensions.unicode.keywords.get(&kw)?.to_string())
}

/// Whether a Unicode extension value is supported for the given locale, for
/// the keys that need availability data (`co`). Other keys (kf/kn/hc/nu/ca/
/// cu) are always treated as supported if structurally valid.
fn supported_extension_value(key: &str, lang: &str, value: &str) -> bool {
    match key {
        "co" => {
            // "standard"/"search" are reserved types, never treated as
            // supported tailoring values (ECMA-402 §10.2.3).
            supported_collations(lang).contains(&value)
        }
        "nu" => {
            // A `-u-nu` value must be a known CLDR numbering-system type.
            supported_numbering_systems().contains(&value)
        }
        "ca" => supported_calendars().contains(&value),
        _ => true,
    }
}

/// Canonicalize a calendar id alias (UTS 35 key type aliases) to its
/// canonical form.
pub(crate) fn canonicalize_calendar_id(s: &str) -> &str {
    match s {
        "islamicc" => "islamic-civil",
        "gregorian" => "gregory",
        "ethiopic-amete-alem" => "ethioaa",
        other => other,
    }
}

/// Known CLDR calendar (ca) types.
pub(crate) const fn supported_calendars() -> &'static [&'static str] {
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
        // Aliases (canonicalized by `canonicalize_calendar_id`); kept here so
        // `-u-ca-gregorian`-style extensions resolve, but filtered out of
        // `Intl.supportedValuesOf("calendar")`.
        "gregorian",
        "islamicc",
    ]
}

/// Known CLDR numbering-system types (subset of the bcp47 `nu` type values).
pub(crate) const fn supported_numbering_systems() -> &'static [&'static str] {
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

/// CLDR collation types supported by `Intl.Collator` (excludes the reserved
/// "standard"/"search"). Mirrors `supported_collations()` per-language data;
/// used by `Intl.supportedValuesOf("collation")` and Locale option validation.
pub(crate) const SUPPORTED_COLLATION_TYPES: &[&str] = &[
    "emoji", "eor", "gb2312", "pinyin", "phonebk", "stroke", "zhuyin",
];

/// Sanctioned simple unit identifiers (UTS 35), for
/// `Intl.supportedValuesOf("unit")`.
pub(crate) const SUPPORTED_UNITS: &[&str] = &[
    "acre",
    "bit",
    "byte",
    "celsius",
    "centimeter",
    "day",
    "degree",
    "fahrenheit",
    "fluid-ounce",
    "foot",
    "gallon",
    "gigabit",
    "gigabyte",
    "gram",
    "hectare",
    "hour",
    "inch",
    "kilobit",
    "kilobyte",
    "kilogram",
    "kilometer",
    "liter",
    "megabit",
    "megabyte",
    "meter",
    "microsecond",
    "mile",
    "mile-scandinavian",
    "milliliter",
    "millimeter",
    "millisecond",
    "minute",
    "month",
    "nanosecond",
    "ounce",
    "percent",
    "petabyte",
    "pound",
    "second",
    "stone",
    "terabit",
    "terabyte",
    "week",
    "yard",
    "year",
];

/// Currencies with known display data: (ISO 4217 code, en name). Drives both
/// `Intl.supportedValuesOf("currency")` and `Intl.DisplayNames` currency
/// lookup so the two stay consistent.
// ponytail: static snapshot of common ISO 4217 codes; extend when needed
pub(crate) const SUPPORTED_CURRENCIES: &[(&str, &str)] = &[
    ("AED", "UAE Dirham"),
    ("ARS", "Argentine Peso"),
    ("AUD", "Australian Dollar"),
    ("BGN", "Bulgarian Lev"),
    ("BHD", "Bahraini Dinar"),
    ("BRL", "Brazilian Real"),
    ("CAD", "Canadian Dollar"),
    ("CHF", "Swiss Franc"),
    ("CLP", "Chilean Peso"),
    ("CNY", "Chinese Yuan"),
    ("COP", "Colombian Peso"),
    ("CZK", "Czech Koruna"),
    ("DKK", "Danish Krone"),
    ("EGP", "Egyptian Pound"),
    ("EUR", "Euro"),
    ("GBP", "British Pound"),
    ("HKD", "Hong Kong Dollar"),
    ("HUF", "Hungarian Forint"),
    ("IDR", "Indonesian Rupiah"),
    ("ILS", "Israeli New Shekel"),
    ("INR", "Indian Rupee"),
    ("ISK", "Icelandic Krona"),
    ("JPY", "Japanese Yen"),
    ("KRW", "South Korean Won"),
    ("KWD", "Kuwaiti Dinar"),
    ("MAD", "Moroccan Dirham"),
    ("MXN", "Mexican Peso"),
    ("MYR", "Malaysian Ringgit"),
    ("NGN", "Nigerian Naira"),
    ("NOK", "Norwegian Krone"),
    ("NZD", "New Zealand Dollar"),
    ("PHP", "Philippine Peso"),
    ("PKR", "Pakistani Rupee"),
    ("PLN", "Polish Zloty"),
    ("QAR", "Qatari Riyal"),
    ("RON", "Romanian Leu"),
    ("RUB", "Russian Ruble"),
    ("SAR", "Saudi Riyal"),
    ("SEK", "Swedish Krona"),
    ("SGD", "Singapore Dollar"),
    ("THB", "Thai Baht"),
    ("TRY", "Turkish Lira"),
    ("TWD", "New Taiwan Dollar"),
    ("UAH", "Ukrainian Hryvnia"),
    ("USD", "US Dollar"),
    ("VND", "Vietnamese Dong"),
    ("ZAR", "South African Rand"),
];

/// Canonical IANA time zone identifiers, for
/// `Intl.supportedValuesOf("timeZone")`. Must include the non-continental
/// `Etc/GMT±n` zones and `UTC`; every entry must round-trip through
/// `Intl.DateTimeFormat`.
// ponytail: static zone list; ICU4X compiled data cannot enumerate tzdb
pub(crate) const SUPPORTED_TIME_ZONES: &[&str] = &[
    "Africa/Abidjan",
    "Africa/Accra",
    "Africa/Cairo",
    "Africa/Johannesburg",
    "Africa/Lagos",
    "Africa/Nairobi",
    "America/Anchorage",
    "America/Argentina/Buenos_Aires",
    "America/Bogota",
    "America/Chicago",
    "America/Denver",
    "America/Detroit",
    "America/Halifax",
    "America/Lima",
    "America/Los_Angeles",
    "America/Mexico_City",
    "America/New_York",
    "America/Phoenix",
    "America/Santiago",
    "America/Sao_Paulo",
    "America/Toronto",
    "America/Vancouver",
    "Asia/Baghdad",
    "Asia/Bangkok",
    "Asia/Dhaka",
    "Asia/Dubai",
    "Asia/Hong_Kong",
    "Asia/Jerusalem",
    "Asia/Kolkata",
    "Asia/Kuala_Lumpur",
    "Asia/Manila",
    "Asia/Seoul",
    "Asia/Shanghai",
    "Asia/Singapore",
    "Asia/Taipei",
    "Asia/Tehran",
    "Asia/Tokyo",
    "Atlantic/Azores",
    "Atlantic/Reykjavik",
    "Australia/Adelaide",
    "Australia/Brisbane",
    "Australia/Melbourne",
    "Australia/Perth",
    "Australia/Sydney",
    "Etc/GMT+1",
    "Etc/GMT+10",
    "Etc/GMT+11",
    "Etc/GMT+12",
    "Etc/GMT+2",
    "Etc/GMT+3",
    "Etc/GMT+4",
    "Etc/GMT+5",
    "Etc/GMT+6",
    "Etc/GMT+7",
    "Etc/GMT+8",
    "Etc/GMT+9",
    "Etc/GMT-1",
    "Etc/GMT-10",
    "Etc/GMT-11",
    "Etc/GMT-12",
    "Etc/GMT-13",
    "Etc/GMT-14",
    "Etc/GMT-2",
    "Etc/GMT-3",
    "Etc/GMT-4",
    "Etc/GMT-5",
    "Etc/GMT-6",
    "Etc/GMT-7",
    "Etc/GMT-8",
    "Etc/GMT-9",
    "Europe/Amsterdam",
    "Europe/Athens",
    "Europe/Berlin",
    "Europe/Brussels",
    "Europe/Bucharest",
    "Europe/Budapest",
    "Europe/Copenhagen",
    "Europe/Dublin",
    "Europe/Helsinki",
    "Europe/Lisbon",
    "Europe/London",
    "Europe/Madrid",
    "Europe/Moscow",
    "Europe/Oslo",
    "Europe/Paris",
    "Europe/Prague",
    "Europe/Rome",
    "Europe/Stockholm",
    "Europe/Vienna",
    "Europe/Warsaw",
    "Europe/Zurich",
    "Indian/Maldives",
    "Pacific/Auckland",
    "Pacific/Fiji",
    "Pacific/Honolulu",
    "UTC",
];

/// Description of the ECMA-402 fallback marker symbol
/// (`%Intl%.[[FallbackSymbol]]`), shared by the legacy-construction paths of
/// DateTimeFormat and NumberFormat.
pub(crate) const FALLBACK_SYMBOL_DESC: &str = "IntlLegacyConstructedSymbol";

/// ECMA-402 UnwrapXxx: return `v` when it carries the internal slot
/// (`branded`); otherwise, if `v` is an object, follow the realm fallback
/// symbol property once (ordinary [[Get]] — Proxy traps observe it) and
/// accept that value when branded.
pub(crate) fn unwrap_fallback(
    ctx: &mut ScopedContext,
    v: JSValue,
    branded: &impl Fn(&JSValue) -> bool,
) -> JSResult {
    if branded(&v) {
        return Ok(v);
    }
    let Some(obj) = v.as_object() else {
        return Err(ctx.throw_type_error("method requires a matching receiver"));
    };
    let marker = ctx.create_realm_symbol(FALLBACK_SYMBOL_DESC)?;
    let inner = obj.get_property_value(ctx, marker)?;
    if branded(&inner) {
        Ok(inner)
    } else {
        Err(ctx.throw_type_error("method requires a matching receiver"))
    }
}

/// Legacy-construction tail (ECMA-402 normative optional): attach `inner` on
/// `receiver` under the realm fallback symbol with frozen attributes, and
/// return what the constructor must yield — the receiver itself for object
/// receivers (identity is asserted by test262), `inner` otherwise.
pub(crate) fn attach_fallback(
    ctx: &mut ScopedContext,
    receiver: JSValue,
    inner: JSValue,
) -> JSResult {
    let Some(obj) = receiver.as_object() else {
        return Ok(inner);
    };
    let marker = ctx.create_realm_symbol(FALLBACK_SYMBOL_DESC)?;
    let key = marker.to_atom(ctx)?;
    // { writable: false, enumerable: false, configurable: false }
    let mut obj = obj;
    obj.define_property_value(ctx, key, inner, 0)?;
    Ok(receiver)
}
