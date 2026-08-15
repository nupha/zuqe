// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.NumberFormat` (§11).

use icu::{decimal::DecimalFormatter, locale::Locale};

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{
    locale_to_string, make_bound_method, make_str, resolve_locale, to_utf8_string,
};

#[derive(Debug)]
pub(crate) struct NumberFormatData {
    pub locale: Locale,
    pub formatter: DecimalFormatter,
    pub min_fraction_digits: i32,
    pub max_fraction_digits: i32,
    pub min_significant_digits: Option<u8>,
    pub max_significant_digits: Option<u8>,
    pub notation: Notation,
    pub style: Style,
    pub currency: Option<String>,
    pub use_grouping: bool,
}

impl GcTrace for NumberFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Debug)]
pub(crate) enum Notation {
    Standard,
    CompactShort,
    CompactLong,
    Scientific,
    Engineering,
}

#[derive(Debug)]
pub(crate) enum Style {
    Decimal,
    Percent,
    Currency,
    Unit,
}

pub(crate) static CLASS_DEF_NUMBER_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_NumberFormat,
    js_number_format_proto_funcs,
    Some(ClassCtorDef::new(js_number_format_constructor, 0, true)),
);

pub(crate) fn js_number_format_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let formatter = DecimalFormatter::try_new(locale.clone().into(), Default::default())
        .map_err(|_| ctx.throw_range_error("Unable to load locale data"))?;

    let mut data = NumberFormatData {
        locale,
        formatter,
        min_fraction_digits: 0,
        max_fraction_digits: 3,
        min_significant_digits: None,
        max_significant_digits: None,
        notation: Notation::Standard,
        style: Style::Decimal,
        currency: None,
        use_grouping: true,
    };

    if !options.is_undefined() {
        apply_options(ctx, &mut data, options)?;
    }

    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_NUMBER_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn apply_options(
    ctx: &mut ScopedContext,
    data: &mut NumberFormatData,
    options: JSValue,
) -> Result<(), i32> {
    let obj = options.objectify(ctx)?;

    // style: decimal | percent | currency | unit
    let style = obj.get_property_str(ctx, "style")?;
    if !style.is_undefined() {
        let s = to_utf8_string(ctx, style)?;
        data.style = match s.as_str() {
            "percent" => Style::Percent,
            "currency" => Style::Currency,
            "unit" => Style::Unit,
            _ => Style::Decimal,
        };
    }

    // currency
    let currency = obj.get_property_str(ctx, "currency")?;
    if !currency.is_undefined() {
        data.currency = Some(to_utf8_string(ctx, currency)?);
    }

    // notation
    let notation = obj.get_property_str(ctx, "notation")?;
    if !notation.is_undefined() {
        let s = to_utf8_string(ctx, notation)?;
        data.notation = match s.as_str() {
            "compact" => Notation::CompactShort,
            "engineering" => Notation::Engineering,
            "scientific" => Notation::Scientific,
            _ => Notation::Standard,
        };
    }

    // useGrouping
    let use_grouping = obj.get_property_str(ctx, "useGrouping")?;
    if !use_grouping.is_undefined() {
        data.use_grouping = use_grouping.to_bool()?;
    }

    // minimumFractionDigits / maximumFractionDigits
    let min_frac = obj.get_property_str(ctx, "minimumFractionDigits")?;
    let max_frac = obj.get_property_str(ctx, "maximumFractionDigits")?;
    if !max_frac.is_undefined() {
        data.max_fraction_digits = max_frac.to_number(ctx)?.to_f64() as i32;
    }
    if !min_frac.is_undefined() {
        data.min_fraction_digits = min_frac.to_number(ctx)?.to_f64() as i32;
        if data.max_fraction_digits < data.min_fraction_digits {
            data.max_fraction_digits = data.min_fraction_digits;
        }
    }
    // defaults based on style
    match data.style {
        Style::Percent => {
            if max_frac.is_undefined() {
                data.max_fraction_digits = 0;
            }
        }
        Style::Currency => {
            if max_frac.is_undefined() && min_frac.is_undefined() {
                data.max_fraction_digits = 2;
                data.min_fraction_digits = 2;
            }
        }
        _ => {}
    }

    // minimumSignificantDigits / maximumSignificantDigits
    let min_sig = obj.get_property_str(ctx, "minimumSignificantDigits")?;
    let max_sig = obj.get_property_str(ctx, "maximumSignificantDigits")?;
    if !min_sig.is_undefined() || !max_sig.is_undefined() {
        let min = if min_sig.is_undefined() {
            1
        } else {
            min_sig.to_number(ctx)?.to_f64() as u8
        };
        let max = if max_sig.is_undefined() {
            let min2 = min.max(1);
            // default max is min for min>1 else 21
            if min2 > 1 { min2 } else { 21 }
        } else {
            max_sig.to_number(ctx)?.to_f64() as u8
        };
        data.min_significant_digits = Some(min);
        data.max_significant_digits = Some(max);
        data.min_fraction_digits = 0;
        data.max_fraction_digits = 0;
    }

    Ok(())
}

