// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.DurationFormat` (§15).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::{
    common::{locale_to_string, make_str, resolve_locale, to_utf8_string, values_to_array},
    number_format::is_valid_bcp47_type,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UStyle {
    Long,
    Short,
    Narrow,
    Numeric,
    TwoDigit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UDisplay {
    Auto,
    Always,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum BStyle {
    Long,
    Short,
    Narrow,
    Digital,
}

#[derive(Debug, Clone, Copy)]
struct UnitOpts {
    style: UStyle,
    display: UDisplay,
}

#[derive(Debug)]
pub struct DurationFormatData {
    pub(crate) locale: Locale,
    pub(crate) numbering_system: String,
    pub(crate) base_style: BStyle,
    // Table order: years..nanoseconds.
    units: [UnitOpts; 10],
    pub(crate) fractional_digits: Option<i32>,
}

impl GcTrace for DurationFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

const UNITS: [&str; 10] = [
    "years",
    "months",
    "weeks",
    "days",
    "hours",
    "minutes",
    "seconds",
    "milliseconds",
    "microseconds",
    "nanoseconds",
];

const SINGULAR: [&str; 10] = [
    "year",
    "month",
    "week",
    "day",
    "hour",
    "minute",
    "second",
    "millisecond",
    "microsecond",
    "nanosecond",
];

/// Per-unit allowed styles (Table 1): date-like units have no numeric styles;
/// ms/us/ns lack "2-digit".
fn unit_styles(idx: usize) -> &'static [&'static str] {
    match idx {
        0..=3 => &["long", "short", "narrow"],
        7..=9 => &["long", "short", "narrow", "numeric"],
        _ => &["long", "short", "narrow", "numeric", "2-digit"],
    }
}

fn ustyle_from_str(s: &str) -> UStyle {
    match s {
        "long" => UStyle::Long,
        "narrow" => UStyle::Narrow,
        "numeric" => UStyle::Numeric,
        "2-digit" => UStyle::TwoDigit,
        _ => UStyle::Short,
    }
}

fn ustyle_str(s: UStyle) -> &'static str {
    match s {
        UStyle::Long => "long",
        UStyle::Short => "short",
        UStyle::Narrow => "narrow",
        UStyle::Numeric => "numeric",
        UStyle::TwoDigit => "2-digit",
    }
}

fn bstyle_str(s: BStyle) -> &'static str {
    match s {
        BStyle::Long => "long",
        BStyle::Short => "short",
        BStyle::Narrow => "narrow",
        BStyle::Digital => "digital",
    }
}

pub(crate) static CLASS_DEF_DURATION_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_DurationFormat,
    js_duration_format_proto_funcs,
    Some(ClassCtorDef::new(js_duration_format_constructor, 0, true)),
);

/// `GetOption(options, key, "string", values, fallback)`.
fn get_option_string(
    ctx: &mut ScopedContext,
    obj: &crate::Object,
    key: &str,
    values: &[&str],
) -> Result<Option<String>, i32> {
    let v = obj.get_property_str(ctx, key)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let s = to_utf8_string(ctx, v)?;
    if !values.is_empty() && !values.contains(&s.as_str()) {
        return Err(ctx.throw_range_error("Invalid option value"));
    }
    Ok(Some(s))
}

/// GetNumberOption(options, "fractionalDigits", 0, 9) with an undefined
/// default.
fn get_fractional_digits(ctx: &mut ScopedContext, obj: &crate::Object) -> Result<Option<i32>, i32> {
    let v = obj.get_property_str(ctx, "fractionalDigits")?;
    if v.is_undefined() {
        return Ok(None);
    }
    let n = v.to_number(ctx)?.to_f64();
    if n.is_nan() || n < 0.0 || n > 9.0 {
        return Err(ctx.throw_range_error("Invalid fractionalDigits value"));
    }
    Ok(Some(n.floor() as i32))
}

