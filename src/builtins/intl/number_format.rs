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
    locale_to_string, make_bound_method, make_str, resolve_locale, to_utf8_string, values_to_array,
};

#[derive(Debug)]
pub(crate) struct NumberFormatData {
    pub locale: Locale,
    pub formatter: DecimalFormatter,
    pub min_fraction_digits: i32,
    pub max_fraction_digits: i32,
    pub min_significant_digits: Option<u8>,
    pub max_significant_digits: Option<u8>,
    pub min_integer_digits: i32,
    pub notation: Notation,
    pub style: Style,
    pub currency: Option<String>,
    pub currency_display: &'static str,
    pub use_grouping: bool,
    pub grouping_sep: char,
    pub decimal_sep: char,
    pub percent_space: bool,
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

/// Validate a BCP-47 `type`-like value: `3*8alphanum *("-" (3*8alphanum))`.
pub(crate) fn is_valid_bcp47_type(s: &str) -> bool {
    for part in s.split('-') {
        let len = part.len();
        if !(3..=8).contains(&len) || !part.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return false;
        }
    }
    true
}

/// Best-effort locale-aware grouping, decimal and percent spacing separators
/// (CLDR-derived for common locales; fall back to en-US style).
fn locale_separators(locale: &Locale) -> (char, char, bool) {
    let lang = locale.id.language.to_string();
    // `percent_space` = whether a (no-break) space precedes the percent sign.
    match lang.as_str() {
        "de" => ('.', ',', true),
        "fr" => ('\u{202f}', ',', true),
        "en" | "th" | "ja" | "es" | "it" | "pt" | "nl" | "sv" | "da" | "nb" | "fi" | "ko"
        | "zh" => (',', '.', false),
        "ar" => ('٬', '٫', true),
        "hi" => (',', '.', false),
        _ => (',', '.', false),
    }
}

/// Build a `NumberFormatData` from `(locales, options)`, used by both the
/// `Intl.NumberFormat` constructor and `Number.prototype.toLocaleString`.
pub(crate) fn build_number_format_data(
    ctx: &mut ScopedContext,
    locales: JSValue,
    options: JSValue,
) -> Result<NumberFormatData, i32> {
    let locale = resolve_locale(ctx, locales)?;

    let formatter = DecimalFormatter::try_new(locale.clone().into(), Default::default())
        .map_err(|_| ctx.throw_range_error("Unable to load locale data"))?;

    // Locale-aware grouping/decimal separators (best-effort; CLDR-derived).
    let (grouping_sep, decimal_sep, percent_space) = locale_separators(&locale);

    let mut data = NumberFormatData {
        locale,
        formatter,
        min_fraction_digits: 0,
        max_fraction_digits: 3,
        min_significant_digits: None,
        max_significant_digits: None,
        min_integer_digits: 1,
        notation: Notation::Standard,
        style: Style::Decimal,
        currency: None,
        currency_display: "symbol",
        use_grouping: true,
        grouping_sep,
        decimal_sep,
        percent_space,
    };

    if !options.is_undefined() {
        apply_options(ctx, &mut data, options)?;
    }
    Ok(data)
}