/// Accessor getter for `NumberFormat.prototype.format`: returns a bound
/// function that formats with the receiver instance.
fn js_number_format_format_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    if !is_number_format(&this_val) {
        return Err(ctx.throw_type_error("not a NumberFormat"));
    }
    make_bound_method(ctx, this_val, js_number_format_format_bound, "format", 1)
}

fn is_number_format(v: &JSValue) -> bool {
    v.as_object()
        .is_some_and(|o| o.payload().intl_number_format().is_some())
}

fn js_number_format_format_bound(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = cc.data(0);
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let this = this_obj
        .payload()
        .intl_number_format()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let v = cc.arg(0);
    let s = format_value(ctx, this, v)?;
    Ok(make_str(ctx, s.into_bytes())?)
}

fn format_value(
    ctx: &mut ScopedContext,
    this: &NumberFormatData,
    v: JSValue,
) -> Result<String, i32> {
    // 11.1.6 FormatNumericValue: ToIntlMathematicalValue then rounding.
    let n = v.to_number(ctx)?.to_f64();

    if n.is_nan() {
        return Ok("NaN".to_string());
    }
    if n.is_infinite() {
        return Ok(if n > 0.0 { "∞" } else { "-∞" }.to_string());
    }

    // Percent style scales by 100.
    let scaled = match this.style {
        Style::Percent => n * 100.0,
        _ => n,
    };

    // Significant digits take precedence over fraction digits.
    if let (Some(min), Some(max)) = (this.min_significant_digits, this.max_significant_digits) {
        return format_with_significant(ctx, scaled, min, max, this.use_grouping);
    }

    // Round to fraction digits; pad to the *minimum* number of fraction digits.
    let rounded = round_to_fraction(scaled, this.min_fraction_digits, this.max_fraction_digits);
    let s = format_fixed(ctx, rounded, this.min_fraction_digits, this.use_grouping)?;

    // Currency/Unit/compact formatting fall back to plain numeric here.
    Ok(s)
}

fn round_to_fraction(n: f64, min: i32, max: i32) -> f64 {
    if max == 0 {
        return n.round();
    }
    let p = 10f64.powi(max);
    (n * p).round() / p
}

fn format_fixed(
    ctx: &mut ScopedContext,
    n: f64,
    frac: i32,
    use_grouping: bool,
) -> Result<String, i32> {
    if frac == 0 {
        let mut s = n.to_string();
        if !use_grouping {
            return Ok(s);
        }
        insert_grouping(&mut s);
        return Ok(s);
    }
    // format with fixed decimals.
    let mut s = format!("{:.*}", frac as usize, n);
    if !use_grouping {
        return Ok(s);
    }
    insert_grouping(&mut s);
    Ok(s)
}

