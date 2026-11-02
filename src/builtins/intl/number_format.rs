// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.NumberFormat` (§11).

use std::cmp::Ordering;

use crate::{
    ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, PayloadPtr},
    value::JSValue,
};
use icu::{decimal::DecimalFormatter, locale::Locale};

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
    pub(crate) use_grouping: UseGrouping,
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
    /// ECMA-402 `roundingIncrement`: the rounding step in units of the last
    /// digit the rounding type keeps (1 = round to that digit).
    pub(crate) rounding_increment: u32,
    pub(crate) rounding_priority: RoundingPriority,
    /// The digit type `rounding_priority` resolves to (ECMA-402
    /// `[[RoundingType]]`); fraction and significant settings are exclusive
    /// once resolved.
    pub(crate) rounding_type: RoundingType,
    /// ECMA-402 `trailingZeroDisplay`: `stripIfInteger` drops a fraction that
    /// is entirely zeros.
    pub(crate) strip_trailing_zeros: bool,
}

impl NumberFormatData {
    /// The rounding position implied by the resolved rounding type.
    const fn round_pos(&self) -> RoundPos {
        match self.rounding_type {
            RoundingType::SignificantDigits => RoundPos::Sig(match self.max_significant_digits {
                Some(v) => v,
                None => 21,
            }),
            RoundingType::FractionDigits => RoundPos::Frac(self.max_fraction_digits),
        }
    }
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
    /// v3: show the minus sign, but never a plus sign. Unlike `Auto`, a
    /// negative value that rounds to zero loses the sign.
    Negative,
}

/// ECMA-402 `roundingMode`: how the dropped digits are rounded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RoundingMode {
    /// Toward +∞ (up for positive values, down for negative ones).
    Ceil,
    /// Toward -∞.
    Floor,
    /// Away from zero.
    Expand,
    /// Toward zero (drop the tail).
    Trunc,
    /// Nearest, ties toward +∞.
    HalfCeil,
    /// Nearest, ties toward -∞.
    HalfFloor,
    /// Nearest, ties away from zero (the default).
    HalfExpand,
    /// Nearest, ties toward zero.
    HalfTrunc,
    /// Nearest, ties to the even digit.
    HalfEven,
}

impl RoundingMode {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Ceil => "ceil",
            Self::Floor => "floor",
            Self::Expand => "expand",
            Self::Trunc => "trunc",
            Self::HalfCeil => "halfCeil",
            Self::HalfFloor => "halfFloor",
            Self::HalfExpand => "halfExpand",
            Self::HalfTrunc => "halfTrunc",
            Self::HalfEven => "halfEven",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ceil" => Self::Ceil,
            "floor" => Self::Floor,
            "expand" => Self::Expand,
            "trunc" => Self::Trunc,
            "halfCeil" => Self::HalfCeil,
            "halfFloor" => Self::HalfFloor,
            "halfExpand" => Self::HalfExpand,
            "halfTrunc" => Self::HalfTrunc,
            "halfEven" => Self::HalfEven,
            _ => return None,
        })
    }
}

/// ECMA-402 `roundingPriority`: which digit setting wins when both are given.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RoundingPriority {
    /// Significant digits whenever either bound is set, otherwise fraction
    /// digits.
    Auto,
    /// The setting that keeps fewer digits.
    LessPrecision,
    /// The setting that keeps more digits.
    MorePrecision,
}

impl RoundingPriority {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::LessPrecision => "lessPrecision",
            Self::MorePrecision => "morePrecision",
        }
    }
}

/// The resolved rounding type (ECMA-402 `[[RoundingType]]`). The two settings
/// are exclusive once resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RoundingType {
    FractionDigits,
    SignificantDigits,
}

/// ECMA-402 `useGrouping`. The v3 string form also selects the grouping
/// threshold; `Auto` is the locale's CLDR default, which is what the boolean
/// form used to mean.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum UseGrouping {
    Always,
    Auto,
    /// Group only when the integer part has more than 4 digits.
    Min2,
    False,
}

impl UseGrouping {
    /// Whether grouping happens at all.
    pub(crate) const fn enabled(self) -> bool {
        !matches!(self, Self::False)
    }

    /// Whether an integer part of `int_len` digits is grouped.
    ///
    /// `min_group2` is the locale's CLDR `minimumGroupingDigits=2` flag and
    /// only affects the locale default (`Auto`).
    pub(crate) const fn groups_for(self, int_len: usize, min_group2: bool) -> bool {
        match self {
            Self::False => false,
            Self::Always => true,
            Self::Min2 => int_len > 4,
            Self::Auto => !(min_group2 && int_len == 4),
        }
    }
}

