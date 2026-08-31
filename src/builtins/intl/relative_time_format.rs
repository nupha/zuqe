// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.RelativeTimeFormat` (§15).

use icu::locale::Locale;

use crate::{
    ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
    value::JSValue,
};

use super::common::{
    get_string_option, locale_to_string, make_str, require_new_target, resolve_locale,
    to_utf8_string, values_to_array,
};

#[derive(Debug)]
pub struct RelativeTimeFormatData {
    pub(crate) locale: Locale,
    pub(crate) style: RelStyle,
    pub(crate) numeric: RelNumeric,
    pub(crate) numbering_system: String,
}

impl GcTrace for RelativeTimeFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum RelStyle {
    Long,
    Short,
    Narrow,
}

#[derive(Debug)]
pub(crate) enum RelNumeric {
    Always,
    Auto,
}

crate::def_native_object!(
    builtin,
    "RelativeTimeFormat",
    CLASS_DEF_RELATIVE_TIME_FORMAT,
    class_id: (crate::JS_CLASS_INTL_RELATIVE_TIME_FORMAT, crate::Atom::CONST_RelativeTimeFormat),
    crate::builtins::intl::relative_time_format::RelativeTimeFormatData,
    js_relative_time_format_proto_funcs,
    Some(crate::class::ClassCtorDef::new(
        js_relative_time_format_constructor,
        0,
        true
    ))
);

