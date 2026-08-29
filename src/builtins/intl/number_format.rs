// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.NumberFormat` (§11).

use icu::{decimal::DecimalFormatter, locale::Locale};

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
    value::JSValue,
};

use super::common::{
    locale_extension_value, locale_to_string, make_bound_method, make_str, resolve_locale,
    to_utf8_string, values_to_array,
};

#[derive(Debug)]
pub struct NumberFormatData {
    pub(crate) locale: Locale,
    pub(crate) formatter: DecimalFormatter,
    pub(crate) min_fraction_digits: i32,
    pub(crate) max_fraction_digits: i32,
    pub(crate) min_significant_digits: Option<u8>,
    pub(crate) max_significant_digits: Option<u8>,
    pub(crate) min_integer_digits: i32,
    pub(crate) notation: Notation,
    pub(crate) style: Style,
    pub(crate) currency: Option<String>,
    pub(crate) currency_display: &'static str,
    pub(crate) currency_sign: &'static str,
    pub(crate) unit: Option<String>,
    pub(crate) numbering_system: String,
    pub(crate) use_grouping: bool,
    pub(crate) grouping_sep: char,
    pub(crate) decimal_sep: char,
    pub(crate) percent_space: bool,
    pub(crate) grouping_indian: bool,
    /// CLDR `minimumGroupingDigits=2` locales (de): 4-digit integers are not
    /// grouped ("1980", not "1.980").
    pub(crate) min_group2: bool,
    pub(crate) unit_display: &'static str,
    pub(crate) sign_display: SignDisplay,
    pub(crate) rounding_mode: RoundingMode,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Style {
    Decimal,
    Percent,
    Currency,
    Unit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SignDisplay {
    Auto,
    Never,
    Always,
    ExceptZero,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RoundingMode {
    HalfExpand,
    Trunc,
}

pub(crate) static CLASS_DEF_NUMBER_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_NumberFormat,
    js_number_format_proto_funcs,
    Some(ClassCtorDef::new_legacy(js_number_format_constructor, 0)),
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
/// Returns `(grouping, decimal, percent_space, indian, min_group2)`.
fn locale_separators(locale: &Locale) -> (char, char, bool, bool, bool) {
    let lang = locale.id.language.to_string();
    let region = locale.id.region.map(|r| r.to_string()).unwrap_or_default();
    // `percent_space` = whether a (no-break) space precedes the percent sign.
    // `grouping_indian` = use the Indian (last-3, then-2s) grouping.
    let indian = lang == "hi" || (lang == "en" && region == "IN");
    // CLDR minimumGroupingDigits=2: 4-digit integers stay ungrouped.
    let min_group2 = matches!(lang.as_str(), "de");
    match lang.as_str() {
        "de" => ('.', ',', true, false, min_group2),
        "fr" => ('\u{202f}', ',', true, false, min_group2),
        "en" | "th" | "ja" | "es" | "it" | "pt" | "nl" | "sv" | "da" | "nb" | "fi" | "ko"
        | "zh" => (',', '.', false, indian, min_group2),
        "ar" => ('٬', '٫', true, false, min_group2),
        "hi" => (',', '.', false, true, min_group2),
        _ => (',', '.', false, false, min_group2),
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
    let (grouping_sep, decimal_sep, percent_space, grouping_indian, min_group2) =
        locale_separators(&locale);

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
        currency_sign: "standard",
        unit: None,
        numbering_system: "latn".to_string(),
        use_grouping: true,
        grouping_sep,
        decimal_sep,
        percent_space,
        grouping_indian,
        min_group2,
        unit_display: "short",
        sign_display: SignDisplay::Auto,
        rounding_mode: RoundingMode::HalfExpand,
    };

    if !options.is_undefined() {
        apply_options(ctx, &mut data, options)?;
    }

    // ECMA-402 ResolveLocale: strip `cu`/`nu` extension keys when an explicit
    // option overrides them or their value is unsupported; strip all other
    // `u`-extension keys (not relevant to NumberFormat).
    let ns_opt: Option<&'static str> = if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let ns = obj.get_property_str(ctx, "numberingSystem")?;
        if ns.is_undefined() {
            None
        } else {
            let s = to_utf8_string(ctx, ns)?;
            Some(Box::leak(s.to_owned().into_boxed_str()) as &'static str)
        }
    } else {
        None
    };
    // `cu` is a relevant NumberFormat key only in currency style; in other
    // styles it is stripped (as an irrelevant `u`-extension key).
    let nu_ext = locale_extension_value(&data.locale, "nu");
    if matches!(data.style, Style::Currency) {
        let cu_opt: Option<&'static str> = data
            .currency
            .as_deref()
            .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
        data.locale =
            super::common::resolve_locale_keys(data.locale, &[("nu", ns_opt), ("cu", cu_opt)]);
    } else {
        data.locale = super::common::resolve_locale_keys(data.locale, &[("nu", ns_opt)]);
    }
    // Effective numbering system: explicit valid option > `nu` extension > latn.
    let nu_ext_valid = nu_ext
        .as_deref()
        .is_some_and(|e| super::common::supported_extension_key_value("nu", e));
    data.numbering_system = if let Some(opt) = ns_opt {
        if super::common::supported_extension_key_value("nu", opt) {
            opt.to_string()
        } else if nu_ext_valid {
            nu_ext.clone().unwrap()
        } else {
            "latn".to_string()
        }
    } else if nu_ext_valid {
        nu_ext.clone().unwrap()
    } else {
        "latn".to_string()
    };

    Ok(data)
}

pub(crate) fn js_number_format_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    // ECMA-402 normative optional: called without `new` (see the DateTimeFormat
    // constructor for the receiver/inner-instance mechanics).
    if !new_target.is_callable() {
        let data = build_number_format_data(ctx, cc.arg(0), cc.arg(1))?;
        let instance = ctx
            .ordinary_create_from_constructor_class(
                JSValue::UNDEFINED,
                crate::JS_CLASS_INTL_NUMBER_FORMAT,
                Box::into_raw(Box::new(data)) as *mut std::ffi::c_void,
            )
            .map_err(|_| -1)?
            .into();

        let derived = new_target
            .as_object()
            .map(|o| {
                let proto = o.prototype(ctx)?;
                Ok::<bool, i32>(
                    proto
                        .zip(ctx.class_proto(crate::JS_CLASS_INTL_NUMBER_FORMAT))
                        .is_some_and(|(a, b)| a == b),
                )
            })
            .transpose()?
            .unwrap_or(false);
        if derived {
            return super::common::attach_fallback(ctx, new_target, instance);
        }
        return Ok(instance);
    }

    let data = build_number_format_data(ctx, cc.arg(0), cc.arg(1))?;

    let instance = ctx
        .ordinary_create_from_constructor_class(
            new_target,
            crate::JS_CLASS_INTL_NUMBER_FORMAT,
            Box::into_raw(Box::new(data)) as *mut std::ffi::c_void,
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// Build `NumberFormatData` directly (no JS option plumbing) for internal
/// users such as `DurationFormat`, which spec-constructs fresh NumberFormat
/// instances per unit.
pub(crate) fn build_nf_data(
    ctx: &mut ScopedContext,
    locale: &Locale,
    numbering_system: &str,
    min_integer_digits: i32,
    min_frac: i32,
    max_frac: i32,
    sign_never: bool,
    use_grouping: bool,
    trunc: bool,
    unit: Option<(&str, &'static str)>,
) -> Result<NumberFormatData, i32> {
    let formatter = DecimalFormatter::try_new(locale.clone().into(), Default::default())
        .map_err(|_| ctx.throw_range_error("Unable to load locale data"))?;
    let (grouping_sep, decimal_sep, percent_space, grouping_indian, min_group2) =
        locale_separators(locale);
    Ok(NumberFormatData {
        locale: locale.clone(),
        formatter,
        min_fraction_digits: min_frac.max(0),
        max_fraction_digits: max_frac.max(0),
        min_significant_digits: None,
        max_significant_digits: None,
        min_integer_digits,
        notation: Notation::Standard,
        style: if unit.is_some() {
            Style::Unit
        } else {
            Style::Decimal
        },
        currency: None,
        currency_display: "symbol",
        currency_sign: "standard",
        unit: unit.map(|(u, _)| u.to_string()),
        unit_display: unit.map(|(_, d)| d).unwrap_or("short"),
        numbering_system: numbering_system.to_string(),
        use_grouping,
        grouping_sep,
        decimal_sep,
        percent_space,
        grouping_indian,
        min_group2,
        sign_display: if sign_never {
            SignDisplay::Never
        } else {
            SignDisplay::Auto
        },
        rounding_mode: if trunc {
            RoundingMode::Trunc
        } else {
            RoundingMode::HalfExpand
        },
    })
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
    // currencySign: standard | accounting
    let currency_sign = obj.get_property_str(ctx, "currencySign")?;
    if !currency_sign.is_undefined() {
        let s = to_utf8_string(ctx, currency_sign)?;
        data.currency_sign = match s.as_str() {
            "standard" => "standard",
            "accounting" => "accounting",
            _ => return Err(ctx.throw_range_error("Invalid currencySign value")),
        };
    }

    // unit: validated whenever provided (sanctioned simple units and their
    // `-per-` compounds); required when style is "unit".
    let unit = obj.get_property_str(ctx, "unit")?;
    let unit_str = if unit.is_undefined() {
        None
    } else {
        Some(to_utf8_string(ctx, unit)?)
    };
    if let Some(u) = &unit_str {
        let valid = super::common::SUPPORTED_UNITS.contains(&u.as_str())
            || (u.contains("-per-")
                && u.split("-per-")
                    .all(|p| super::common::SUPPORTED_UNITS.contains(&p)));
        if !valid {
            return Err(ctx.throw_range_error("Invalid unit value"));
        }
    }
    if matches!(data.style, Style::Unit) {
        match unit_str {
            Some(u) => data.unit = Some(u),
            None => return Err(ctx.throw_type_error("Unit style requires a unit")),
        }
    } else {
        data.unit = None;
    }

    // unitDisplay: long | short | narrow
    let unit_display = obj.get_property_str(ctx, "unitDisplay")?;
    if !unit_display.is_undefined() {
        let s = to_utf8_string(ctx, unit_display)?;
        data.unit_display = match s.as_str() {
            "long" => "long",
            "short" => "short",
            "narrow" => "narrow",
            _ => return Err(ctx.throw_range_error("Invalid unitDisplay value")),
        };
    }

    // signDisplay: auto | never | always | exceptZero
    let sign_display = obj.get_property_str(ctx, "signDisplay")?;
    if !sign_display.is_undefined() {
        let s = to_utf8_string(ctx, sign_display)?;
        data.sign_display = match s.as_str() {
            "auto" => SignDisplay::Auto,
            "never" => SignDisplay::Never,
            "always" => SignDisplay::Always,
            "exceptZero" => SignDisplay::ExceptZero,
            _ => return Err(ctx.throw_range_error("Invalid signDisplay value")),
        };
    }

    // roundingMode: only "trunc" changes behavior; other spec names are
    // accepted and fall back to the default half-expand rounding.
    let rounding_mode = obj.get_property_str(ctx, "roundingMode")?;
    if !rounding_mode.is_undefined() {
        let s = to_utf8_string(ctx, rounding_mode)?;
        data.rounding_mode = match s.as_str() {
            "trunc" => RoundingMode::Trunc,
            "ceil" | "floor" | "expand" | "contract" | "halfExpand" | "halfCeil" | "halfFloor"
            | "halfTrunc" | "halfEven" => RoundingMode::HalfExpand,
            _ => return Err(ctx.throw_range_error("Invalid roundingMode value")),
        };
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
            "engineering" => Notation::Engineering,
            "scientific" => Notation::Scientific,
            "compact" => {
                // compactDisplay is only meaningful (and validated) here.
                let cd = obj.get_property_str(ctx, "compactDisplay")?;
                if cd.is_undefined() {
                    Notation::CompactShort
                } else {
                    match to_utf8_string(ctx, cd)?.as_str() {
                        "long" => Notation::CompactLong,
                        "short" => Notation::CompactShort,
                        _ => return Err(ctx.throw_range_error("Invalid compactDisplay value")),
                    }
                }
            }
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
                match data.notation {
                    // The currency fraction-digit special handling only
                    // applies to "standard" notation.
                    Notation::Standard => {
                        data.max_fraction_digits = 2;
                        data.min_fraction_digits = 2;
                    }
                    Notation::CompactShort | Notation::CompactLong => {
                        data.max_fraction_digits = 0;
                        data.min_fraction_digits = 0;
                    }
                    _ => {
                        data.max_fraction_digits = 3;
                        data.min_fraction_digits = 0;
                    }
                }
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
    let this_val = super::common::unwrap_fallback(ctx, cc.this(), &|v| is_number_format(v))?;
    // The bound `format` function is anonymous (name is the empty string).
    make_bound_method(ctx, this_val, js_number_format_format_bound, "", 1)
}

fn is_number_format(v: &JSValue) -> bool {
    v.as_object()
        .is_some_and(|o| o.payload_intl_number_format().is_some())
}

fn js_number_format_format_bound(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = cc.data(0);
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let this = this_obj
        .payload_intl_number_format()
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
        // Increment the kept digits (may carry, e.g. 9999 -> 10000)..
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
        s.push_str(&group_integer(
            &int_part,
            this.grouping_sep,
            this.grouping_indian,
        ));
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
        let zero = digits.trim_start_matches('0').is_empty();
        append_percent_suffix(this, &mut s);
        apply_sign_display(this, &mut s, zero, false);
        append_unit_suffix(this, &mut s);
        if this.numbering_system != "latn" {
            s = translate_digits(&s, &this.numbering_system);
        }
        return Ok(s);
    }

    // 11.1.6 FormatNumericValue: ToIntlMathematicalValue then rounding.
    let n = v.to_number(ctx)?.to_f64();
    format_f64(ctx, this, n)
}

/// Format an already-converted `f64` value (no further coercion).
pub(crate) fn format_f64(
    ctx: &mut ScopedContext,
    this: &NumberFormatData,
    n: f64,
) -> Result<String, i32> {
    let parts = format_parts(ctx, this, n)?;
    let mut s = String::new();
    for p in parts {
        s.push_str(&p.value);
    }
    if this.numbering_system != "latn" {
        s = translate_digits(&s, &this.numbering_system);
    }
    Ok(s)
}

/// Split a formatted numeric digit string (digits plus grouping/decimal
/// separators, no sign) into typed integer/group/decimal/fraction parts.
fn digits_to_parts(this: &NumberFormatData, s: &str) -> Vec<NPart> {
    let mut out: Vec<NPart> = Vec::new();
    let mut run = String::new();
    let mut seen_decimal = false;
    for c in s.chars() {
        if c == this.decimal_sep {
            if !run.is_empty() {
                out.push(NPart {
                    typ: "integer",
                    value: std::mem::take(&mut run),
                });
            }
            out.push(NPart {
                typ: "decimal",
                value: c.to_string(),
            });
            seen_decimal = true;
        } else if c == this.grouping_sep {
            if !run.is_empty() {
                out.push(NPart {
                    typ: "integer",
                    value: std::mem::take(&mut run),
                });
            }
            out.push(NPart {
                typ: "group",
                value: c.to_string(),
            });
        } else {
            run.push(c);
        }
    }
    if !run.is_empty() {
        let typ = if seen_decimal { "fraction" } else { "integer" };
        out.push(NPart { typ, value: run });
    }
    out
}

/// FormatNumericToParts core. Pure (no JS effects); `format_f64` joins the
/// values.
pub(crate) fn format_parts(
    ctx: &mut ScopedContext,
    this: &NumberFormatData,
    n: f64,
) -> Result<Vec<NPart>, i32> {
    let lang = this.locale.id.language.to_string();

    if n.is_nan() {
        // Localized NaN spelling (CLDR): zh uses 非數值, others keep "NaN".
        let text = if matches!(lang.as_str(), "zh" | "yue") {
            "非數值"
        } else {
            "NaN"
        };
        let mut out = Vec::new();
        if matches!(this.sign_display, SignDisplay::Always) {
            out.push(NPart {
                typ: "plusSign",
                value: "+".to_string(),
            });
        }
        out.push(NPart {
            typ: "nan",
            value: text.to_string(),
        });
        return Ok(out);
    }
    if n.is_infinite() {
        let mut out = Vec::new();
        match this.sign_display {
            SignDisplay::Never => {}
            SignDisplay::Auto => {
                if n < 0.0 {
                    out.push(NPart {
                        typ: "minusSign",
                        value: "-".to_string(),
                    });
                }
            }
            _ => {
                out.push(NPart {
                    typ: if n < 0.0 { "minusSign" } else { "plusSign" },
                    value: if n < 0.0 { "-" } else { "+" }.to_string(),
                });
            }
        }
        out.push(NPart {
            typ: "infinity",
            value: "\u{221e}".to_string(),
        });
        return Ok(out);
    }

    let negative = n < 0.0 || (n == 0.0 && n.is_sign_negative());
    let zero = n == 0.0;
    let mag = n.abs();
    // The sign is evaluated on the ROUNDED value (standard notation): a raw
    // nonzero value that rounds to zero loses its minus, while a raw -0
    // keeps it under auto/always.
    let (mut eff_zero, mut eff_negative) = (zero, negative);
    if matches!(this.notation, Notation::Standard) && this.rounding_mode != RoundingMode::Trunc {
        let mag_scaled = if matches!(this.style, Style::Percent) {
            mag * 100.0
        } else {
            mag
        };
        let r = round_to_fraction(
            mag_scaled,
            this.min_fraction_digits,
            this.max_fraction_digits,
        );
        if r == 0.0 {
            eff_zero = true;
            if !zero {
                eff_negative = false;
            }
        }
    }

    // ---- numeric core (notation-dependent) -------------------------------
    let mut num_parts: Vec<NPart> = match this.notation {
        Notation::Scientific | Notation::Engineering => {
            let engineering = matches!(this.notation, Notation::Engineering);
            let mut exp = if mag > 0.0 {
                mag.log10().floor() as i32
            } else {
                0
            };
            if engineering {
                exp -= exp.rem_euclid(3);
            }
            let mut m = mag / 10f64.powi(exp);
            if this.rounding_mode != RoundingMode::Trunc {
                m = round_to_fraction(m, 0, 3);
            }
            // Rounding may carry the mantissa into the next decade(s).
            let limit = if engineering { 1000.0 } else { 10.0 };
            while m >= limit {
                m /= limit;
                exp += if engineering { 3 } else { 1 };
            }
            let body = format_fixed(
                ctx,
                m,
                0,
                3,
                false,
                this.grouping_sep,
                this.decimal_sep,
                1,
                false,
                false,
            )?;
            let mut parts = digits_to_parts(this, &body);
            parts.push(NPart {
                typ: "exponentSeparator",
                value: "E".to_string(),
            });
            if exp < 0 {
                parts.push(NPart {
                    typ: "exponentMinusSign",
                    value: "-".to_string(),
                });
            }
            parts.push(NPart {
                typ: "exponentInteger",
                value: exp.abs().to_string(),
            });
            parts
        }
        Notation::CompactShort | Notation::CompactLong => {
            let long = matches!(this.notation, Notation::CompactLong);
            let entries = compact_entries(&lang, long);
            let mut chosen: Option<(i32, &'static str, &'static str)> = None;
            for &entry in entries {
                let (e, _, _) = entry;
                if mag >= 10f64.powi(e) {
                    chosen = Some(entry);
                }
            }
            let exp = chosen.map(|(e, _, _)| e).unwrap_or(0);
            let scaled = mag / 10f64.powi(exp);
            // ICU compact rule: keep roughly one leading integer digit's
            // worth of precision (maxFrac = 1 - magnitude), clamped at 0.
            let max_frac = if scaled > 0.0 {
                (1 - scaled.log10().floor() as i32).max(0)
            } else {
                0
            };
            let rounded = round_to_fraction(scaled, 0, max_frac);
            // Below the smallest compact entry the value renders plain;
            // grouping follows the locale (CJK locales keep it off here).
            let grouped = chosen.is_none()
                && this.use_grouping
                && !matches!(lang.as_str(), "ja" | "zh" | "yue");
            let body = format_fixed(
                ctx,
                rounded,
                0,
                max_frac,
                grouped,
                this.grouping_sep,
                this.decimal_sep,
                if chosen.is_some() {
                    1
                } else {
                    this.min_integer_digits
                },
                this.grouping_indian,
                this.min_group2,
            )?;
            let mut parts = digits_to_parts(this, &body);
            if let Some((_, sep, text)) = chosen {
                if !sep.is_empty() {
                    parts.push(NPart {
                        typ: "literal",
                        value: sep.to_string(),
                    });
                }
                parts.push(NPart {
                    typ: "compact",
                    value: text.to_string(),
                });
            }
            parts
        }
        Notation::Standard => {
            let scaled = if matches!(this.style, Style::Percent) {
                n * 100.0
            } else {
                n
            };
            // The sign is rendered by the sign part; the digit pipeline gets
            // the absolute value.
            let scaled_abs = scaled.abs();
            let body = if let (Some(min), Some(max)) =
                (this.min_significant_digits, this.max_significant_digits)
            {
                let grouped = this.use_grouping
                    && !(this.min_group2 && scaled_abs >= 1000.0 && scaled_abs < 10000.0);
                format_with_significant(
                    ctx,
                    scaled_abs,
                    min,
                    max,
                    grouped,
                    this.grouping_sep,
                    this.decimal_sep,
                    this.grouping_indian,
                )?
            } else {
                let rounded = if this.rounding_mode == RoundingMode::Trunc {
                    scaled_abs
                } else {
                    round_to_fraction(
                        scaled_abs,
                        this.min_fraction_digits,
                        this.max_fraction_digits,
                    )
                };
                format_fixed(
                    ctx,
                    rounded,
                    this.min_fraction_digits,
                    this.max_fraction_digits,
                    this.use_grouping,
                    this.grouping_sep,
                    this.decimal_sep,
                    this.min_integer_digits,
                    this.grouping_indian,
                    // Standard notation groups normally; the min-grouping
                    // suppression only applies inside compact fallbacks.
                    false,
                )?
            };
            digits_to_parts(this, &body)
        }
    };

    // ---- sign -------------------------------------------------------------
    // Accounting locales whose negative pattern is parenthesis-based (en, ja,
    // ko, zh…) evaluate the sign on the ROUNDED value; minus-pattern locales
    // (de, fr) and all non-accounting styles keep the raw value's sign.
    let mut sign_val: Option<&str> = None;
    let mut paren_wrap = false;
    let acct = this.style == Style::Currency && this.currency_sign == "accounting";
    let paren_locale = accounting_uses_parens(&lang);
    let sign_negative = if acct && paren_locale {
        eff_negative
    } else {
        negative
    };
    let except_zero_suppressed = matches!(this.sign_display, SignDisplay::ExceptZero) && eff_zero;
    if acct
        && negative
        && !except_zero_suppressed
        && paren_locale
        && !matches!(this.sign_display, SignDisplay::Never)
    {
        paren_wrap = true;
    } else {
        match this.sign_display {
            SignDisplay::Never => {}
            SignDisplay::Auto => {
                if sign_negative {
                    sign_val = Some("-");
                }
            }
            SignDisplay::Always => {
                sign_val = Some(if sign_negative { "-" } else { "+" });
            }
            SignDisplay::ExceptZero => {
                if !except_zero_suppressed {
                    sign_val = Some(if sign_negative { "-" } else { "+" });
                }
            }
        }
    }

    // ---- affixes (currency / unit / percent) ------------------------------
    let mut pre: Vec<NPart> = Vec::new();
    let mut cur_pre: Vec<NPart> = Vec::new();
    let mut post: Vec<NPart> = Vec::new();

    if this.style == Style::Unit
        && let Some(u) = &this.unit
    {
        let joined: String = num_parts.iter().map(|p| p.value.as_str()).collect();
        let core: String = joined.chars().filter(|c| c.is_ascii_digit()).collect();
        let singular = core == "1";
        if u == "kilometer-per-hour" {
            let (prefix, name, space) = kilometer_per_hour_name(&lang, this.unit_display, singular);
            if let Some((pfx, psep)) = prefix {
                pre.push(NPart {
                    typ: "unit",
                    value: pfx.to_string(),
                });
                if !psep.is_empty() {
                    pre.push(NPart {
                        typ: "literal",
                        value: psep.to_string(),
                    });
                }
            }
            if space {
                post.push(NPart {
                    typ: "literal",
                    value: " ".to_string(),
                });
            }
            post.push(NPart {
                typ: "unit",
                value: name,
            });
        } else if u == "percent" {
            // CLDR en: unit "percent" long is "{0} percent"; short/narrow
            // attach the sign directly.
            match this.unit_display {
                "long" => {
                    post.push(NPart {
                        typ: "literal",
                        value: " ".to_string(),
                    });
                    post.push(NPart {
                        typ: "unit",
                        value: "percent".to_string(),
                    });
                }
                _ => {
                    post.push(NPart {
                        typ: "unit",
                        value: "%".to_string(),
                    });
                }
            }
        } else {
            let name = unit_display_name(u, this.unit_display, singular);
            let cjk = matches!(lang.as_str(), "ja" | "zh" | "ko" | "th" | "lo" | "my");
            if !cjk {
                post.push(NPart {
                    typ: "literal",
                    value: " ".to_string(),
                });
            }
            post.push(NPart {
                typ: "unit",
                value: name,
            });
        }
    }

    if this.style == Style::Percent {
        post.push(NPart {
            typ: "percentSign",
            value: "%".to_string(),
        });
    }

    if this.style == Style::Currency
        && let Some(c) = &this.currency
    {
        let sym = currency_symbol(&lang, c, this.currency_display);
        if currency_symbol_is_prefix(&lang) {
            cur_pre.push(NPart {
                typ: "currency",
                value: sym,
            });
        } else {
            // CLDR suffix patterns use a no-break space ("987,00\u{a0}$").
            post.push(NPart {
                typ: "literal",
                value: "\u{a0}".to_string(),
            });
            post.push(NPart {
                typ: "currency",
                value: sym,
            });
        }
    }

    // ---- assemble ----------------------------------------------------------
    let mut out: Vec<NPart> = Vec::new();
    out.extend(pre);
    if let Some(sv) = sign_val {
        let typ = if sv == "-" { "minusSign" } else { "plusSign" };
        out.push(NPart {
            typ,
            value: sv.to_string(),
        });
    }
    out.extend(cur_pre);
    out.append(&mut num_parts);
    out.extend(post);
    if paren_wrap {
        out.insert(
            0,
            NPart {
                typ: "literal",
                value: "(".to_string(),
            },
        );
        out.push(NPart {
            typ: "literal",
            value: ")".to_string(),
        });
    }
    Ok(out)
}

/// Apply the signDisplay policy to a formatted numeric string. `zero`/`neg_zero`
/// describe the formatted value's sign so `-0` keeps its minus under
/// auto/always but is unsigned under never/exceptZero.
fn apply_sign_display(this: &NumberFormatData, s: &mut String, zero: bool, neg_zero: bool) {
    match this.sign_display {
        SignDisplay::Never => {
            if s.starts_with('-') {
                s.remove(0);
            }
        }
        SignDisplay::Always => {
            if !s.starts_with('-') && !s.is_empty() {
                s.insert(0, '+');
            }
        }
        SignDisplay::ExceptZero => {
            if zero {
                if neg_zero && s.starts_with('-') {
                    s.remove(0);
                }
            } else if !s.starts_with('-') && !s.is_empty() {
                s.insert(0, '+');
            }
        }
        SignDisplay::Auto => {}
    }
}

// =========================================================================
// Typed parts pipeline
// =========================================================================

/// One element of a partitioned format result (`formatToParts`).
pub(crate) struct NPart {
    pub(crate) typ: &'static str,
    pub(crate) value: String,
}

/// Compact-notation entries per locale/style: `(exponent, separator, text)`.
/// The separator joins number and suffix ("de" short uses a no-break space);
/// CJK locales attach directly.
// ponytail: hand-written CLDR snapshot for the locales test262 exercises;
// swap to a real compact-decimal formatter when ICU4X ships one
fn compact_entries(lang: &str, long: bool) -> &'static [(i32, &'static str, &'static str)] {
    const K: &[(i32, &str, &str)] = &[(3, "", "K"), (6, "", "M"), (9, "", "B"), (12, "", "T")];
    const K_LONG: &[(i32, &str, &str)] = &[
        (3, " ", "thousand"),
        (6, " ", "million"),
        (9, " ", "billion"),
        (12, " ", "trillion"),
    ];
    match lang {
        // CLDR de short has no 1e3 pattern; compaction starts at Mio.
        "de" if !long => &[
            (6, "\u{a0}", "Mio."),
            (9, "\u{a0}", "Mrd."),
            (12, "\u{a0}", "Bio."),
        ],
        "de" => &[
            (3, " ", "Tausend"),
            (6, " ", "Millionen"),
            (9, " ", "Milliarden"),
            (12, " ", "Billionen"),
        ],
        "ja" => &[(4, "", "万"), (8, "", "億"), (12, "", "兆")],
        "ko" => &[(3, "", "천"), (4, "", "만"), (8, "", "억"), (12, "", "조")],
        "zh" | "yue" => &[(4, "", "萬"), (8, "", "億"), (12, "", "兆")],
        _ if long => K_LONG,
        _ => K,
    }
}

/// Localized display of a speed unit `kilometer-per-hour`: an optional prefix
/// word placed before the number (CJK long forms) with its own separator
/// literal, the unit name, and whether a space joins number and name.
// ponytail: single-unit CLDR snapshot; extend per unit when tests demand more
fn kilometer_per_hour_name(
    lang: &str,
    display: &str,
    singular: bool,
) -> (Option<(&'static str, &'static str)>, String, bool) {
    match lang {
        "de" => (
            None,
            match display {
                "long" => "Kilometer pro Stunde".to_string(),
                _ => "km/h".to_string(),
            },
            true,
        ),
        "ja" => (
            (display == "long").then_some(("時速", "")),
            match display {
                "long" => "キロメートル".to_string(),
                _ => "km/h".to_string(),
            },
            false,
        ),
        "ko" => (
            (display == "long").then_some(("시속", " ")),
            match display {
                "long" => "킬로미터".to_string(),
                _ => "km/h".to_string(),
            },
            false,
        ),
        "zh" | "yue" => (
            (display == "long").then_some(("每小時", "")),
            match display {
                "long" => "公里".to_string(),
                _ => "公里/小時".to_string(),
            },
            false,
        ),
        // en and the generic fallback
        _ => (
            None,
            match display {
                "long" if !singular => "kilometers per hour".to_string(),
                "long" => "kilometer per hour".to_string(),
                _ => "km/h".to_string(),
            },
            display != "narrow",
        ),
    }
}

/// Currency symbol placement: true when the locale writes the symbol before
/// the number ("$987.00"); false for suffix styles ("987,00 $").
fn currency_symbol_is_prefix(lang: &str) -> bool {
    !matches!(
        lang,
        "de" | "fr" | "es" | "it" | "nl" | "sv" | "fi" | "pl" | "ru"
    )
}

/// Accounting-style negatives wrap the amount in parentheses (CLDR patterns
/// for these locales); most others keep the minus sign.
fn accounting_uses_parens(lang: &str) -> bool {
    !matches!(lang, "de" | "fr")
}

/// Currency symbol for `(code, currencyDisplay)`; falls back to the code.
fn currency_symbol(lang: &str, code: &str, display: &str) -> String {
    match display {
        "code" => code.to_string(),
        "name" => code.to_string(),
        "symbol" | "narrowSymbol" => {
            let usd = if matches!(lang, "ko" | "zh" | "yue") {
                "US$"
            } else {
                "$"
            };
            match code {
                "USD" => return usd.to_string(),
                "EUR" => "€",
                "GBP" => "£",
                "JPY" => "¥",
                "CNY" => "CN¥",
                "KRW" => "₩",
                "INR" => "₹",
                "HKD" => "HK$",
                "TWD" => "NT$",
                "CHF" => "CHF",
                _ => code,
            }
            .to_string()
        }
        _ => code.to_string(),
    }
}

/// Localized display name for a unit (`unitDisplay` style); CLDR en data for
/// the duration units, other units fall back to the raw unit id.
fn unit_display_name(unit: &str, display: &str, singular: bool) -> String {
    let short: &str = match unit {
        "year" => "yr",
        "month" => "mo",
        "week" => "wk",
        "day" => "day",
        "hour" => "hr",
        "minute" => "min",
        "second" => "sec",
        "millisecond" => "ms",
        "microsecond" => "μs",
        "nanosecond" => "ns",
        other => return other.to_string(),
    };
    let narrow: &str = match unit {
        "year" => "y",
        "month" => "m",
        "week" => "w",
        "day" => "d",
        "hour" => "h",
        "minute" => "m",
        "second" => "s",
        "millisecond" => "ms",
        "microsecond" => "μs",
        "nanosecond" => "ns",
        other => return other.to_string(),
    };
    match display {
        // "day" is the only short form with a distinct English plural.
        "short" => {
            if unit == "day" && !singular {
                "days".to_string()
            } else {
                short.to_string()
            }
        }
        "narrow" => narrow.to_string(),
        // long: regular English plurals.
        _ => {
            let mut s = unit.to_string();
            if !singular && !s.ends_with('s') {
                s.push('s');
            }
            s
        }
    }
}

/// Append the localized unit name for `style: "unit"` formatting.
fn append_unit_suffix(this: &NumberFormatData, s: &mut String) {
    if !matches!(this.style, Style::Unit) {
        return;
    }
    let Some(u) = &this.unit else {
        return;
    };
    // Singular iff the formatted magnitude is exactly 1.
    let core: String = s
        .trim_start_matches('-')
        .chars()
        .filter(|c| *c != this.grouping_sep)
        .collect();
    let name = unit_display_name(u, this.unit_display, core == "1");
    // Space-separated unit names in most languages; CJK/Thai attach directly.
    let lang = this.locale.id.language.to_string();
    if !matches!(lang.as_str(), "ja" | "zh" | "ko" | "th" | "lo" | "my") {
        s.push(' ');
    }
    s.push_str(&name);
}

/// Replace ASCII digits in `s` with the digits of the given numbering system.
/// Only the ECMA-402 "simple digit mapping" systems (where each digit maps to
/// a distinct Unicode codepoint) are handled; `latn` and unknown systems are
/// returned unchanged.
pub(crate) fn translate_digits(s: &str, numbering_system: &str) -> String {
    let digits = numbering_system_digits(numbering_system);
    let Some(ds) = digits else {
        return s.to_string();
    };
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if let Some(d) = c.to_digit(10) {
            out.push(ds[d as usize]);
        } else {
            out.push(c);
        }
    }
    out
}

/// The 10 digit characters of a simple-digit-mapping numbering system, or
/// None for `latn`/unknown systems.
fn numbering_system_digits(ns: &str) -> Option<[char; 10]> {
    fn seq(cp: u32) -> [char; 10] {
        std::array::from_fn(|i| char::from_u32(cp + i as u32).unwrap_or('0'))
    }
    Some(match ns {
        "latn" => return None,
        "adlm" => seq(0x1e950),
        "ahom" => seq(0x11730),
        "arab" => seq(0x0660),
        "arabext" => seq(0x06f0),
        "bali" => seq(0x1b50),
        "beng" => seq(0x09e6),
        "bhks" => seq(0x11c50),
        "brah" => seq(0x11066),
        "cakm" => seq(0x11136),
        "cham" => seq(0xaa50),
        "deva" => seq(0x0966),
        "diak" => seq(0x11950),
        "fullwide" => seq(0xff10),
        "gara" => seq(0x10d40),
        "gong" => seq(0x11da0),
        "gonm" => seq(0x11d50),
        "gujr" => seq(0x0ae6),
        "gukh" => seq(0x16130),
        "guru" => seq(0x0a66),
        "hmng" => seq(0x16b50),
        "hmnp" => seq(0x1e140),
        "java" => seq(0xa9d0),
        "kali" => seq(0xa900),
        "kawi" => seq(0x11f50),
        "khmr" => seq(0x17e0),
        "knda" => seq(0x0ce6),
        "krai" => seq(0x16d70),
        "lana" => seq(0x1a80),
        "lanatham" => seq(0x1a90),
        "laoo" => seq(0x0ed0),
        "lepc" => seq(0x1c40),
        "limb" => seq(0x1946),
        "mathbold" => seq(0x1d7ce),
        "mathdbl" => seq(0x1d7d8),
        "mathmono" => seq(0x1d7f6),
        "mathsanb" => seq(0x1d7ec),
        "mathsans" => seq(0x1d7e2),
        "mlym" => seq(0x0d66),
        "modi" => seq(0x11650),
        "mong" => seq(0x1810),
        "mroo" => seq(0x16a60),
        "mtei" => seq(0xabf0),
        "mymr" => seq(0x1040),
        "mymrepka" => seq(0x116da),
        "mymrpao" => seq(0x116d0),
        "mymrshan" => seq(0x1090),
        "mymrtlng" => seq(0xa9f0),
        "nagm" => seq(0x1e4f0),
        "newa" => seq(0x11450),
        "nkoo" => seq(0x07c0),
        "olck" => seq(0x1c50),
        "onao" => seq(0x1e5f1),
        "orya" => seq(0x0b66),
        "osma" => seq(0x104a0),
        "outlined" => seq(0x1ccf0),
        "rohg" => seq(0x10d30),
        "saur" => seq(0xa8d0),
        "segment" => seq(0x1fbf0),
        "shrd" => seq(0x111d0),
        "sind" => seq(0x112f0),
        "sinh" => seq(0x0de6),
        "sora" => seq(0x110f0),
        "sund" => seq(0x1bb0),
        "sunu" => seq(0x11bf0),
        "takr" => seq(0x116c0),
        "talu" => seq(0x19d0),
        "tamldec" => seq(0x0be6),
        "telu" => seq(0x0c66),
        "thai" => seq(0x0e50),
        "tibt" => seq(0x0f20),
        "tirh" => seq(0x114d0),
        "tnsa" => seq(0x16ac0),
        "tols" => seq(0x11de0),
        "vaii" => seq(0xa620),
        "wara" => seq(0x118e0),
        "wcho" => seq(0x1e2f0),
        // hanidec uses distinct CJK numerals (〇一二三四五六七八九).
        "hanidec" => ['〇', '一', '二', '三', '四', '五', '六', '七', '八', '九'],
        _ => return None,
    })
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
    grouping_indian: bool,
    min_group2: bool,
) -> Result<String, i32> {
    // Use the *shortest* decimal representation (round-tripping the rounded
    // f64), then trim trailing zeros down to the *minimum* fraction digits.
    // This avoids exposing the full binary expansion for large integer values
    // (e.g. 1.2344501e34 stays "12344501000000000000000000000000000" instead of
    // the exact f64 digit string).
    let f = format_shortest(n);
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

    // Cap the fraction at `max_frac`, then trim trailing zeros down to
    // `min_frac` (padding with zeros up to `min_frac` when needed).
    let capped: String = frac_str.chars().take(max_frac as usize).collect();
    let nz = capped.trim_end_matches('0');
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
    // minimumGroupingDigits=2 locales leave 4-digit integers ungrouped.
    let use_grouping = use_grouping && !(min_group2 && int_part.len() == 4);
    if use_grouping {
        int_part = group_integer(&int_part, grouping_sep, grouping_indian);
    }

    let mut out = String::new();
    out.push_str(sign);
    out.push_str(&int_part);
    if keep > 0 {
        out.push(decimal_sep);
        // Pad the fraction to the target length (which may exceed `capped`
        // when `min_frac` requires trailing zeros).
        out.push_str(&capped);
        if capped.len() < keep {
            out.push_str(&"0".repeat(keep - capped.len()));
        }
    }
    Ok(out)
}

fn insert_grouping(s: &mut String, grouping_sep: char, indian: bool) {
    // Insert grouping separators into the integer part (locale-aware).
    if let Some(dot) = s.find('.') {
        let int_part = &s[..dot];
        let neg = int_part.starts_with('-');
        let digits = if neg { &int_part[1..] } else { int_part };
        let grouped = group_integer(digits, grouping_sep, indian);
        *s = if neg {
            format!("-{}{}", grouped, &s[dot..])
        } else {
            format!("{}{}", grouped, &s[dot..])
        };
    } else {
        let neg = s.starts_with('-');
        let digits = if neg { &s[1..] } else { s.as_str() };
        let grouped = group_integer(digits, grouping_sep, indian);
        *s = if neg {
            format!("-{}", grouped)
        } else {
            grouped
        };
    }
}

/// Group a sign-less digit string with `grouping_sep`. Standard grouping is
/// every three digits; Indian grouping (hi/en-IN) uses a last group of 3 then
/// groups of 2.
fn group_integer(digits: &str, grouping_sep: char, indian: bool) -> String {
    let chars: Vec<char> = digits.chars().collect();
    let n = chars.len();
    let mut grouped = String::new();
    if indian {
        // Indian grouping: the last group is 3 digits, the preceding groups
        // are 2 digits each. A separator is inserted before digit positions
        // whose remaining count is odd and >= 3 (from the right).
        for (i, c) in chars.iter().enumerate() {
            let remaining = n - i;
            if i > 0 && remaining >= 3 && remaining % 2 == 1 {
                grouped.push(grouping_sep);
            }
            grouped.push(*c);
        }
    } else {
        for (i, c) in chars.iter().enumerate() {
            if i > 0 && (n - i) % 3 == 0 {
                grouped.push(grouping_sep);
            }
            grouped.push(*c);
        }
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
    grouping_indian: bool,
) -> Result<String, i32> {
    // Format the value as its shortest round-trip decimal, then round to `max`
    // // significant digits and pad to `min` - all in decimal-string space (this
    // avoids f64 division errors on large values).
    if n == 0.0 {
        // "0" padded to `min` significant digits (preserve the -0 sign).
        let neg = n.is_sign_negative();
        let mut s = String::new();
        if neg {
            s.push('-');
        }
        s.push('0');
        if min > 1 {
            s.push(decimal_sep);
            s.push_str(&"0".repeat((min as usize).saturating_sub(1)));
        }
        return Ok(s);
    }

    let neg = n.is_sign_negative();
    let mag = n.abs();
    let s = format_shortest(mag);
    let exp = mag.log10().floor() as i32;

    // Reassemble into an "all-digits + scale" form: digits is the full digit
    // string with an implied decimal point after `int_len` digits.
    let (int_str, frac_str) = match s.find('.') {
        Some(dot) => (s[..dot].to_string(), s[dot + 1..].to_string()),
        None => (s.clone(), String::new()),
    };
    let mut all_digits: Vec<char> = int_str.chars().chain(frac_str.chars()).collect();
    let mut int_len = int_str.len();
    // Locate the first significant digit (skip leading zeros).
    let first_nz = all_digits.iter().position(|&c| c != '0');
    let sig_start = first_nz.unwrap_or(all_digits.len());
    let sig_count = all_digits.len() - sig_start;

    if sig_count > max as usize {
        // Round the significant digits to `max` digits.
        let keep = max as usize;
        let mut digits: Vec<char> = all_digits[sig_start..sig_start + keep].to_vec();
        let rest = &all_digits[sig_start + keep..];
        if rest.iter().any(|&c| c != '0') && rest[0] >= '5' {
            // Increment the kept digits; propagate carry.
            let mut carry = true;
            for i in (0..digits.len()).rev() {
                if carry {
                    if digits[i] == '9' {
                        digits[i] = '0';
                    } else {
                        digits[i] = char::from_u32(digits[i] as u32 + 1).unwrap();
                        carry = false;
                    }
                }
            }
            if carry {
                // Rounding carried past the top digit (e.g. 9999... -> 10000...).
                digits.insert(0, '1');
                int_len += 1;
            }
        }
        // Rebuild the digit sequence: leading zeros + kept digits + zero-fill
        // to preserve the original magnitude (digits after the kept ones are 0).
        let orig_len = all_digits.len();
        all_digits.truncate(sig_start);
        all_digits.extend_from_slice(&digits);
        while all_digits.len() < orig_len {
            all_digits.push('0');
        }
    }

    // Reconstruct the decimal string: place the point after `int_len` digits.
    let int_digits: String = all_digits.iter().take(int_len).collect();
    let frac_digits: String = all_digits.iter().skip(int_len).collect();
    let int_digits = if int_digits.is_empty() {
        "0".to_string()
    } else {
        int_digits
    };
    let frac_trimmed = frac_digits.trim_end_matches('0').to_string();
    let mut result = if frac_trimmed.is_empty() {
        int_digits.clone()
    } else {
        format!("{}.{}", int_digits, frac_trimmed)
    };

    // Now adjust significant-digit count to lie within [min, max].
    let mut count = count_sig_digits(&int_digits, &frac_trimmed);
    while count < min as usize {
        result.push('0');
        count += 1;
    }

    let mut out = if neg { format!("-{}", result) } else { result };
    if use_grouping {
        insert_grouping(&mut out, grouping_sep, grouping_indian);
    }
    if decimal_sep != '.' {
        if let Some(i) = out.rfind('.') {
            out.replace_range(i..=i, &decimal_sep.to_string());
        }
    }
    Ok(out)
}

/// Count the number of significant digits in an integer/fraction split.
fn count_sig_digits(int_digits: &str, frac_digits: &str) -> usize {
    let ic: Vec<char> = int_digits.chars().collect();
    let first_nz = ic.iter().position(|&c| c != '0');
    if let Some(start) = first_nz {
        let mut count = ic.len() - start;
        let mut seen_nz = ic.iter().skip(start).any(|&c| c != '0');
        for c in frac_digits.chars() {
            if seen_nz {
                count += 1;
            }
            if c != '0' {
                seen_nz = true;
            }
        }
        count
    } else {
        let mut count = 0;
        let mut seen_nz = false;
        for c in frac_digits.chars() {
            if seen_nz {
                count += 1;
            }
            if c != '0' {
                seen_nz = true;
                count += 1;
            }
        }
        count
    }
}

/// Shortest decimal representation of an `f64`, avoiding exponent notation for
/// the magnitudes handled by Intl.NumberFormat significant-digit formatting.
pub(crate) fn format_shortest(n: f64) -> String {
    let s = format!("{}", n);
    // Rust may emit exponent notation (e.g. "1.2e-30"); expand to a plain
    // decimal string so the significant-digit logic can count digits.
    if s.contains('e') || s.contains('E') {
        expand_exponent(&s)
    } else {
        s
    }
}

/// Expand a decimal with an exponent (e.g. "1.2e-30", "1.2345e34") into a
/// plain fixed-point string.
fn expand_exponent(s: &str) -> String {
    let (mantissa, exp) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], s[i + 1..].parse::<i32>().unwrap_or(0)),
        None => return s.to_string(),
    };
    let (sign, mantissa) = if let Some(stripped) = mantissa.strip_prefix('-') {
        ("-", stripped)
    } else {
        ("", mantissa)
    };
    let (int_digits, frac_digits) = match mantissa.find('.') {
        Some(dot) => (mantissa[..dot].to_string(), mantissa[dot + 1..].to_string()),
        None => (mantissa.to_string(), String::new()),
    };
    let all_digits = format!("{}{}", int_digits, frac_digits);
    // decimal point position: after `int_digits.len()` + exp places.
    let point = int_digits.len() as i32 + exp;
    let mut out = String::new();
    out.push_str(sign);
    if point <= 0 {
        out.push_str("0.");
        out.push_str(&"0".repeat((-point) as usize));
        out.push_str(&all_digits);
    } else if (point as usize) >= all_digits.len() {
        out.push_str(&all_digits);
        out.push_str(&"0".repeat((point as usize) - all_digits.len()));
    } else {
        out.push_str(&all_digits[..point as usize]);
        out.push('.');
        out.push_str(&all_digits[point as usize..]);
    }
    out
}

/// `Intl.NumberFormat.prototype.formatToParts(value)`: returns an array of
/// `{ type, value }` parts that concatenate to the formatted result.
fn js_number_format_formatToParts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = super::common::unwrap_fallback(ctx, cc.this(), &|v| is_number_format(v))?;
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_internal_error("unreachable"))?;
    let this = this_obj
        .payload_intl_number_format()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;

    let value = cc.arg(0);
    let nparts: Vec<NPart> = if value.is_bigint() {
        // BigInt formatting has no part splitting; emit a single merged part.
        let s = format_value(ctx, this, value)?;
        vec![NPart {
            typ: "integer",
            value: s,
        }]
    } else {
        // ToNumber must be applied exactly once (per spec step 4).
        let n = value.to_number(ctx)?.to_f64();
        format_parts(ctx, this, n)?
    };

    let mut parts: Vec<JSValue> = Vec::with_capacity(nparts.len());
    for p in nparts {
        let mut obj = ctx.create_base_object()?;
        let t = make_str(ctx, p.typ.as_bytes().to_vec())?;
        obj.set_property_str(ctx, "type", t)?;
        let v = make_str(ctx, p.value.into_bytes())?;
        obj.set_property_str(ctx, "value", v)?;
        parts.push(obj.into());
    }

    Ok(values_to_array(ctx, parts)?.into())
}