pub(crate) fn js_number_format_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let data = build_number_format_data(ctx, cc.arg(0), cc.arg(1))?;

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

    // localeMatcher: lookup | best fit
    let matcher = obj.get_property_str(ctx, "localeMatcher")?;
    if !matcher.is_undefined() {
        let s = to_utf8_string(ctx, matcher)?;
        if s != "lookup" && s != "best fit" {
            return Err(ctx.throw_range_error("Invalid localeMatcher value"));
        }
    }

    // numberingSystem: must match the `type` BCP47 nonterminal
    // (3*8alphanum *("-" (3*8alphanum))).
    let ns = obj.get_property_str(ctx, "numberingSystem")?;
    if !ns.is_undefined() {
        let s = to_utf8_string(ctx, ns)?;
        if !is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid numberingSystem value"));
        }
    }

    // style: decimal | percent | currency | unit
    let style = obj.get_property_str(ctx, "style")?;
    if !style.is_undefined() {
        let s = to_utf8_string(ctx, style)?;
        data.style = match s.as_str() {
            "decimal" => Style::Decimal,
            "percent" => Style::Percent,
            "currency" => Style::Currency,
            "unit" => Style::Unit,
            _ => return Err(ctx.throw_range_error("Invalid style value")),
        };
    }

    // currency (must be present and a valid 3-letter code for currency style)
    let currency = obj.get_property_str(ctx, "currency")?;
    let currency_str = if currency.is_undefined() {
        None
    } else {
        let s = to_utf8_string(ctx, currency)?;
        // Per ECMA-402, an invalid currency code throws RangeError.
        let code = s.to_ascii_uppercase();
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(ctx.throw_range_error("Invalid currency code"));
        }
        Some(code)
    };
    if matches!(data.style, Style::Currency) {
        data.currency = currency_str.clone();
        if currency_str.is_none() {
            return Err(ctx.throw_type_error("Currency style requires a currency"));
        }
    } else {
        // currency is only exposed for currency style.
        data.currency = None;
    }

    // currencyDisplay: symbol | code | name | narrowSymbol
    let currency_display = obj.get_property_str(ctx, "currencyDisplay")?;
    if !currency_display.is_undefined() {
        let s = to_utf8_string(ctx, currency_display)?;
        match s.as_str() {
            "symbol" => data.currency_display = "symbol",
            "code" => data.currency_display = "code",
            "name" => data.currency_display = "name",
            "narrowSymbol" => data.currency_display = "narrowSymbol",
            _ => return Err(ctx.throw_range_error("Invalid currencyDisplay value")),
        }
    }

    // minimumIntegerDigits: integer in [1, 21].
    let min_int = obj.get_property_str(ctx, "minimumIntegerDigits")?;
    if !min_int.is_undefined() {
        let m = min_int.to_number(ctx)?.to_f64();
        if m.is_nan() || m < 1.0 || m > 21.0 {
            return Err(ctx.throw_range_error("Invalid minimumIntegerDigits"));
        }
        data.min_integer_digits = m as i32;
    }

    // notation: standard | compact | scientific | engineering
    let notation = obj.get_property_str(ctx, "notation")?;
    if !notation.is_undefined() {
        let s = to_utf8_string(ctx, notation)?;
        data.notation = match s.as_str() {
            "standard" => Notation::Standard,
            "compact" => Notation::CompactShort,
            "engineering" => Notation::Engineering,
            "scientific" => Notation::Scientific,
            _ => return Err(ctx.throw_range_error("Invalid notation value")),
        };
    }

    // useGrouping
    let use_grouping = obj.get_property_str(ctx, "useGrouping")?;
    if !use_grouping.is_undefined() {
        data.use_grouping = use_grouping.to_bool()?;
    }

    // minimumFractionDigits / maximumFractionDigits (each in [0, 100]).
    let min_frac = obj.get_property_str(ctx, "minimumFractionDigits")?;
    let max_frac = obj.get_property_str(ctx, "maximumFractionDigits")?;
    if !max_frac.is_undefined() {
        let m = max_frac.to_number(ctx)?.to_f64();
        if m.is_nan() || m < 0.0 || m > 100.0 {
            return Err(ctx.throw_range_error("Invalid maximumFractionDigits"));
        }
        data.max_fraction_digits = m as i32;
    }
    if !min_frac.is_undefined() {
        let m = min_frac.to_number(ctx)?.to_f64();
        if m.is_nan() || m < 0.0 || m > 100.0 {
            return Err(ctx.throw_range_error("Invalid minimumFractionDigits"));
        }
        data.min_fraction_digits = m as i32;
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
            let m = min_sig.to_number(ctx)?.to_f64();
            if !(1.0..=21.0).contains(&m) {
                return Err(ctx.throw_range_error("Invalid significant digits"));
            }
            m as u8
        };
        let max = if max_sig.is_undefined() {
            let min2 = min.max(1);
            // default max is min for min>1 else 21
            if min2 > 1 { min2 } else { 21 }
        } else {
            let mx = max_sig.to_number(ctx)?.to_f64();
            if !(1.0..=21.0).contains(&mx) {
                return Err(ctx.throw_range_error("Invalid significant digits"));
            }
            mx as u8
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
    // The bound `format` function is anonymous (name is the empty string).
    make_bound_method(ctx, this_val, js_number_format_format_bound, "", 1)
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

/// Format an integer value given as a decimal digit string (no sign, no
/// leading zeros) using the NumberFormat options, with exact integer arithmetic.
/// `negative` selects the leading sign.
/// Round an integer digit string (no sign) to `sig` significant digits, half-up.
fn round_significant(digits: &str, sig: usize) -> String {
    if sig >= digits.len() {
        return digits.to_string();
    }
    let keep = &digits[..sig];
    let next = digits.as_bytes()[sig];
    let mut keep = keep.to_string();
    if next >= b'5' {
        // Increment the kept digits (may carry, e.g. 9999 -> 10000).
        let chars: Vec<u8> = keep.bytes().collect();
        let mut result = Vec::with_capacity(chars.len());
        let mut carry = 1;
        for &c in chars.iter().rev() {
            if carry == 0 {
                result.push(c);
            } else if c == b'9' {
                result.push(b'0');
            } else {
                result.push(c + 1);
                carry = 0;
            }
        }
        if carry == 1 {
            result.push(b'1');
        }
        result.reverse();
        keep = String::from_utf8(result).unwrap();
    }
    // Pad back to the original magnitude with zeros.
    keep.push_str(&"0".repeat(digits.len() - sig));
    keep
}

fn format_bigint_digits(
    _ctx: &mut ScopedContext,
    this: &NumberFormatData,
    negative: bool,
    digits: &str,
) -> Result<String, i32> {
    // Apply a power-of-ten scale for percent (×100). BigInts are integers, so
    // the scaled value is still an integer.
    let scaled_digits = match this.style {
        Style::Percent => format!("{}{}", digits, "00"),
        _ => digits.to_string(),
    };

    let mut digits = scaled_digits.trim_start_matches('0').to_string();
    if digits.is_empty() {
        digits = "0".to_string();
    }
    // Round to the significant-digit count when significant digits are set
    // (BigInt-exact, half-up).
    if let (Some(min_sig), Some(max_sig)) =
        (this.min_significant_digits, this.max_significant_digits)
    {
        let sig = max_sig.max(min_sig) as usize;
        digits = round_significant(&digits, sig);
        // Pad to at least `min_sig` significant digits with trailing zeros.
        if digits.len() < min_sig as usize {
            digits.push_str(&"0".repeat(min_sig as usize - digits.len()));
        }
    }
    let mut int_part = if digits.is_empty() {
        "0".to_string()
    } else {
        digits
    };

    // Pad to the minimum integer digits.
    let min_int = this.min_integer_digits.max(1) as usize;
    if int_part.len() < min_int {
        int_part = format!("{}{}", "0".repeat(min_int - int_part.len()), int_part);
    }

    // Fraction digits only arise from `minimumFractionDigits` padding, since a
    // BigInt input is an exact integer (NumberFormat rounding never introduces
    // a fractional remainder for an integer value).
    let min_frac = this.min_fraction_digits.max(0) as usize;

    let mut s = String::new();
    if negative {
        s.push('-');
    }
    if this.use_grouping {
        // Group integer part with the locale grouping separator.
        let chars: Vec<char> = int_part.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && (chars.len() - i) % 3 == 0 {
                s.push(this.grouping_sep);
            }
            s.push(*c);
        }
    } else {
        s.push_str(&int_part);
    }
    if min_frac > 0 {
        s.push(this.decimal_sep);
        s.push_str(&"0".repeat(min_frac));
    }
    Ok(s)
}