/// GetDurationUnitOptions: resolve style/display for one unit and enforce the
/// numeric-run constraints (`prevStyle` is the style of the previous time
/// unit).
fn get_unit_options(
    ctx: &mut ScopedContext,
    obj: &crate::Object,
    idx: usize,
    base: BStyle,
    prev: Option<UStyle>,
) -> Result<(UStyle, UDisplay), i32> {
    let unit = UNITS[idx];
    let style_v = get_option_string(ctx, obj, unit, unit_styles(idx))?;

    let mut display_default = UDisplay::Always;
    let style = if let Some(s) = style_v {
        ustyle_from_str(&s)
    } else if base == BStyle::Digital {
        // Digital defaults: h/m/s numeric (always-displayed); date-like
        // units fall back to their "short" digital base, auto-displayed.
        if !(4..=6).contains(&idx) {
            display_default = UDisplay::Auto;
        }
        if idx <= 3 {
            UStyle::Short
        } else {
            UStyle::Numeric
        }
    } else if matches!(prev, Some(UStyle::Numeric) | Some(UStyle::TwoDigit)) {
        // Continue a numeric run; only minutes/seconds stay always-displayed.
        if !(5..=6).contains(&idx) {
            display_default = UDisplay::Auto;
        }
        UStyle::Numeric
    } else {
        display_default = UDisplay::Auto;
        match base {
            BStyle::Long => UStyle::Long,
            BStyle::Short => UStyle::Short,
            BStyle::Narrow => UStyle::Narrow,
            BStyle::Digital => unreachable!(),
        }
    };

    let display_v = get_option_string(ctx, obj, &format!("{}Display", unit), &["auto", "always"])?;
    let display = match display_v.as_deref() {
        Some("always") => UDisplay::Always,
        Some("auto") => UDisplay::Auto,
        _ => display_default,
    };

    // ValidateDurationUnitStyle + 2-digit forcing for minutes/seconds inside a
    // numeric run.
    if matches!(prev, Some(UStyle::Numeric) | Some(UStyle::TwoDigit)) {
        if !matches!(style, UStyle::Numeric | UStyle::TwoDigit) {
            return Err(ctx.throw_range_error("Invalid unit style after a numeric-styled unit"));
        }
    }
    let mut style = style;
    if matches!(prev, Some(UStyle::Numeric) | Some(UStyle::TwoDigit)) && (5..=6).contains(&idx) {
        style = UStyle::TwoDigit;
    }
    Ok((style, display))
}