crate::def_native_object!(
    builtin,
    "NumberFormat",
    CLASS_DEF_NUMBER_FORMAT,
    class_id: (crate::JS_CLASS_INTL_NUMBER_FORMAT, crate::Atom::CONST_NumberFormat),
    crate::builtins::intl::number_format::NumberFormatData,
    js_number_format_proto_funcs,
    Some(crate::class::ClassCtorDef::new_legacy(js_number_format_constructor, 0))
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
        use_grouping: UseGrouping::Auto,
        grouping_sep,
        decimal_sep,
        percent_space,
        grouping_indian,
        min_group2,
        unit_display: "short",
        sign_display: SignDisplay::Auto,
        rounding_mode: RoundingMode::HalfExpand,
        rounding_increment: 1,
        rounding_priority: RoundingPriority::Auto,
        rounding_type: RoundingType::FractionDigits,
        strip_trailing_zeros: false,
    };

    let ns_opt: Option<&'static str> = if options.is_undefined() {
        None
    } else {
        // The option was already read (and validated) while applying the
        // options; reading it again here would show up in the observable
        // option-read order.
        apply_options(ctx, &mut data, options)?
    };

    // ECMA-402 ResolveLocale: strip `cu`/`nu` extension keys when an explicit
    // option overrides them or their value is unsupported; strip all other
    // `u`-extension keys (not relevant to NumberFormat).
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
            .ordinary_create_from_constructor_payload(
                JSValue::UNDEFINED,
                PayloadPtr::new(crate::JS_CLASS_INTL_NUMBER_FORMAT, data),
            )
            .map(JSValue::from)?;

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
    ctx.ordinary_create_from_constructor_payload(
        new_target,
        PayloadPtr::new(crate::JS_CLASS_INTL_NUMBER_FORMAT, data),
    )
    .map(JSValue::from)
}

/// Build `NumberFormatData` directly (no JS option plumbing) for internal
/// users such as `DurationFormat`, which spec-constructs fresh NumberFormat
/// instances per unit.
#[allow(clippy::too_many_arguments)]
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
        use_grouping: if use_grouping {
            UseGrouping::Always
        } else {
            UseGrouping::False
        },
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
        rounding_increment: 1,
        rounding_priority: RoundingPriority::Auto,
        rounding_type: RoundingType::FractionDigits,
        strip_trailing_zeros: false,
    })
}