pub(crate) fn format_value(
    ctx: &mut ScopedContext,
    this: &NumberFormatData,
    v: JSValue,
) -> Result<String, i32> {
    // BigInt inputs must be formatted exactly (no f64 conversion).
    if v.is_bigint() {
        let bi = v.as_bigint().unwrap();
        let negative = bi.as_impl().sign();
        let digits = crate::bigint::to_string_radix(&bi, 10);
        let digits = digits.trim_start_matches('-');
        let mut s = format_bigint_digits(ctx, this, negative, digits)?;
        append_percent_suffix(this, &mut s);
        return Ok(s);
    }

    // 11.1.6 FormatNumericValue: ToIntlMathematicalValue then rounding.
    let n = v.to_number(ctx)?.to_f64();
    format_f64(ctx, this, n)
}

/// Format an already-converted `f64` value (no further coercion).
fn format_f64(ctx: &mut ScopedContext, this: &NumberFormatData, n: f64) -> Result<String, i32> {
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
    let mut s = if let (Some(min), Some(max)) =
        (this.min_significant_digits, this.max_significant_digits)
    {
        format_with_significant(
            ctx,
            scaled,
            min,
            max,
            this.use_grouping,
            this.grouping_sep,
            this.decimal_sep,
        )?
    } else {
        // Round to fraction digits; pad to the *minimum* fraction digits.
        let rounded = round_to_fraction(scaled, this.min_fraction_digits, this.max_fraction_digits);
        format_fixed(
            ctx,
            rounded,
            this.min_fraction_digits,
            this.max_fraction_digits,
            this.use_grouping,
            this.grouping_sep,
            this.decimal_sep,
            this.min_integer_digits,
        )?
    };

    append_percent_suffix(this, &mut s);
    Ok(s)
}