fn js_duration_format_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    if new_target.is_undefined() {
        return Err(ctx.throw_type_error("Intl.DurationFormat must be called with 'new'"));
    }

    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);
    let has_options = !options.is_undefined();

    let data = if has_options {
        let obj = options.objectify(ctx)?;

        // Option read order is observable (constructor-options-order):
        // localeMatcher, numberingSystem, style, per-unit, fractionalDigits.
        get_option_string(ctx, &obj, "localeMatcher", &["lookup", "best fit"])?;

        // numberingSystem must match the BCP47 `type` nonterminal.
        let numbering_opt = get_option_string(ctx, &obj, "numberingSystem", &[])?;
        if let Some(s) = &numbering_opt {
            if !is_valid_bcp47_type(s) {
                return Err(ctx.throw_range_error("Invalid numberingSystem value"));
            }
        }

        let base_style =
            match get_option_string(ctx, &obj, "style", &["long", "short", "narrow", "digital"])? {
                Some(s) => match s.as_str() {
                    "long" => BStyle::Long,
                    "narrow" => BStyle::Narrow,
                    "digital" => BStyle::Digital,
                    _ => BStyle::Short,
                },
                None => BStyle::Short,
            };

        let mut units = [UnitOpts {
            style: UStyle::Short,
            display: UDisplay::Auto,
        }; 10];
        // prevStyle tracks hours..microseconds (the units that can start or
        // continue a numeric run).
        let mut prev: Option<UStyle> = None;
        for idx in 0..10 {
            let (style, display) = get_unit_options(ctx, &obj, idx, base_style, prev)?;
            units[idx] = UnitOpts { style, display };
            if (4..=8).contains(&idx) {
                prev = Some(style);
            }
        }

        let fractional_digits = get_fractional_digits(ctx, &obj)?;

        DurationFormatData {
            locale,
            numbering_system: "latn".to_string(),
            base_style,
            units,
            fractional_digits,
        }
        .with_numbering_system(numbering_opt.as_deref())
    } else {
        DurationFormatData {
            locale,
            numbering_system: "latn".to_string(),
            base_style: BStyle::Short,
            units: [UnitOpts {
                style: UStyle::Short,
                display: UDisplay::Auto,
            }; 10],
            fractional_digits: None,
        }
        .with_numbering_system(None)
    };

    let instance = ctx
        .ordinary_create_from_constructor(
            new_target,
            ObjectPayload::INTL_DURATION_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

impl DurationFormatData {
    /// Resolve the effective numbering system: explicit valid option >
    /// `nu` extension > latn.
    fn with_numbering_system(mut self, ns_opt: Option<&str>) -> Self {
        let nu_static: Option<&'static str> =
            ns_opt.map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
        self.locale = super::common::resolve_locale_keys(self.locale, &[("nu", nu_static)]);
        let nu_ext = super::common::locale_extension_value(&self.locale, "nu");
        self.numbering_system = if let Some(opt) = nu_static {
            if super::common::supported_extension_key_value("nu", opt) {
                opt.to_string()
            } else if let Some(ext) = nu_ext {
                ext
            } else {
                "latn".to_string()
            }
        } else if let Some(ext) = nu_ext {
            ext
        } else {
            "latn".to_string()
        };
        self
    }
}

fn js_duration_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let this = this_obj
        .payload()
        .intl_duration_format()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let mut o = ctx.create_base_object()?;
    let loc = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", loc, JS_PROP_C_W_E)?;
    let ns = make_str(ctx, this.numbering_system.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "numberingSystem", ns, JS_PROP_C_W_E)?;
    let st = make_str(ctx, bstyle_str(this.base_style).as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "style", st, JS_PROP_C_W_E)?;
    for (idx, unit) in UNITS.iter().enumerate() {
        let s = make_str(ctx, ustyle_str(this.units[idx].style).as_bytes().to_vec())?;
        o.define_property_value_str(ctx, unit, s, JS_PROP_C_W_E)?;
        let d = make_str(
            ctx,
            match this.units[idx].display {
                UDisplay::Always => "always",
                UDisplay::Auto => "auto",
            }
            .as_bytes()
            .to_vec(),
        )?;
        o.define_property_value_str(ctx, &format!("{}Display", unit), d, JS_PROP_C_W_E)?;
    }
    if let Some(fd) = this.fractional_digits {
        o.define_property_value_str(ctx, "fractionalDigits", JSValue::from(fd), JS_PROP_C_W_E)?;
    }
    Ok(o.into())
}

/// A partitioned format part: `"literal"` separators carry no unit; number
/// parts are emitted as a single `"integer"` part (matching NumberFormat's
/// merged part output) tagged with the singular unit name.
struct Part {
    typ: &'static str,
    value: String,
    unit: Option<&'static str>,
}

/// ToDurationRecord: read the ten duration fields as integral Numbers and
/// validate sign consistency and magnitude bounds.
fn to_duration_record(ctx: &mut ScopedContext, input: JSValue) -> Result<[f64; 10], i32> {
    let Some(obj) = input.as_object() else {
        if input.is_string() {
            return Err(ctx.throw_range_error("invalid duration string"));
        }
        return Err(ctx.throw_type_error("duration must be an object"));
    };
    // Alphabetical field order per spec; map to table indices years..ns.
    const FIELD_ORDER: [usize; 10] = [3, 4, 8, 7, 5, 1, 9, 6, 2, 0];
    const FIELDS: [&str; 10] = [
        "days",
        "hours",
        "microseconds",
        "milliseconds",
        "minutes",
        "months",
        "nanoseconds",
        "seconds",
        "weeks",
        "years",
    ];
    let mut rec = [0.0f64; 10];
    let mut any = false;
    for (field, idx) in FIELDS.iter().zip(FIELD_ORDER.iter()) {
        let v = obj.get_property_str(ctx, field)?;
        if v.is_undefined() {
            continue;
        }
        any = true;
        let n = v.to_number(ctx)?.to_f64();
        if !n.is_finite() || n != n.trunc() {
            return Err(ctx.throw_range_error("invalid duration field"));
        }
        rec[*idx] = n;
    }
    if !any {
        return Err(ctx.throw_type_error("invalid duration"));
    }
    if !is_valid_duration(rec) {
        return Err(ctx.throw_range_error("invalid duration"));
    }
    Ok(rec)
}