pub(crate) fn js_relative_time_format_constructor(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    require_new_target(ctx, cc.this())?;
    let locale = resolve_locale(ctx, cc.arg(0))?;
    // RTF uses legacy ToObject semantics: primitives are wrapped, only
    // undefined means "no options" (null throws via objectify).
    let options = if cc.arg(1).is_undefined() {
        None
    } else {
        Some(cc.arg(1).objectify(ctx)?)
    };

    let mut data = RelativeTimeFormatData {
        locale,
        style: RelStyle::Long,
        numeric: RelNumeric::Always,
        numbering_system: "latn".to_string(),
    };

    // Spec read order: localeMatcher, numberingSystem, style, numeric.
    let mut ns_opt: Option<String> = None;
    if let Some(obj) = options {
        let _matcher = get_string_option(ctx, &obj, "localeMatcher", &["lookup", "best fit"])?;
        ns_opt = get_string_option(ctx, &obj, "numberingSystem", &[])?;
        if let Some(s) = get_string_option(ctx, &obj, "style", &["long", "short", "narrow"])? {
            data.style = match s.as_str() {
                "short" => RelStyle::Short,
                "narrow" => RelStyle::Narrow,
                _ => RelStyle::Long,
            };
        }
        if let Some(s) = get_string_option(ctx, &obj, "numeric", &["always", "auto"])? {
            data.numeric = if s == "auto" {
                RelNumeric::Auto
            } else {
                RelNumeric::Always
            };
        }
    }

    // Effective numbering system: explicit valid option > `nu` extension > latn.
    let nu_ext = super::common::locale_extension_value(&data.locale, "nu");
    let nu_ext_valid = nu_ext
        .as_deref()
        .is_some_and(|e| super::common::supported_extension_key_value("nu", e));
    let ns_opt = match ns_opt {
        Some(s) => {
            // A malformed type is a RangeError; a well-formed but unknown
            // system falls back silently via ResolveLocale.
            if !super::number_format::is_valid_bcp47_type(&s) {
                return Err(ctx.throw_range_error("Invalid numberingSystem value"));
            }
            if super::common::supported_extension_key_value("nu", &s) {
                Some(s)
            } else {
                None
            }
        }
        None => None,
    };
    data.numbering_system = ns_opt
        .clone()
        .or_else(|| nu_ext_valid.then(|| nu_ext.clone().unwrap()))
        .unwrap_or_else(|| "latn".to_string());
    let ns_static: Option<&'static str> =
        ns_opt.map(|s| Box::leak(s.into_boxed_str()) as &'static str);
    data.locale = super::common::resolve_locale_keys(data.locale, &[("nu", ns_static)]);

    let instance = ctx
        .ordinary_create_from_constructor_class(
            cc.this(),
            crate::JS_CLASS_INTL_RELATIVE_TIME_FORMAT,
            data,
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// One element of a partitioned relative-time pattern.
struct Part {
    kind: &'static str,
    value: String,
    /// Attached to numeric parts (`integer`, `group`, `decimal`, `fraction`).
    unit: Option<&'static str>,
}

fn js_relative_time_format_format(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let this = this_obj
        .payload_intl_relative_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let parts = partition_relative_time(ctx, this, cc.arg(0), cc.arg(1))?;
    let s: String = parts.iter().map(|p| p.value.as_str()).collect();
    Ok(make_str(ctx, s.into_bytes())?)
}

fn js_relative_time_format_format_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let this = this_obj
        .payload_intl_relative_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let parts = partition_relative_time(ctx, this, cc.arg(0), cc.arg(1))?;
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        let mut o = ctx.create_base_object()?;
        let t = make_str(ctx, p.kind.as_bytes().to_vec())?;
        o.set_property_str(ctx, "type", t)?;
        let v = make_str(ctx, p.value.into_bytes())?;
        o.set_property_str(ctx, "value", v)?;
        if let Some(u) = p.unit {
            let u = make_str(ctx, u.as_bytes().to_vec())?;
            o.set_property_str(ctx, "unit", u)?;
        }
        out.push(o.into());
    }
    Ok(values_to_array(ctx, out)?)
}

/// PartitionRelativeTimePattern: build the typed parts for `(value, unit)`.
fn partition_relative_time(
    ctx: &mut ScopedContext,
    this: &RelativeTimeFormatData,
    value: JSValue,
    unit_val: JSValue,
) -> Result<Vec<Part>, i32> {
    // FormatRelativeTime: ToNumber(value) happens before the unit handling.
    let n = value.to_number(ctx)?.to_f64();
    // SingularRelativeTimeUnit: plural forms map to their singular; anything
    // else is a RangeError. ToString runs first (symbols throw TypeError).
    let unit_raw = to_utf8_string(ctx, unit_val)?;
    let singular: &'static str = match unit_raw.as_str() {
        "second" | "seconds" => "second",
        "minute" | "minutes" => "minute",
        "hour" | "hours" => "hour",
        "day" | "days" => "day",
        "week" | "weeks" => "week",
        "month" | "months" => "month",
        "quarter" | "quarters" => "quarter",
        "year" | "years" => "year",
        _ => return Err(ctx.throw_range_error("Invalid unit argument")),
    };
    // Non-finite values are rejected after the unit validation.
    if !n.is_finite() {
        return Err(ctx.throw_range_error("value must be finite"));
    }

    // Numeric "auto" idioms (CLDR patterns for en; other locales fall back).
    let lang = this.locale.id.language.to_string();
    if matches!(this.numeric, RelNumeric::Auto)
        && n.fract() == 0.0
        && n.abs() <= 1.0
        && lang == "en"
    {
        let i = n as i64;
        let phrase = match (singular, i) {
            ("day", -1) => Some("yesterday"),
            ("day", 0) => Some("today"),
            ("day", 1) => Some("tomorrow"),
            ("week", -1) => Some("last week"),
            ("week", 0) => Some("this week"),
            ("week", 1) => Some("next week"),
            ("month", -1) => Some("last month"),
            ("month", 0) => Some("this month"),
            ("month", 1) => Some("next month"),
            ("quarter", -1) => Some("last quarter"),
            ("quarter", 0) => Some("this quarter"),
            ("quarter", 1) => Some("next quarter"),
            ("year", -1) => Some("last year"),
            ("year", 0) => Some("this year"),
            ("year", 1) => Some("next year"),
            ("hour", 0) => Some("this hour"),
            ("minute", 0) => Some("this minute"),
            ("second", 0) => Some("now"),
            _ => None,
        };
        if let Some(s) = phrase {
            return Ok(vec![Part {
                kind: "literal",
                value: s.to_string(),
                unit: None,
            }]);
        }
    }

    // -0 formats like a past value ("0 seconds ago").
    let past = n < 0.0 || (n == 0.0 && n.is_sign_negative());
    let abs = n.abs();
    let cat = plural_category(&lang, abs);
    let word = unit_word(this.style, &lang, singular, cat);
    let mut parts: Vec<Part> = Vec::new();
    let prefix: &str = if past {
        ""
    } else if lang == "en" {
        "in "
    } else {
        "za "
    };
    if !prefix.is_empty() {
        // An empty past-tense prefix emits no leading literal part.
        parts.push(Part {
            kind: "literal",
            value: prefix.to_string(),
            unit: None,
        });
    }
    parts.extend(render_number_parts(ctx, this, abs, singular)?);
    let suffix = if past && lang == "en" {
        format!(" {} ago", word)
    } else if past {
        format!(" {} temu", word)
    } else {
        format!(" {}", word)
    };
    parts.push(Part {
        kind: "literal",
        value: suffix,
        unit: None,
    });
    Ok(parts)
}