fn apply_options(
    ctx: &mut ScopedContext,
    data: &mut NumberFormatData,
    options: JSValue,
) -> Result<Option<&'static str>, i32> {
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
    // (3*8alphanum *("-" (3*8alphanum))). Returned so the caller reuses it
    // instead of reading the option twice (the read order is observable).
    let ns = obj.get_property_str(ctx, "numberingSystem")?;
    let ns_option: Option<&'static str> = if ns.is_undefined() {
        None
    } else {
        let s = to_utf8_string(ctx, ns)?;
        if !is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid numberingSystem value"));
        }
        Some(Box::leak(s.into_boxed_str()) as &'static str)
    };

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

    // ── notation ─────────────────────────────────────────────────────────
    // Read before the digit options: they need it for the default digit
    // counts, and ECMA-402 reads `notation` first.
    let notation = obj.get_property_str(ctx, "notation")?;
    let mut compact_notation = false;
    if !notation.is_undefined() {
        let s = to_utf8_string(ctx, notation)?;
        data.notation = match s.as_str() {
            "standard" => Notation::Standard,
            "engineering" => Notation::Engineering,
            "scientific" => Notation::Scientific,
            "compact" => {
                compact_notation = true;
                Notation::CompactShort
            }
            _ => return Err(ctx.throw_range_error("Invalid notation value")),
        };
    }

    // ── digit options (ECMA-402 `SetNumberFormatDigitOptions`) ───────────
    // Read in spec order: the v3 options are order-observable through getters.
    let min_int = number_option(ctx, &obj, "minimumIntegerDigits", 1.0, 21.0)?;
    let min_frac_opt = number_option(ctx, &obj, "minimumFractionDigits", 0.0, 100.0)?;
    let max_frac_opt = number_option(ctx, &obj, "maximumFractionDigits", 0.0, 100.0)?;
    if let (Some(mn), Some(mx)) = (min_frac_opt, max_frac_opt)
        && mx < mn
    {
        return Err(
            ctx.throw_range_error("minimumFractionDigits must not exceed maximumFractionDigits")
        );
    }
    let min_sig_opt = number_option(ctx, &obj, "minimumSignificantDigits", 1.0, 21.0)?;
    let max_sig_opt = number_option(ctx, &obj, "maximumSignificantDigits", 1.0, 21.0)?;
    if let (Some(mn), Some(mx)) = (min_sig_opt, max_sig_opt)
        && mx < mn
    {
        return Err(ctx.throw_range_error(
            "minimumSignificantDigits must not exceed maximumSignificantDigits",
        ));
    }

    // roundingIncrement: one of the mill-friendly steps up to 5000.
    let inc = obj.get_property_str(ctx, "roundingIncrement")?;
    let mut rounding_increment = 1u32;
    if !inc.is_undefined() {
        let m = inc.to_number(ctx)?.to_f64();
        if m.is_nan() || !(1.0..=5000.0).contains(&m) {
            return Err(ctx.throw_range_error("Invalid roundingIncrement value"));
        }
        let v = m.floor() as u32;
        if !matches!(
            v,
            1 | 2 | 5 | 10 | 20 | 25 | 50 | 100 | 200 | 250 | 500 | 1000 | 2000 | 2500 | 5000
        ) {
            return Err(ctx.throw_range_error("Invalid roundingIncrement value"));
        }
        rounding_increment = v;
    }

    // roundingMode
    let mode = obj.get_property_str(ctx, "roundingMode")?;
    let mut rounding_mode = RoundingMode::HalfExpand;
    if !mode.is_undefined() {
        let s = to_utf8_string(ctx, mode)?;
        rounding_mode = RoundingMode::parse(&s)
            .ok_or_else(|| ctx.throw_range_error("Invalid roundingMode value"))?;
    }

    // roundingPriority
    let prio = obj.get_property_str(ctx, "roundingPriority")?;
    let mut rounding_priority = RoundingPriority::Auto;
    if !prio.is_undefined() {
        let s = to_utf8_string(ctx, prio)?;
        rounding_priority = match s.as_str() {
            "auto" => RoundingPriority::Auto,
            "lessPrecision" => RoundingPriority::LessPrecision,
            "morePrecision" => RoundingPriority::MorePrecision,
            _ => return Err(ctx.throw_range_error("Invalid roundingPriority value")),
        };
    }

    // trailingZeroDisplay
    let tzd = obj.get_property_str(ctx, "trailingZeroDisplay")?;
    let mut strip_trailing_zeros = false;
    if !tzd.is_undefined() {
        let s = to_utf8_string(ctx, tzd)?;
        strip_trailing_zeros = match s.as_str() {
            "auto" => false,
            "stripIfInteger" => true,
            _ => return Err(ctx.throw_range_error("Invalid trailingZeroDisplay value")),
        };
    }

    // compactDisplay is read unconditionally, right after the digit options.
    let compact_display = obj.get_property_str(ctx, "compactDisplay")?;
    let mut compact_long = false;
    if !compact_display.is_undefined() {
        let s = to_utf8_string(ctx, compact_display)?;
        compact_long = match s.as_str() {
            "long" => true,
            "short" => false,
            _ => return Err(ctx.throw_range_error("Invalid compactDisplay value")),
        };
    }
    if compact_notation {
        data.notation = if compact_long {
            Notation::CompactLong
        } else {
            Notation::CompactShort
        };
    }

    // useGrouping: v3 takes a string; the legacy booleans keep their meaning.
    let use_grouping = obj.get_property_str(ctx, "useGrouping")?;
    if !use_grouping.is_undefined() {
        data.use_grouping = parse_use_grouping(ctx, use_grouping)?;
    } else if matches!(
        data.notation,
        Notation::CompactShort | Notation::CompactLong
    ) {
        // Compact notation defaults to "min2" (four-digit integers stay
        // ungrouped); every other notation defaults to "auto".
        data.use_grouping = UseGrouping::Min2;
    }

    // signDisplay: auto | never | always | exceptZero | negative
    let sign_display = obj.get_property_str(ctx, "signDisplay")?;
    if !sign_display.is_undefined() {
        let s = to_utf8_string(ctx, sign_display)?;
        data.sign_display = match s.as_str() {
            "auto" => SignDisplay::Auto,
            "never" => SignDisplay::Never,
            "always" => SignDisplay::Always,
            "exceptZero" => SignDisplay::ExceptZero,
            "negative" => SignDisplay::Negative,
            _ => return Err(ctx.throw_range_error("Invalid signDisplay value")),
        };
    }

    // ── resolve the digit settings ───────────────────────────────────────
    if let Some(v) = min_int {
        data.min_integer_digits = v as i32;
    }
    // Style/notation default digit counts (`mnfdDefault`/`mxfdDefault`).
    let (mnfd_default, mxfd_default) = match data.style {
        Style::Percent => (0, 0),
        Style::Currency => match data.notation {
            Notation::Standard => (2, 2),
            Notation::CompactShort | Notation::CompactLong => (0, 0),
            _ => (0, 3),
        },
        _ => (0, 3),
    };
    // ECMA-402: the defaults fill in only what the options leave unspecified,
    // and a lone `maximumFractionDigits` pulls the minimum down with it
    // (`{style: "currency", maximumFractionDigits: 1}` resolves mnfd to 1).
    let (min_frac, max_frac) = match (min_frac_opt, max_frac_opt) {
        (None, None) => (mnfd_default, mxfd_default),
        (None, Some(mx)) => {
            let mx = mx as i32;
            (mx.min(mnfd_default), mx)
        }
        (Some(mn), None) => {
            let mn = mn as i32;
            (mn, mn.max(mxfd_default))
        }
        (Some(mn), Some(mx)) => (mn as i32, mx as i32),
    };
    data.min_fraction_digits = min_frac;
    data.max_fraction_digits = max_frac;

    let has_sd = min_sig_opt.is_some() || max_sig_opt.is_some();
    // `mxsd` defaults to 21 (not to `mnsd`): a lone `minimumSignificantDigits`
    // pads without capping the other end.
    let min_sig = min_sig_opt.map_or(1, |v| v as u8);
    let max_sig = max_sig_opt.map_or(21, |v| v as u8).max(min_sig);

    // Which setting rounds. `auto` prefers significant digits whenever either
    // bound is set; the explicit priorities compare the two maximums, a tie
    // going to significant digits under `lessPrecision` and to fraction digits
    // under `morePrecision`.
    let sig_max = if has_sd { max_sig as i32 } else { 21 };
    data.rounding_type = match rounding_priority {
        RoundingPriority::Auto => {
            if has_sd {
                RoundingType::SignificantDigits
            } else {
                RoundingType::FractionDigits
            }
        }
        RoundingPriority::LessPrecision => {
            if sig_max <= max_frac {
                RoundingType::SignificantDigits
            } else {
                RoundingType::FractionDigits
            }
        }
        RoundingPriority::MorePrecision => {
            if sig_max > max_frac {
                RoundingType::SignificantDigits
            } else {
                RoundingType::FractionDigits
            }
        }
    };
    if has_sd || data.rounding_type == RoundingType::SignificantDigits {
        data.min_significant_digits = Some(min_sig);
        data.max_significant_digits = Some(max_sig);
    }

    // `roundingIncrement` is only meaningful for a fully specified fraction
    // rounding: no priority, no significant digits, mnfd == mxfd.
    if rounding_increment != 1 {
        if rounding_priority != RoundingPriority::Auto {
            return Err(ctx.throw_type_error("roundingIncrement requires roundingPriority auto"));
        }
        if has_sd {
            return Err(ctx
                .throw_type_error("roundingIncrement cannot be combined with significant digits"));
        }
        if min_frac != max_frac {
            return Err(ctx.throw_range_error(
                "roundingIncrement requires equal minimum and maximum fraction digits",
            ));
        }
    }

    data.rounding_increment = rounding_increment;
    data.rounding_mode = rounding_mode;
    data.rounding_priority = rounding_priority;
    data.strip_trailing_zeros = strip_trailing_zeros;

    Ok(ns_option)
}