/// IsValidDuration: consistent signs, y/m/w below 2^32, and normalized time
/// below 2^53 seconds (computed exactly via integer nanoseconds).
fn is_valid_duration(rec: [f64; 10]) -> bool {
    // Sign of the most significant non-zero field.
    let mut sign = 0i32;
    for &v in rec.iter() {
        if !v.is_finite() {
            return false;
        }
        if v < 0.0 {
            if sign > 0 {
                return false;
            }
            sign = -1;
        } else if v > 0.0 {
            if sign < 0 {
                return false;
            }
            sign = 1;
        }
    }
    for idx in 0..3 {
        if rec[idx].abs() >= 4_294_967_296.0 {
            return false;
        }
    }
    // Time fields (days..nanoseconds) with their nanosecond multipliers.
    const TIME_IDX: [usize; 7] = [3, 4, 5, 6, 7, 8, 9];
    const TIME_MULT: [i128; 7] = [
        86_400_000_000_000,
        3_600_000_000_000,
        60_000_000_000,
        1_000_000_000,
        1_000_000,
        1_000,
        1,
    ];
    // Exact integer check (no f64 rounding at the 2^53-second boundary):
    // reject any field whose scaled magnitude reaches the limit, accumulate
    // the rest exactly, then compare the total. `f64 as i128` is exact for
    // integral f64 values within range and saturates beyond it (which the
    // per-field comparison then rejects anyway).
    const LIMIT_NS: i128 = 9_007_199_254_740_992_000_000_000; // 2^53 * 1e9
    let mut total_ns: i128 = 0;
    for (k, &idx) in TIME_IDX.iter().enumerate() {
        let m = TIME_MULT[k];
        let avi = rec[idx].abs() as i128;
        if avi.saturating_mul(m) >= LIMIT_NS {
            return false;
        }
        total_ns += avi * m;
    }
    total_ns.abs() < LIMIT_NS
}

fn duration_sign(rec: [f64; 10]) -> i32 {
    for &v in rec.iter() {
        if v < 0.0 {
            return -1;
        }
        if v > 0.0 {
            return 1;
        }
    }
    0
}

/// Exact combined value at a fractional exponent (9/6/3 for the
/// seconds/milliseconds/microseconds level): sum the remaining sub-second
/// fields into an exact decimal string, then round once to f64.
fn combined_fractional(rec: &[f64; 10], level_idx: usize) -> f64 {
    // level_idx: 6=seconds(exp 9), 7=milliseconds(exp 6), 8=microseconds(exp 3).
    let exp: u32 = match level_idx {
        6 => 9,
        7 => 6,
        _ => 3,
    };
    let mults: [i128; 4] = [1_000_000_000, 1_000_000, 1_000, 1];
    let mut total: i128 = 0;
    for k in (level_idx - 6)..4 {
        total += rec[6 + k] as i128 * mults[k];
    }
    let div = 10i128.pow(exp);
    let q = total / div;
    let mut r = total % div;
    if r < 0 {
        r = -r;
    }
    format!("{}.{:0width$}", q, r, width = exp as usize)
        .parse::<f64>()
        .unwrap_or(0.0)
}