/// CLDR plural categories used here: en has one/other; pl adds few/many.
fn plural_category(lang: &str, n: f64) -> &'static str {
    if lang == "pl" {
        if n.fract() != 0.0 {
            return "other";
        }
        let i = n as i64;
        if i == 1 {
            return "one";
        }
        let m10 = i % 10;
        let m100 = i % 100;
        if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
            "few"
        } else {
            "many"
        }
    } else if n == 1.0 {
        "one"
    } else {
        "other"
    }
}

/// Localized unit word (CLDR en/pl data for long/short/narrow styles).
fn unit_word(style: RelStyle, lang: &str, unit: &str, cat: &str) -> String {
    if lang != "pl" {
        // en
        let one = cat == "one";
        return match (style, unit) {
            (RelStyle::Short, "second") => "sec.".into(),
            (RelStyle::Short, "minute") => "min.".into(),
            (RelStyle::Short, "hour") => "hr.".into(),
            (RelStyle::Short, "day") => (if one { "day" } else { "days" }).into(),
            (RelStyle::Short, "week") => "wk.".into(),
            (RelStyle::Short, "month") => "mo.".into(),
            (RelStyle::Short, "quarter") => (if one { "qtr." } else { "qtrs." }).into(),
            (RelStyle::Short, "year") => "yr.".into(),
            _ if one => unit.to_string(),
            _ => format!("{unit}s"),
        };
    }
    let word = match (style, unit, cat) {
        // long
        (RelStyle::Long, "second", "many") => "sekund",
        (RelStyle::Long, "second", "few") => "sekundy",
        (RelStyle::Long, "second", "one") => "sekundę",
        (RelStyle::Long, "second", _) => "sekundy",
        (RelStyle::Long, "minute", "many") => "minut",
        (RelStyle::Long, "minute", "few") => "minuty",
        (RelStyle::Long, "minute", "one") => "minutę",
        (RelStyle::Long, "minute", _) => "minuty",
        (RelStyle::Long, "hour", "many") => "godzin",
        (RelStyle::Long, "hour", "few") => "godziny",
        (RelStyle::Long, "hour", "one") => "godzinę",
        (RelStyle::Long, "hour", _) => "godziny",
        (RelStyle::Long, "day", "many" | "few") => "dni",
        (RelStyle::Long, "day", "one") => "dzień",
        (RelStyle::Long, "day", _) => "dnia",
        (RelStyle::Long, "week", "many") => "tygodni",
        (RelStyle::Long, "week", "few") => "tygodnie",
        (RelStyle::Long, "week", "one") => "tydzień",
        (RelStyle::Long, "week", _) => "tygodnia",
        (RelStyle::Long, "month", "many") => "miesięcy",
        (RelStyle::Long, "month", "few") => "miesiące",
        (RelStyle::Long, "month", "one") => "miesiąc",
        (RelStyle::Long, "month", _) => "miesiąca",
        (RelStyle::Long, "quarter", "many") => "kwartałów",
        (RelStyle::Long, "quarter", "few") => "kwartały",
        (RelStyle::Long, "quarter", "one") => "kwartał",
        (RelStyle::Long, "quarter", _) => "kwartału",
        (RelStyle::Long, "year", "many") => "lat",
        (RelStyle::Long, "year", "few") => "lata",
        (RelStyle::Long, "year", "one") => "rok",
        (RelStyle::Long, "year", _) => "roku",
        // short
        (RelStyle::Short, "second", _) => "sek.",
        (RelStyle::Short, "minute", _) => "min",
        (RelStyle::Short, "hour", _) => "godz.",
        (RelStyle::Short, "day", "many" | "few") => "dni",
        (RelStyle::Short, "day", "one") => "dzień",
        (RelStyle::Short, "day", _) => "dnia",
        (RelStyle::Short, "week", "one") => "tydz.",
        (RelStyle::Short, "week", _) => "tyg.",
        (RelStyle::Short, "month", _) => "mies.",
        (RelStyle::Short, "quarter", _) => "kw.",
        (RelStyle::Short, "year", "many") => "lat",
        (RelStyle::Short, "year", "few") => "lata",
        (RelStyle::Short, "year", "one") => "rok",
        (RelStyle::Short, "year", _) => "roku",
        // narrow
        (RelStyle::Narrow, "second", _) => "s",
        (RelStyle::Narrow, "minute", _) => "min",
        (RelStyle::Narrow, "hour", _) => "g.",
        (RelStyle::Narrow, "day", "many" | "few") => "dni",
        (RelStyle::Narrow, "day", "one") => "dzień",
        (RelStyle::Narrow, "day", _) => "dnia",
        (RelStyle::Narrow, "week", "one") => "tydz.",
        (RelStyle::Narrow, "week", _) => "tyg.",
        (RelStyle::Narrow, "month", _) => "mies.",
        (RelStyle::Narrow, "quarter", _) => "kw.",
        (RelStyle::Narrow, "year", "many") => "lat",
        (RelStyle::Narrow, "year", "few") => "lata",
        (RelStyle::Narrow, "year", "one") => "rok",
        (RelStyle::Narrow, "year", _) => "roku",
        _ => "",
    };
    word.to_string()
}