/// `GetNumberOption` (ECMA-402 §9.2.13): an integral option in `[min, max]`.
///
/// The range check runs on the converted *number* (before the truncation), so
/// `{roundingIncrement: 5000.1}` is out of range rather than flooring to 5000.
fn number_option(
    ctx: &mut ScopedContext,
    obj: &crate::object::Object,
    name: &str,
    min: f64,
    max: f64,
) -> Result<Option<f64>, i32> {
    let v = obj.get_property_str(ctx, name)?;
    if v.is_undefined() {
        return Ok(None);
    }
    let n = v.to_number(ctx)?.to_f64();
    if n.is_nan() || n < min || n > max {
        return Err(ctx.throw_range_error("Invalid digit option value"));
    }
    Ok(Some(n.floor()))
}

/// ECMA-402 `useGrouping`: the v3 strings, the legacy boolean, or any value
/// whose boolean form is false (0, null, "") meaning "never".
///
/// `"true"`/`"false"` are accepted as `"auto"` (the spec folds the pre-v3
/// string forms into the locale default); a truthy non-boolean is not a valid
/// grouping request and throws.
fn parse_use_grouping(ctx: &mut ScopedContext, v: JSValue) -> Result<UseGrouping, i32> {
    if let Some(b) = v.as_bool() {
        return Ok(if b {
            UseGrouping::Always
        } else {
            UseGrouping::False
        });
    }
    if let Some(s) = v.as_string(false) {
        let s = s.content().encode_utf8(false);
        return match s.as_ref() {
            "always" => Ok(UseGrouping::Always),
            "min2" => Ok(UseGrouping::Min2),
            "auto" | "true" | "false" => Ok(UseGrouping::Auto),
            "" => Ok(UseGrouping::False),
            _ => Err(ctx.throw_range_error("Invalid useGrouping value")),
        };
    }
    if v.to_bool()? {
        Err(ctx.throw_range_error("Invalid useGrouping value"))
    } else {
        Ok(UseGrouping::False)
    }
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
    make_str(ctx, s.into_bytes())
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
    if this.use_grouping.enabled() {
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
    format_f64(this, n)
}

/// Format an already-converted `f64` value (no further coercion).
fn format_f64(this: &NumberFormatData, n: f64) -> Result<String, i32> {
    let parts = format_parts(this, n)?;
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
pub(crate) fn format_parts(this: &NumberFormatData, n: f64) -> Result<Vec<NPart>, i32> {
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
            SignDisplay::Auto | SignDisplay::Negative => {
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

    // Standard notation rounds the (percent-scaled) magnitude once, in decimal.
    // The same digits decide the sign here and build the body further down.
    let mut standard_digits: Option<Digits> = None;
    if matches!(this.notation, Notation::Standard) {
        let scaled = if matches!(this.style, Style::Percent) {
            n * 100.0
        } else {
            n
        };
        let d = round_magnitude(
            scaled.abs(),
            this.round_pos(),
            this.rounding_increment,
            this.rounding_mode,
            negative,
        );
        if d.is_zero() {
            eff_zero = true;
            if !zero {
                eff_negative = false;
            }
        }
        standard_digits = Some(d);
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
                // Pre-round the mantissa in f64: the decade carry must be
                // visible here so it lands on the exponent, which the decimal
                // digit pipeline cannot feed back.
                m = round_to_fraction(m, 3);
            }
            // Rounding may carry the mantissa into the next decade(s).
            let limit = if engineering { 1000.0 } else { 10.0 };
            while m >= limit {
                m /= limit;
                exp += if engineering { 3 } else { 1 };
            }
            let body = format_fixed(
                m,
                0,
                3,
                false,
                this.grouping_sep,
                this.decimal_sep,
                1,
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
            let entries = compact_entries(&this.locale, long);
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
            let rounded = round_to_fraction(scaled, max_frac);
            // Below the smallest compact entry the value renders plain;
            // grouping follows the locale (CJK locales keep it off here, and
            // the CLDR minimumGroupingDigits=2 locales skip 4-digit integers).
            let grouped = chosen.is_none()
                && this.use_grouping.enabled()
                && !matches!(lang.as_str(), "ja" | "zh" | "yue")
                && !(this.min_group2 && (1000.0..10000.0).contains(&rounded));

            let body = format_fixed(
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
            // Produced above: standard notation always rounds the magnitude.
            let d = standard_digits.take().expect("standard digits");
            let min_int = this.min_integer_digits.max(1) as usize;
            let int_len = d.int.len().max(min_int);
            let trim = match this.rounding_type {
                RoundingType::SignificantDigits => Trim::Significant {
                    min_sig: this.min_significant_digits.unwrap_or(1),
                },
                RoundingType::FractionDigits => Trim::Fraction {
                    min_frac: this.min_fraction_digits,
                },
            };
            // Standard notation groups normally; only the locale default
            // applies `minimumGroupingDigits=2`.
            let group = match this.rounding_type {
                RoundingType::SignificantDigits => {
                    this.use_grouping.groups_for(int_len, this.min_group2)
                }
                RoundingType::FractionDigits => this.use_grouping.groups_for(int_len, false),
            };
            let body = assemble_digits(
                d.int,
                d.frac,
                this.min_integer_digits,
                trim,
                group,
                this.grouping_sep,
                this.grouping_indian,
                this.decimal_sep,
                this.strip_trailing_zeros,
            );
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
    // Both "exceptZero" and "negative" drop the sign of a value that rounds to
    // zero — and with it the accounting parentheses. "auto" keeps the locale's
    // negative pattern even for a value that rounds to zero.
    let except_zero_suppressed = matches!(
        this.sign_display,
        SignDisplay::ExceptZero | SignDisplay::Negative
    ) && eff_zero;
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
            // "negative": the minus sign only — and only for a value that is
            // actually negative after rounding (so `-0` and `-0.0001` render
            // without a sign).
            SignDisplay::Negative => {
                if negative && !eff_zero {
                    sign_val = Some("-");
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
        // "negative": keep the minus, never add a plus, and drop the sign of a
        // zero.
        SignDisplay::Negative => {
            if zero && s.starts_with('-') {
                s.remove(0);
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
fn compact_entries(locale: &Locale, long: bool) -> &'static [(i32, &'static str, &'static str)] {
    const K: &[(i32, &str, &str)] = &[(3, "", "K"), (6, "", "M"), (9, "", "B"), (12, "", "T")];
    // Indian English compacts at lakh (1e5) and crore (1e7), not at 1e6.
    const IN_SHORT: &[(i32, &str, &str)] = &[(3, "", "K"), (5, "", "L"), (7, "", "Cr")];
    const IN_LONG: &[(i32, &str, &str)] =
        &[(3, " ", "thousand"), (5, " ", "lakh"), (7, " ", "crore")];
    const K_LONG: &[(i32, &str, &str)] = &[
        (3, " ", "thousand"),
        (6, " ", "million"),
        (9, " ", "billion"),
        (12, " ", "trillion"),
    ];
    let lang = locale.id.language.to_string();
    let region = locale.id.region.map(|r| r.to_string()).unwrap_or_default();
    match lang.as_str() {
        "en" if region == "IN" => {
            if long {
                IN_LONG
            } else {
                IN_SHORT
            }
        }
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

// ---------------------------------------------------------------------------
// Decimal rounding (ECMA-402 `[[RoundingType]]` / `[[RoundingMode]]`)
// ---------------------------------------------------------------------------
//
// All rounding happens on the value's *decimal* digits, once. Formatting an
// already rounded `f64` (`(n * 10^m).round() / 10^m`) double-rounds and cannot
// see a decimal tie: `1.015 * 100 == 101.4999…` rounds down to 1.01 where the
// spec (and ICU) round `1.015` to 1.02.

/// A magnitude's decimal digits. The decimal point sits after `int.len()`
/// digits; `int` has no leading zeros beyond a lone `"0"`.
struct Digits {
    int: String,
    frac: String,
}

impl Digits {
    fn is_zero(&self) -> bool {
        self.int.bytes().chain(self.frac.bytes()).all(|b| b == b'0')
    }
}

/// Where the rounding position sits.
#[derive(Clone, Copy)]
enum RoundPos {
    /// Keep `n` fraction digits; the last kept one is the rounding position.
    Frac(i32),
    /// Keep `n` significant digits.
    Sig(u8),
}

/// Whether a formatted number keeps fraction digits or significant digits,
/// and the minimum count of each.
#[derive(Clone, Copy)]
enum Trim {
    Fraction { min_frac: i32 },
    Significant { min_sig: u8 },
}

/// Round `n` to `max_frac` fraction digits in `f64` space.
///
/// Only for the mantissa of scientific/compact notation, where the caller needs
/// the rounded *value* back to detect a decade carry into the exponent. Normal
/// formatting rounds in decimal ([`round_magnitude`]).
fn round_to_fraction(n: f64, max_frac: i32) -> f64 {
    if max_frac <= 0 {
        return n.round();
    }
    let p = 10f64.powi(max_frac);
    (n * p).round() / p
}

/// Round `mag` (finite, non-negative) at `pos` with `mode`/`increment`.
///
/// `negative` is the sign of the original value: `ceil`/`floor`/`halfCeil`/
/// `halfFloor` round toward ±∞, which the magnitude alone cannot express.
fn round_magnitude(
    mag: f64,
    pos: RoundPos,
    increment: u32,
    mode: RoundingMode,
    negative: bool,
) -> Digits {
    debug_assert!(mag.is_finite() && mag >= 0.0);
    let s = format_shortest(mag);
    let (int, frac) = match s.find('.') {
        Some(dot) => (s[..dot].to_string(), s[dot + 1..].to_string()),
        None => (s, String::new()),
    };
    let int_len = int.len();
    let mut digits: Vec<u8> = int.bytes().chain(frac.bytes()).collect();
    let keep = match pos {
        RoundPos::Frac(n) => int_len + n.max(0) as usize,
        RoundPos::Sig(n) => match digits.iter().position(|&d| d != b'0') {
            Some(first) => first + n as usize,
            // Zero has no significant digit to round at.
            None => digits.len(),
        },
    };
    // The carry past the leading digit grows the integer part by a decade.
    let int_len =
        int_len + usize::from(round_decimal(&mut digits, keep, increment, mode, negative));
    if int_len > digits.len() {
        // The rounding position sat inside the integer's trailing zeros.
        digits.resize(int_len, b'0');
    }
    let frac: String = digits
        .split_off(int_len)
        .iter()
        .map(|&b| b as char)
        .collect();
    let int: String = digits.iter().map(|&b| b as char).collect();
    Digits { int, frac }
}

/// Round the digit sequence at `keep` leading digits, in place.
///
/// `increment` is in units of the rounding position (`1` rounds that digit).
/// Returns `1` when the rounding carried past the leading digit (the caller
/// must add a decade), `0` otherwise.
fn round_decimal(
    digits: &mut Vec<u8>,
    keep: usize,
    increment: u32,
    mode: RoundingMode,
    negative: bool,
) -> u8 {
    // A value with no digit past the rounding position still needs the
    // increment check: `1.15` rounded in steps of `0.1` sits exactly halfway.
    // One zero digit makes the dropped tail explicit.
    if digits.len() <= keep {
        digits.resize(keep + 1, b'0');
    }
    let kept_mod = decimal_mod(&digits[..keep], increment);
    if kept_mod == 0 && digits[keep..].iter().all(|&d| d == b'0') {
        // Already on the grid: nothing to round.
        digits.truncate(keep);
        return 0;
    }
    let cmp = compare_remainder(kept_mod, &digits[keep..], increment);
    let up = match mode {
        RoundingMode::Expand => true,
        RoundingMode::Trunc => false,
        RoundingMode::Ceil => !negative,
        RoundingMode::Floor => negative,
        RoundingMode::HalfExpand => cmp != Ordering::Less,
        RoundingMode::HalfTrunc => cmp == Ordering::Greater,
        RoundingMode::HalfCeil => cmp == Ordering::Greater || (cmp == Ordering::Equal && !negative),
        RoundingMode::HalfFloor => cmp == Ordering::Greater || (cmp == Ordering::Equal && negative),
        RoundingMode::HalfEven => match cmp {
            Ordering::Greater => true,
            Ordering::Less => false,
            // Ties go to the even multiple of the increment.
            Ordering::Equal => decimal_mod(&digits[..keep], increment * 2) >= increment,
        },
    };
    digits.truncate(keep);
    if up {
        decimal_add_assign(digits, increment - kept_mod)
    } else {
        decimal_sub_assign(digits, kept_mod);
        0
    }
}

/// `digits` (a decimal integer) modulo `m`.
fn decimal_mod(digits: &[u8], m: u32) -> u32 {
    debug_assert!(m > 0);
    let mut r = 0u32;
    for &d in digits {
        r = (r * 10 + (d - b'0') as u32) % m;
    }
    r
}

/// Add `v` to a decimal digit string. Returns `1` when the sum carried past the
/// leading digit (a new, most significant digit was pushed).
fn decimal_add_assign(digits: &mut Vec<u8>, v: u32) -> u8 {
    let mut carry = v as u64;
    for d in digits.iter_mut().rev() {
        if carry == 0 {
            break;
        }
        let sum = (*d - b'0') as u64 + carry;
        *d = b'0' + (sum % 10) as u8;
        carry = sum / 10;
    }
    if carry == 0 {
        return 0;
    }
    let mut extra: Vec<u8> = Vec::new();
    while carry > 0 {
        extra.push(b'0' + (carry % 10) as u8);
        carry /= 10;
    }
    extra.reverse();
    digits.splice(0..0, extra);
    1
}

/// Subtract `v` from a decimal digit string. `v` must not exceed the value.
fn decimal_sub_assign(digits: &mut Vec<u8>, v: u32) {
    let mut borrow = v as u64;
    for d in digits.iter_mut().rev() {
        if borrow == 0 {
            break;
        }
        let cur = (*d - b'0') as i64 - (borrow % 10) as i64;
        borrow /= 10;
        if cur < 0 {
            *d = b'0' + (cur + 10) as u8;
            borrow += 1;
        } else {
            *d = b'0' + cur as u8;
        }
    }
    debug_assert_eq!(borrow, 0, "decimal_sub_assign underflow");
}

/// Compare the dropped digits against half an increment.
///
/// The remainder in units of the last dropped digit is `kept_mod * 10^k` plus
/// the dropped digits themselves, where `k` is their count; half a step is
/// `increment * 10^k / 2`. Both are compared as decimal strings so no width cap
/// applies (`maximumFractionDigits` allows up to 100 digits).
fn compare_remainder(kept_mod: u32, tail: &[u8], increment: u32) -> Ordering {
    let k = tail.len();
    let mut rem: Vec<u8> = kept_mod.to_string().into_bytes();
    rem.extend_from_slice(tail);
    let half: Vec<u8> = if increment % 2 == 0 {
        // half = (increment / 2) * 10^k
        let mut v = (increment / 2).to_string().into_bytes();
        v.resize(v.len() + k, b'0');
        v
    } else if k == 0 {
        // Half a step is not integral and the remainder is zero.
        return Ordering::Less;
    } else {
        // half = increment * 5 * 10^(k-1)
        let mut v = (increment * 5).to_string().into_bytes();
        v.resize(v.len() + k - 1, b'0');
        v
    };
    compare_decimal(&rem, &half)
}

/// Compare two unsigned decimal digit strings by value.
fn compare_decimal(a: &[u8], b: &[u8]) -> Ordering {
    fn trimmed(s: &[u8]) -> &[u8] {
        let first = s.iter().position(|&c| c != b'0').unwrap_or(s.len());
        &s[first..]
    }
    let (a, b) = (trimmed(a), trimmed(b));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Number of significant digits in an integer/fraction split.
///
/// Integer trailing zeros count (`1000` has 4); leading zeros do not. Zero
/// itself counts as one digit, so `minimumSignificantDigits: 3` renders it
/// `0.00`.
fn sig_digit_count(int: &str, frac: &str) -> usize {
    if let Some(i) = int.bytes().position(|b| b != b'0') {
        return (int.len() - i) + frac.len();
    }
    match frac.bytes().position(|b| b != b'0') {
        Some(j) => frac.len() - j,
        None => 1,
    }
}

/// Trim and pad the rounded digits into the number body: no sign, no affixes.
#[allow(clippy::too_many_arguments)]
fn assemble_digits(
    mut int: String,
    mut frac: String,
    min_int: i32,
    trim: Trim,
    group: bool,
    grouping_sep: char,
    grouping_indian: bool,
    decimal_sep: char,
    strip_if_integer: bool,
) -> String {
    match trim {
        Trim::Fraction { min_frac } => {
            // Trailing zeros beyond the minimum are noise ("1.50" → "1.5"),
            // and the minimum pads back up ("1" → "1.00").
            let min_frac = min_frac.max(0) as usize;
            let keep = frac.trim_end_matches('0').len().max(min_frac);
            frac.truncate(keep);
            if frac.len() < min_frac {
                frac.push_str(&"0".repeat(min_frac - frac.len()));
            }
        }
        Trim::Significant { min_sig } => {
            let mut count = sig_digit_count(&int, &frac);
            // Trailing zeros are only significant as far as the minimum needs.
            while count > min_sig as usize && frac.ends_with('0') {
                frac.pop();
                count -= 1;
            }
            while count < min_sig as usize {
                frac.push('0');
                count += 1;
            }
        }
    }
    if strip_if_integer && frac.bytes().all(|b| b == b'0') {
        frac.clear();
    }
    let min_int = min_int.max(1) as usize;
    if int.len() < min_int {
        int.insert_str(0, &"0".repeat(min_int - int.len()));
    }
    let mut out = if group {
        group_integer(&int, grouping_sep, grouping_indian)
    } else {
        int
    };
    if !frac.is_empty() {
        out.push(decimal_sep);
        out.push_str(&frac);
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn format_fixed(
    n: f64,
    min_frac: i32,
    max_frac: i32,
    group: bool,
    grouping_sep: char,
    decimal_sep: char,
    min_int: i32,
    grouping_indian: bool,
) -> Result<String, i32> {
    let negative = n < 0.0;
    // Scientific/compact callers round the mantissa themselves; rounding again
    // at the same position is a no-op, so half-expand is always correct here.
    let d = round_magnitude(
        n.abs(),
        RoundPos::Frac(max_frac),
        1,
        RoundingMode::HalfExpand,
        negative,
    );
    let body = assemble_digits(
        d.int,
        d.frac,
        min_int,
        Trim::Fraction { min_frac },
        group,
        grouping_sep,
        grouping_indian,
        decimal_sep,
        false,
    );
    Ok(if negative { format!("-{body}") } else { body })
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
            if i > 0 && (n - i).is_multiple_of(3) {
                grouped.push(grouping_sep);
            }
            grouped.push(*c);
        }
    }
    grouped
}

#[allow(clippy::too_many_arguments)]
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
        format_parts(this, n)?
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

    values_to_array(ctx, parts)
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
    // The resolved rounding type owns one digit group; the other is omitted.
    match this.rounding_type {
        RoundingType::FractionDigits => {
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
        }
        RoundingType::SignificantDigits => {
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
    }
    // `useGrouping` is a string in v3; `false` still means "never".
    let use_grouping = match this.use_grouping {
        UseGrouping::False => JSValue::from(false),
        UseGrouping::Always => make_str(ctx, b"always".to_vec())?,
        UseGrouping::Auto => make_str(ctx, b"auto".to_vec())?,
        UseGrouping::Min2 => make_str(ctx, b"min2".to_vec())?,
    };
    o.define_property_value_str(ctx, "useGrouping", use_grouping, JS_PROP_C_W_E)?;
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
            SignDisplay::Negative => b"negative".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "signDisplay", sd, JS_PROP_C_W_E)?;
    // Rounding settings (v3) are always reported.
    o.define_property_value_str(
        ctx,
        "roundingIncrement",
        JSValue::from(this.rounding_increment as i32),
        JS_PROP_C_W_E,
    )?;
    let rounding_mode = make_str(ctx, this.rounding_mode.as_str().as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "roundingMode", rounding_mode, JS_PROP_C_W_E)?;
    let rounding_priority = make_str(ctx, this.rounding_priority.as_str().as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "roundingPriority", rounding_priority, JS_PROP_C_W_E)?;
    let trailing_zero = make_str(
        ctx,
        if this.strip_trailing_zeros {
            b"stripIfInteger".to_vec()
        } else {
            b"auto".to_vec()
        },
    )?;
    o.define_property_value_str(ctx, "trailingZeroDisplay", trailing_zero, JS_PROP_C_W_E)?;
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