/// Append the percent sign (with locale-appropriate spacing) for percent style.
fn append_percent_suffix(this: &NumberFormatData, s: &mut String) {
    if matches!(this.style, Style::Percent) {
        if this.percent_space {
            s.push('\u{00a0}');
        }
        s.push('%');
    }
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
    min_frac: i32,
    max_frac: i32,
    use_grouping: bool,
    grouping_sep: char,
    decimal_sep: char,
    min_int: i32,
) -> Result<String, i32> {
    // Format with the *maximum* fraction digits, then trim trailing zeros down
    // to the *minimum*.
    let f = if max_frac == 0 {
        n.to_string()
    } else {
        format!("{:.*}", max_frac as usize, n)
    };
    // Split into sign/integer/fraction at the (single, ASCII) decimal point.
    let (sign, int_str, frac_str) = match f.find('.') {
        Some(dot) => {
            let int_part = &f[..dot];
            let (sg, int_digits) = if let Some(stripped) = int_part.strip_prefix('-') {
                ("-", stripped)
            } else {
                ("", int_part)
            };
            (sg, int_digits.to_string(), f[dot + 1..].to_string())
        }
        None => {
            let (sg, int_digits) = if let Some(stripped) = f.strip_prefix('-') {
                ("-", stripped)
            } else {
                ("", f.as_str())
            };
            (sg, int_digits.to_string(), String::new())
        }
    };

    // Trim trailing zeros of the fraction down to `min_frac`.
    let nz = frac_str.trim_end_matches('0');
    let keep = if min_frac == 0 && nz.is_empty() {
        0
    } else {
        nz.len().max(min_frac as usize)
    };

    // Pad the integer part to the minimum integer digits.
    let mut int_part = int_str;
    if int_part.len() < min_int as usize {
        int_part = format!(
            "{}{}",
            "0".repeat(min_int as usize - int_part.len()),
            int_part
        );
    }
    if use_grouping {
        int_part = group_integer(&int_part, grouping_sep);
    }

    let mut out = String::new();
    out.push_str(sign);
    out.push_str(&int_part);
    if keep > 0 {
        out.push(decimal_sep);
        out.push_str(&frac_str[..keep]);
    }
    Ok(out)
}