fn insert_grouping(s: &mut String) {
    // Insert commas into the integer part (en-US default grouping).
    if let Some(dot) = s.find('.') {
        let int_part = &s[..dot];
        let mut grouped = String::new();
        let chars: Vec<char> = int_part.chars().collect();
        let neg = !chars.is_empty() && chars[0] == '-';
        let digits = if neg { &chars[1..] } else { &chars[..] };
        for (i, c) in digits.iter().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(*c);
        }
        if neg {
            *s = format!("-{}{}", grouped, &s[dot..]);
        } else {
            *s = format!("{}{}", grouped, &s[dot..]);
        }
    } else {
        let mut grouped = String::new();
        let chars: Vec<char> = s.chars().collect();
        let neg = !chars.is_empty() && chars[0] == '-';
        let digits = if neg { &chars[1..] } else { &chars[..] };
        for (i, c) in digits.iter().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                grouped.push(',');
            }
            grouped.push(*c);
        }
        *s = if neg {
            format!("-{}", grouped)
        } else {
            grouped
        };
    }
}

fn format_with_significant(
    ctx: &mut ScopedContext,
    n: f64,
    min: u8,
    max: u8,
    use_grouping: bool,
) -> Result<String, i32> {
    // Round n to `max` significant digits.
    if n == 0.0 {
        let s = format!("{:.*}", (min as usize).saturating_sub(1).min(20), 0.0);
        return Ok(s);
    }
    let exponent = n.abs().log10().floor() as i32;
    let scale = 10f64.powi(max as i32 - 1 - exponent);
    let rounded = (n * scale).round() / scale;
    // Determine fractional digits needed.
    let frac = (max as i32 - 1 - exponent).max(0);
    let s = format!("{:.*}", frac.min(20) as usize, rounded);
    Ok(s)
}

fn js_number_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let this = this_obj
        .payload()
        .intl_number_format()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &this.locale)?;
    o.set_property_str(ctx, "locale", _v1)?;
    let _v2 = make_str(ctx, b"latn".to_vec())?;
    o.set_property_str(ctx, "numberingSystem", _v2)?;
    let style = make_str(
        ctx,
        match this.style {
            Style::Decimal => b"decimal".to_vec(),
            Style::Percent => b"percent".to_vec(),
            Style::Currency => b"currency".to_vec(),
            Style::Unit => b"unit".to_vec(),
        },
    )?;
    o.set_property_str(ctx, "style", style)?;
    let notation = make_str(
        ctx,
        match this.notation {
            Notation::Standard => b"standard".to_vec(),
            Notation::CompactShort => b"compact".to_vec(),
            Notation::CompactLong => b"compact".to_vec(),
            Notation::Scientific => b"scientific".to_vec(),
            Notation::Engineering => b"engineering".to_vec(),
        },
    )?;
    o.set_property_str(ctx, "notation", notation)?;
    o.set_property_str(ctx, "useGrouping", JSValue::from(this.use_grouping))?;
    o.set_property_str(
        ctx,
        "minimumFractionDigits",
        JSValue::from(this.min_fraction_digits),
    )?;
    o.set_property_str(
        ctx,
        "maximumFractionDigits",
        JSValue::from(this.max_fraction_digits),
    )?;
    if let Some(min) = this.min_significant_digits {
        o.set_property_str(ctx, "minimumSignificantDigits", JSValue::from(min as i32))?;
    }
    if let Some(max) = this.max_significant_digits {
        o.set_property_str(ctx, "maximumSignificantDigits", JSValue::from(max as i32))?;
    }
    if let Some(c) = &this.currency {
        let _v3 = make_str(ctx, c.clone().into_bytes())?;
        o.set_property_str(ctx, "currency", _v3)?;
        let _v4 = make_str(ctx, b"symbol".to_vec())?;
        o.set_property_str(ctx, "currencyDisplay", _v4)?;
    }
    Ok(o.into())
}

const js_number_format_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("format", Some(js_number_format_format_getter), None, 0),
    FieldDef::FUNC("formatToParts", 1, js_number_format_format_bound, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_number_format_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.NumberFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