/// PartitionDurationFormatPattern (test262 revision: no "fractional" style;
/// sub-second numeric fields merge upward into the preceding unit).
fn partition(
    ctx: &mut ScopedContext,
    df: &DurationFormatData,
    rec: &[f64; 10],
) -> Result<Vec<Part>, i32> {
    let mut groups: Vec<Vec<Part>> = Vec::new();
    let mut need_sep = false;
    let mut sign_shown = true;

    for idx in 0..10 {
        let mut value = rec[idx];
        let opts = df.units[idx];
        let mut done = false;
        let mut min_frac = 0i32;
        let mut max_frac = 3i32;
        let mut trunc = false;

        // Numeric sub-second units merge into seconds/milliseconds/
        // microseconds with truncation and fraction digit options.
        if (6..=8).contains(&idx) && df.units[idx + 1].style == UStyle::Numeric {
            value = combined_fractional(rec, idx);
            min_frac = df.fractional_digits.unwrap_or(0);
            max_frac = df.fractional_digits.unwrap_or(9);
            trunc = true;
            done = true;
        }

        // Display zero minutes when a following seconds part will show.
        let display_required = idx == 5
            && need_sep
            && (df.units[6].display == UDisplay::Always || rec[6..10].iter().any(|&v| v != 0.0));

        if value != 0.0 || opts.display != UDisplay::Auto || display_required {
            let sign_never = !sign_shown;
            if value == 0.0 {
                // Normalize negative zero; the first displayed unit of a
                // negative duration re-applies it so auto shows "-0".
                value = 0.0;
                if sign_shown && duration_sign(*rec) < 0 {
                    value = -0.0;
                }
                sign_shown = false;
            } else if sign_shown {
                sign_shown = false;
            }
            let numeric_like = matches!(opts.style, UStyle::Numeric | UStyle::TwoDigit);
            let nf = super::number_format::build_nf_data(
                ctx,
                &df.locale,
                &df.numbering_system,
                if opts.style == UStyle::TwoDigit { 2 } else { 1 },
                min_frac,
                max_frac,
                sign_never,
                // Numeric runs disable grouping; standalone units keep it.
                !numeric_like,
                trunc,
                if numeric_like {
                    None
                } else {
                    let disp = match opts.style {
                        UStyle::Long => "long",
                        UStyle::Short => "short",
                        _ => "narrow",
                    };
                    Some((SINGULAR[idx], disp))
                },
            )?;
            // Mirror NumberFormat.formatToParts: numeric values may split
            // into minusSign/integer/decimal/fraction parts; standalone units
            // add a trailing unit part.
            let nparts = super::number_format::format_parts(ctx, &nf, value)?;
            if need_sep {
                groups.last_mut().unwrap().push(Part {
                    typ: "literal",
                    value: ":".to_string(),
                    unit: None,
                });
            } else {
                if numeric_like {
                    need_sep = true;
                }
                groups.push(Vec::new());
            }
            let target = groups.last_mut().unwrap();
            for np in nparts {
                target.push(Part {
                    typ: np.typ,
                    value: np.value,
                    unit: Some(SINGULAR[idx]),
                });
            }
        }
        if done {
            break;
        }
    }

    // Join the displayed unit groups like ListFormat(type "unit"), including
    // the narrow-style " & " before the last element.
    let list_style = match df.base_style {
        BStyle::Digital => super::list_format::ListStyle::Short,
        BStyle::Long => super::list_format::ListStyle::Long,
        BStyle::Narrow => super::list_format::ListStyle::Narrow,
        BStyle::Short => super::list_format::ListStyle::Short,
    };
    let mut out: Vec<Part> = Vec::new();
    let n = groups.len();
    let list_lang = df.locale.id.language.to_string();
    for (gi, g) in groups.into_iter().enumerate() {
        if gi > 0 {
            out.push(Part {
                typ: "literal",
                value: super::list_format::join_separator(
                    &list_lang,
                    list_style,
                    super::list_format::ListType::Unit,
                    gi,
                    n,
                )
                .to_string(),
                unit: None,
            });
        }
        out.extend(g);
    }
    Ok(out)
}

fn js_duration_format_format(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let this = this_obj
        .payload()
        .intl_duration_format()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let rec = to_duration_record(ctx, cc.arg(0))?;
    let parts = partition(ctx, this, &rec)?;
    let s: String = parts.into_iter().map(|p| p.value).collect();
    make_str(ctx, s.into_bytes())
}

fn js_duration_format_format_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let this = this_obj
        .payload()
        .intl_duration_format()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let rec = to_duration_record(ctx, cc.arg(0))?;
    let parts = partition(ctx, this, &rec)?;
    let mut vals: Vec<JSValue> = Vec::with_capacity(parts.len());
    for p in parts {
        let mut o = ctx.create_base_object()?;
        let t = make_str(ctx, p.typ.as_bytes().to_vec())?;
        o.set_property_str(ctx, "type", t)?;
        let v = make_str(ctx, p.value.into_bytes())?;
        o.set_property_str(ctx, "value", v)?;
        if let Some(u) = p.unit {
            let us = make_str(ctx, u.as_bytes().to_vec())?;
            o.set_property_str(ctx, "unit", us)?;
        }
        vals.push(o.into());
    }
    Ok(values_to_array(ctx, vals)?.into())
}

const js_duration_format_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("format", 1, js_duration_format_format, 0),
    FieldDef::FUNC("formatToParts", 1, js_duration_format_format_to_parts, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_duration_format_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.DurationFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