/// Pad the integer part of a plain number string to at least `min_int` digits.
fn pad_min_integer(s: &mut String, min_int: i32) {
    if min_int <= 1 {
        return;
    }
    let neg = s.starts_with('-');
    let digits = if neg { &s[1..] } else { s.as_str() };
    if digits.len() < min_int as usize {
        let padding = "0".repeat(min_int as usize - digits.len());
        let new = if neg {
            format!("-{}{}", padding, digits)
        } else {
            format!("{}{}", padding, digits)
        };
        *s = new;
    }
}

fn insert_grouping(s: &mut String, grouping_sep: char) {
    // Insert grouping separators into the integer part (locale-aware).
    if let Some(dot) = s.find('.') {
        let int_part = &s[..dot];
        let mut grouped = String::new();
        let chars: Vec<char> = int_part.chars().collect();
        let neg = !chars.is_empty() && chars[0] == '-';
        let digits = if neg { &chars[1..] } else { &chars[..] };
        for (i, c) in digits.iter().enumerate() {
            if i > 0 && (digits.len() - i) % 3 == 0 {
                grouped.push(grouping_sep);
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
                grouped.push(grouping_sep);
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

/// Group a sign-less digit string with `grouping_sep` every three digits.
fn group_integer(digits: &str, grouping_sep: char) -> String {
    let chars: Vec<char> = digits.chars().collect();
    let mut grouped = String::new();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            grouped.push(grouping_sep);
        }
        grouped.push(*c);
    }
    grouped
}

fn format_with_significant(
    ctx: &mut ScopedContext,
    n: f64,
    min: u8,
    max: u8,
    use_grouping: bool,
    grouping_sep: char,
    decimal_sep: char,
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
    let mut s = format!("{:.*}", frac.min(20) as usize, rounded);
    if use_grouping {
        insert_grouping(&mut s, grouping_sep);
    }
    if decimal_sep != '.' {
        if let Some(i) = s.rfind('.') {
            s.replace_range(i..=i, &decimal_sep.to_string());
        }
    }
    Ok(s)
}

/// `Intl.NumberFormat.prototype.formatToParts(value)`: returns an array of
/// `{ type, value }` parts that concatenate to the formatted result.
fn js_number_format_formatToParts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let this = this_obj
        .payload()
        .intl_number_format()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;

    let value = cc.arg(0);
    // ToNumber must be applied exactly once (per spec step 4).
    let (formatted, part_type): (String, &str) = if value.is_bigint() {
        (format_value(ctx, this, value)?, "integer")
    } else {
        let n = value.to_number(ctx)?.to_f64();
        let ptype = if n.is_nan() {
            "nan"
        } else if n.is_infinite() {
            if n > 0.0 { "infinity" } else { "-infinity" }
        } else {
            "integer"
        };
        (format_f64(ctx, this, n)?, ptype)
    };

    let mut parts: Vec<JSValue> = Vec::new();
    let mut part_obj = ctx.create_base_object()?;
    let type_str = make_str(ctx, part_type.as_bytes().to_vec())?;
    part_obj.set_property_str(ctx, "type", type_str)?;
    let val_str = make_str(ctx, formatted.into_bytes())?;
    part_obj.set_property_str(ctx, "value", val_str)?;
    parts.push(part_obj.into());

    Ok(values_to_array(ctx, parts)?.into())
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
    o.set_property_str(
        ctx,
        "minimumIntegerDigits",
        JSValue::from(this.min_integer_digits),
    )?;
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
        let _v4 = make_str(ctx, this.currency_display.as_bytes().to_vec())?;
        o.set_property_str(ctx, "currencyDisplay", _v4)?;
    }
    Ok(o.into())
}

const js_number_format_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("format", Some(js_number_format_format_getter), None, 0),
    FieldDef::FUNC("formatToParts", 1, js_number_format_formatToParts, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_number_format_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.NumberFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