/// Render `abs` as numeric parts: `integer`/`group` segments plus optional
/// `decimal`/`fraction` tail, each carrying the unit. Grouping follows the
/// locale's separator; Polish (minimumGroupingDigits=2) leaves 4-digit
/// values ungrouped.
// ponytail: hardcoded 3-digit grouping + per-locale separators, extend when more locales need it
fn render_number_parts(
    ctx: &mut ScopedContext,
    this: &RelativeTimeFormatData,
    abs: f64,
    unit: &'static str,
) -> Result<Vec<Part>, i32> {
    let _ = ctx;
    let lang = this.locale.id.language.to_string();
    let (group_sep, dec_sep): (char, char) = if lang == "pl" {
        ('\u{a0}', ',')
    } else {
        (',', '.')
    };
    let min_group2 = lang == "pl";

    // Shortest exact representation; split into integer/fraction digit strings.
    let repr = super::number_format::format_shortest(abs);
    let (int_digits, frac_digits) = match repr.find('.') {
        Some(i) => (repr[..i].to_string(), repr[i + 1..].to_string()),
        None => (repr, String::new()),
    };

    let translate = |s: String| -> String {
        if this.numbering_system != "latn" {
            super::number_format::translate_digits(&s, &this.numbering_system)
        } else {
            s
        }
    };

    let chars: Vec<char> = int_digits.chars().collect();
    let group_len = chars.len();
    let use_grouping = group_len > 4 || (group_len == 4 && !min_group2);
    let mut parts = Vec::new();
    let mut chunk_start = 0usize;
    while chunk_start < group_len {
        let remaining = group_len - chunk_start;
        // Standard 3-digit grouping from the right.
        let take = if use_grouping && remaining > 3 {
            remaining - ((remaining - 1) / 3) * 3
        } else {
            remaining
        };
        let end = chunk_start + take;
        let seg: String = chars[chunk_start..end].iter().collect();
        parts.push(Part {
            kind: "integer",
            value: translate(seg),
            unit: Some(unit),
        });
        chunk_start = end;
        if chunk_start < group_len {
            parts.push(Part {
                kind: "group",
                value: group_sep.to_string(),
                unit: Some(unit),
            });
        }
    }
    if !frac_digits.is_empty() {
        parts.push(Part {
            kind: "decimal",
            value: translate(dec_sep.to_string()),
            unit: Some(unit),
        });
        parts.push(Part {
            kind: "fraction",
            value: translate(frac_digits),
            unit: Some(unit),
        });
    }
    Ok(parts)
}

fn js_relative_time_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let this = this_obj
        .payload_intl_relative_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let mut o = ctx.create_base_object()?;
    let _v3 = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", _v3, JS_PROP_C_W_E)?;
    let style = make_str(
        ctx,
        match this.style {
            RelStyle::Long => b"long".to_vec(),
            RelStyle::Short => b"short".to_vec(),
            RelStyle::Narrow => b"narrow".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "style", style, JS_PROP_C_W_E)?;
    let numeric = make_str(
        ctx,
        match this.numeric {
            RelNumeric::Always => b"always".to_vec(),
            RelNumeric::Auto => b"auto".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "numeric", numeric, JS_PROP_C_W_E)?;
    let ns = make_str(ctx, this.numbering_system.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "numberingSystem", ns, JS_PROP_C_W_E)?;
    Ok(o.into())
}

const js_relative_time_format_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("format", 2, js_relative_time_format_format, 0),
    FieldDef::FUNC(
        "formatToParts",
        2,
        js_relative_time_format_format_to_parts,
        0,
    ),
    FieldDef::FUNC(
        "resolvedOptions",
        0,
        js_relative_time_format_resolved_options,
        0,
    ),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.RelativeTimeFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