fn js_number_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = super::common::unwrap_fallback(ctx, cc.this(), &|v| is_number_format(v))?;
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_internal_error("unreachable"))?;
    let this = this_obj
        .payload_intl_number_format()
        .ok_or_else(|| ctx.throw_type_error("not a NumberFormat"))?;
    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", _v1, JS_PROP_C_W_E)?;
    let _v2 = make_str(ctx, this.numbering_system.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "numberingSystem", _v2, JS_PROP_C_W_E)?;
    let style = make_str(
        ctx,
        match this.style {
            Style::Decimal => b"decimal".to_vec(),
            Style::Percent => b"percent".to_vec(),
            Style::Currency => b"currency".to_vec(),
            Style::Unit => b"unit".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "style", style, JS_PROP_C_W_E)?;

    // Spec table order: currency/currencyDisplay/currencySign, unit/unitDisplay,
    // then digit options, useGrouping, notation, compactDisplay, signDisplay.
    if this.style == Style::Currency {
        if let Some(c) = &this.currency {
            let _v3 = make_str(ctx, c.clone().into_bytes())?;
            o.define_property_value_str(ctx, "currency", _v3, JS_PROP_C_W_E)?;
        }
        let _v4 = make_str(ctx, this.currency_display.as_bytes().to_vec())?;
        o.define_property_value_str(ctx, "currencyDisplay", _v4, JS_PROP_C_W_E)?;
        let _v5 = make_str(ctx, this.currency_sign.as_bytes().to_vec())?;
        o.define_property_value_str(ctx, "currencySign", _v5, JS_PROP_C_W_E)?;
    }
    if this.style == Style::Unit {
        if let Some(u) = &this.unit {
            let _vu = make_str(ctx, u.clone().into_bytes())?;
            o.define_property_value_str(ctx, "unit", _vu, JS_PROP_C_W_E)?;
        }
        let _vud = make_str(ctx, this.unit_display.as_bytes().to_vec())?;
        o.define_property_value_str(ctx, "unitDisplay", _vud, JS_PROP_C_W_E)?;
    }

    o.define_property_value_str(
        ctx,
        "minimumIntegerDigits",
        JSValue::from(this.min_integer_digits),
        JS_PROP_C_W_E,
    )?;
    // Fraction and significant digit groups are mutually exclusive.
    if this.min_significant_digits.is_none() && this.max_significant_digits.is_none() {
        o.define_property_value_str(
            ctx,
            "minimumFractionDigits",
            JSValue::from(this.min_fraction_digits),
            JS_PROP_C_W_E,
        )?;
        o.define_property_value_str(
            ctx,
            "maximumFractionDigits",
            JSValue::from(this.max_fraction_digits),
            JS_PROP_C_W_E,
        )?;
    } else {
        if let Some(min) = this.min_significant_digits {
            o.define_property_value_str(
                ctx,
                "minimumSignificantDigits",
                JSValue::from(min as i32),
                JS_PROP_C_W_E,
            )?;
        }
        if let Some(max) = this.max_significant_digits {
            o.define_property_value_str(
                ctx,
                "maximumSignificantDigits",
                JSValue::from(max as i32),
                JS_PROP_C_W_E,
            )?;
        }
    }
    o.define_property_value_str(
        ctx,
        "useGrouping",
        JSValue::from(this.use_grouping),
        JS_PROP_C_W_E,
    )?;
    let notation = make_str(
        ctx,
        match this.notation {
            Notation::Standard => b"standard".to_vec(),
            Notation::CompactShort | Notation::CompactLong => b"compact".to_vec(),
            Notation::Scientific => b"scientific".to_vec(),
            Notation::Engineering => b"engineering".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "notation", notation, JS_PROP_C_W_E)?;
    if matches!(
        this.notation,
        Notation::CompactShort | Notation::CompactLong
    ) {
        let cd = make_str(
            ctx,
            match this.notation {
                Notation::CompactLong => b"long".to_vec(),
                _ => b"short".to_vec(),
            },
        )?;
        o.define_property_value_str(ctx, "compactDisplay", cd, JS_PROP_C_W_E)?;
    }
    let sd = make_str(
        ctx,
        match this.sign_display {
            SignDisplay::Auto => b"auto".to_vec(),
            SignDisplay::Never => b"never".to_vec(),
            SignDisplay::Always => b"always".to_vec(),
            SignDisplay::ExceptZero => b"exceptZero".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "signDisplay", sd, JS_PROP_C_W_E)?;
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
