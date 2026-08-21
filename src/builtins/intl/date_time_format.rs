// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.DateTimeFormat` (§12).

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
    common::{
        locale_to_string, make_bound_method, make_str, resolve_locale, to_utf8_string,
        values_to_array,
    },
    number_format::is_valid_bcp47_type,
};

#[derive(Debug)]
pub struct DateTimeFormatData {
    pub(crate) locale: Locale,
    pub(crate) weekday: Option<DateField>,
    pub(crate) era: Option<DateField>,
    pub(crate) year: Option<DateField>,
    pub(crate) month: Option<DateField>,
    pub(crate) day: Option<DateField>,
    pub(crate) hour: Option<DateField>,
    pub(crate) minute: Option<DateField>,
    pub(crate) second: Option<DateField>,
    pub(crate) fractional_second_digits: Option<u8>,
    pub(crate) hour12: Option<bool>,
    pub(crate) hour_cycle: Option<&'static str>,
    pub(crate) time_zone: String,
    /// True when no `timeZone` option was given: format in the host-local
    /// zone (like `Date`), instead of the reported "UTC" fallback.
    pub(crate) time_zone_is_local: bool,
    pub(crate) time_zone_name: Option<&'static str>,
    pub(crate) calendar: String,
    pub(crate) numbering_system: String,
}

impl GcTrace for DateTimeFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DateField {
    Narrow,
    Short,
    Long,
    TwoDigit,
    Numeric,
}

pub(crate) static CLASS_DEF_DATE_TIME_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_DateTimeFormat,
    js_date_time_format_proto_funcs,
    Some(ClassCtorDef::new(js_date_time_format_constructor, 0, true)),
);

/// Default components to apply when none are explicitly requested, used by the
/// `Date.prototype.toLocaleString` family to select the full/date/time set.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DateTimeDefaults {
    /// `Intl.DateTimeFormat`: none of year/month/day unless dateStyle/timeStyle
    /// absent and no explicit component (then year/month/day = numeric).
    Standard,
    /// `toLocaleString`: date + time components.
    All,
    /// `toLocaleDateString`: date components only.
    DateOnly,
    /// `toLocaleTimeString`: time components only.
    TimeOnly,
}

pub(crate) fn js_date_time_format_constructor(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let new_target = cc.this();
    let data = build_date_time_format_data(ctx, cc.arg(0), cc.arg(1), DateTimeDefaults::Standard)?;
    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_DATE_TIME_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// Build a `DateTimeFormatData` from `(locales, options)` with the given
/// default-component policy, used by the constructor and the Date toLocale
/// methods.
pub(crate) fn build_date_time_format_data(
    ctx: &mut ScopedContext,
    locales: JSValue,
    options: JSValue,
    defaults: DateTimeDefaults,
) -> Result<DateTimeFormatData, i32> {
    let locale = resolve_locale(ctx, locales)?;

    // Unicode extension keys (hc / ca / nu) resolved into the locale; options
    // take precedence over these.
    let mut ext_hc: Option<&'static str> = None;
    for (k, v) in locale.extensions.unicode.keywords.iter() {
        let key = k.to_string();
        let val = v.to_string();
        match key.as_str() {
            "hc" => {
                ext_hc = Some(match val.as_str() {
                    "h11" => "h11",
                    "h12" => "h12",
                    "h23" => "h23",
                    "h24" => "h24",
                    _ => continue,
                });
            }
            _ => {}
        }
    }

    let mut data = DateTimeFormatData {
        locale,
        weekday: None,
        era: None,
        year: None,
        month: None,
        day: None,
        hour: None,
        minute: None,
        second: None,
        fractional_second_digits: None,
        hour12: None,
        hour_cycle: None,
        time_zone: "UTC".to_string(),
        time_zone_is_local: true,
        time_zone_name: None,
        calendar: "gregory".to_string(),
        numbering_system: "latn".to_string(),
    };

    let options_undefined = options.is_undefined();
    let date_format = if options_undefined {
        ctx.create_base_object()?
    } else {
        options.objectify(ctx)?
    };

    // localeMatcher: lookup | best fit
    let matcher = date_format.get_property_str(ctx, "localeMatcher")?;
    if !matcher.is_undefined() {
        let s = to_utf8_string(ctx, matcher)?;
        if s != "lookup" && s != "best fit" {
            return Err(ctx.throw_range_error("Invalid localeMatcher value"));
        }
    }

    // calendar / numberingSystem must match the `type` BCP47 nonterminal.
    let calendar = date_format.get_property_str(ctx, "calendar")?;
    if !calendar.is_undefined() {
        let s = to_utf8_string(ctx, calendar)?;
        if !is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid calendar value"));
        }
    }
    let numbering_system = date_format.get_property_str(ctx, "numberingSystem")?;
    if !numbering_system.is_undefined() {
        let s = to_utf8_string(ctx, numbering_system)?;
        if !is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid numberingSystem value"));
        }
    }

    // When `options` is undefined, do not consult the (monkey-patchable)
    // Object.prototype chain — use the spec defaults directly.
    let weekday = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "weekday")?
    };
    let era = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "era")?
    };
    let year = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "year")?
    };
    let month = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "month")?
    };
    let day = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "day")?
    };
    let hour = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "hour")?
    };
    let minute = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "minute")?
    };
    let second = if options_undefined {
        JSValue::UNDEFINED
    } else {
        date_format.get_property_str(ctx, "second")?
    };
    // Only override the defaults when the corresponding option is present.
    if !weekday.is_undefined() {
        data.weekday = parse_field(ctx, weekday)?;
    }
    if !era.is_undefined() {
        data.era = parse_field(ctx, era)?;
    }
    if !year.is_undefined() {
        data.year = parse_field(ctx, year)?;
    }
    if !month.is_undefined() {
        data.month = parse_field(ctx, month)?;
    }
    if !day.is_undefined() {
        data.day = parse_field(ctx, day)?;
    }
    if !hour.is_undefined() {
        data.hour = parse_time_field(ctx, hour)?;
    }
    if !minute.is_undefined() {
        data.minute = parse_time_field(ctx, minute)?;
    }
    if !second.is_undefined() {
        data.second = parse_time_field(ctx, second)?;
    }
    let fsd = date_format.get_property_str(ctx, "fractionalSecondDigits")?;
    if !fsd.is_undefined() {
        // Per spec: ToNumber, then an integer 1..=3 is required.
        let n = fsd.to_number(ctx)?.to_f64();
        if !matches!(n, 1.0 | 2.0 | 3.0) {
            return Err(ctx.throw_range_error("Invalid fractionalSecondDigits value"));
        }
        data.fractional_second_digits = Some(n as u8);
    }
    let day_period = date_format.get_property_str(ctx, "dayPeriod")?;
    let tzn = date_format.get_property_str(ctx, "timeZoneName")?;
    let has_date_component = !weekday.is_undefined()
        || !era.is_undefined()
        || !year.is_undefined()
        || !month.is_undefined()
        || !day.is_undefined();
    let has_time_component = !hour.is_undefined()
        || !minute.is_undefined()
        || !second.is_undefined()
        || !fsd.is_undefined()
        || !day_period.is_undefined();
    let has_explicit_components = has_date_component || has_time_component || !tzn.is_undefined();
    // dateStyle / timeStyle (validated below); defaults only apply when neither
    // is present.
    let date_style = date_format.get_property_str(ctx, "dateStyle")?;
    let time_style = date_format.get_property_str(ctx, "timeStyle")?;

    // Decide whether to default the date and/or time components, following
    // ECMA-402 CreateDateTimeFormat's `required` handling. A dimension is
    // defaulted when it is required and none of its components was requested.
    // `Intl.DateTimeFormat` (Standard) defaults the date components only when
    // no component whatsoever was requested; the Date `toLocale*` methods
    // default their required dimension(s) when the respective dimension is
    // empty.
    let (default_date, default_time) = match defaults {
        DateTimeDefaults::Standard => (!has_explicit_components, false),
        DateTimeDefaults::All => {
            if options_undefined {
                (true, true)
            } else {
                (!has_explicit_components, !has_explicit_components)
            }
        }
        DateTimeDefaults::DateOnly => {
            if options_undefined {
                (true, false)
            } else {
                (!has_date_component, false)
            }
        }
        DateTimeDefaults::TimeOnly => {
            if options_undefined {
                (false, true)
            } else {
                (false, !has_time_component)
            }
        }
    };
    if date_style.is_undefined() && time_style.is_undefined() {
        if default_date {
            data.year = Some(DateField::Numeric);
            data.month = Some(DateField::Numeric);
            data.day = Some(DateField::Numeric);
        }
        if default_time {
            data.hour = Some(DateField::Numeric);
            data.minute = Some(DateField::Numeric);
            data.second = Some(DateField::Numeric);
        }
    }

    // hour12 / hourCycle validation.
    let hour12 = date_format.get_property_str(ctx, "hour12")?;
    if !hour12.is_undefined() {
        data.hour12 = Some(hour12.to_bool()?);
    }
    let hour_cycle = date_format.get_property_str(ctx, "hourCycle")?;
    if !hour_cycle.is_undefined() {
        let s = to_utf8_string(ctx, hour_cycle)?;
        data.hour_cycle = Some(match s.as_str() {
            "h11" => "h11",
            "h12" => "h12",
            "h23" => "h23",
            "h24" => "h24",
            _ => return Err(ctx.throw_range_error("Invalid hourCycle value")),
        });
    } else {
        // Fall back to the unicode `hc` extension.
        data.hour_cycle = ext_hc;
    }

    // dateStyle / timeStyle. If either is set, explicit format components are
    // a TypeError conflict.
    if (!date_style.is_undefined() || !time_style.is_undefined()) && has_explicit_components {
        return Err(ctx.throw_type_error("Cannot mix dateStyle/timeStyle with components"));
    }
    if !date_style.is_undefined() {
        let s = to_utf8_string(ctx, date_style)?;
        if s != "full" && s != "long" && s != "medium" && s != "short" {
            return Err(ctx.throw_range_error("Invalid dateStyle value"));
        }
    }
    if !time_style.is_undefined() {
        let s = to_utf8_string(ctx, time_style)?;
        if s != "full" && s != "long" && s != "medium" && s != "short" {
            return Err(ctx.throw_range_error("Invalid timeStyle value"));
        }
    }

    // Map dateStyle / timeStyle to the concrete component set.
    if !date_style.is_undefined() {
        let s = to_utf8_string(ctx, date_style)?;
        match s.as_str() {
            "full" | "long" => {
                data.weekday = Some(DateField::Long);
            }
            _ => {}
        }
        data.year = Some(DateField::Numeric);
        data.month = Some(DateField::Numeric);
        data.day = Some(DateField::Numeric);
    }
    if !time_style.is_undefined() {
        let s = to_utf8_string(ctx, time_style)?;
        match s.as_str() {
            "full" | "long" => {
                data.second = Some(DateField::Numeric);
            }
            _ => {}
        }
        data.hour = Some(DateField::Numeric);
        data.minute = Some(DateField::Numeric);
    }

    // formatMatcher: basic | best fit.
    let fmt_matcher = date_format.get_property_str(ctx, "formatMatcher")?;
    if !fmt_matcher.is_undefined() {
        let s = to_utf8_string(ctx, fmt_matcher)?;
        if s != "basic" && s != "best fit" {
            return Err(ctx.throw_range_error("Invalid formatMatcher value"));
        }
    }

    // timeZoneName: short | long | shortOffset | longOffset | shortGeneric | longGeneric.
    let tz_name = date_format.get_property_str(ctx, "timeZoneName")?;
    if !tz_name.is_undefined() {
        let s = to_utf8_string(ctx, tz_name)?;
        match s.as_str() {
            "short" | "long" | "shortOffset" | "longOffset" | "shortGeneric" | "longGeneric" => {}
            _ => return Err(ctx.throw_range_error("Invalid timeZoneName value")),
        }
        data.time_zone_name = Some(match s.as_str() {
            "short" => "short",
            "long" => "long",
            _ => "short",
        });
    }

    // timeZone: accept IANA names (containing "/"), UTC/GMT, and offset
    // notations; reject empty, non-ASCII, and legacy 3-letter abbreviations.
    let tz = date_format.get_property_str(ctx, "timeZone")?;
    data.time_zone_is_local = tz.is_undefined();
    if !tz.is_undefined() {
        let s = to_utf8_string(ctx, tz)?;
        if !is_valid_time_zone(&s) {
            return Err(ctx.throw_range_error("Invalid time zone"));
        }
        data.time_zone = canonicalize_time_zone(&s);
    }

    // ECMA-402 ResolveLocale: strip the `hc`/`ca`/`nu` extension keys when an
    // explicit option overrides them or their value is unsupported; strip all
    // other `u`-extension keys (they are not relevant to DateTimeFormat).
    let calendar_opt = if calendar.is_undefined() {
        None
    } else {
        Some(to_utf8_string(ctx, calendar)?)
    };
    let numbering_opt = if numbering_system.is_undefined() {
        None
    } else {
        Some(to_utf8_string(ctx, numbering_system)?)
    };
    let hc_opt = if options_undefined {
        None
    } else if !hour12.is_undefined() {
        let h12 = hour12.to_bool()?;
        Some(if h12 { "h12" } else { "h23" }.to_string())
    } else if !hour_cycle.is_undefined() {
        let s = to_utf8_string(ctx, hour_cycle)?;
        Some(
            match s.as_str() {
                "h11" => "h11",
                "h12" => "h12",
                "h23" => "h23",
                "h24" => "h24",
                _ => unreachable!(),
            }
            .to_string(),
        )
    } else {
        None
    };
    let hc_static: Option<&'static str> = hc_opt
        .as_deref()
        .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
    let ca_static: Option<&'static str> = calendar_opt
        .as_deref()
        .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
    let nu_static: Option<&'static str> = numbering_opt
        .as_deref()
        .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
    data.locale = super::common::resolve_locale_keys(
        data.locale,
        &[("ca", ca_static), ("nu", nu_static), ("hc", hc_static)],
    );

    // Effective calendar: explicit valid option > `ca` extension > gregory.
    // Calendar values are case-folded (e.g. ISO8601 → iso8601).
    let ca_ext = super::common::locale_extension_value(&data.locale, "ca");
    data.calendar = if let Some(opt) = ca_static {
        let lower = opt.to_ascii_lowercase();
        if super::common::supported_extension_key_value("ca", &lower) {
            canonicalize_calendar(&lower).to_string()
        } else if let Some(ext) = ca_ext {
            canonicalize_calendar(&ext.to_ascii_lowercase()).to_string()
        } else {
            "gregory".to_string()
        }
    } else if let Some(ext) = ca_ext {
        canonicalize_calendar(&ext.to_ascii_lowercase()).to_string()
    } else {
        "gregory".to_string()
    };
    // Effective numbering system: explicit valid option > `nu` extension > latn.
    let nu_ext = super::common::locale_extension_value(&data.locale, "nu");
    data.numbering_system = if let Some(opt) = nu_static {
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

    Ok(data)
}

/// Canonicalize a calendar name (CLDR `ca` type aliases).
fn canonicalize_calendar(s: &str) -> &'static str {
    match s {
        "islamicc" => "islamic-civil",
        "gregorian" => "gregory",
        "ethiopic-amete-alem" => "ethioaa",
        "islamic-civil" => "islamic-civil",
        other => Box::leak(other.to_owned().into_boxed_str()) as &'static str,
    }
}

/// Heuristic validation of a time zone identifier: IANA names contain "/",
/// UTC/GMT and offset notations (±HH[:MM]) are accepted; empty, non-ASCII,
/// and legacy 3-letter abbreviations are rejected.
fn is_valid_time_zone(s: &str) -> bool {
    if s.is_empty() || !s.is_ascii() {
        return false;
    }
    let upper = s.to_ascii_uppercase();
    if upper == "UTC" || upper == "GMT" || s.starts_with("Etc/") {
        return true;
    }
    if s.starts_with('+') || s.starts_with('-') {
        // Offset notation: ±HH, ±HHMM, ±HH:MM.
        let body = &s[1..];
        if body.is_empty() {
            return false;
        }
        // Reject malformed forms such as "+0:003".
        let (hh_s, mm_s) = if let Some(colon) = body.find(':') {
            if body.matches(':').count() != 1 {
                return false;
            }
            let (l, r) = (&body[..colon], &body[colon + 1..]);
            if l.len() != 2 || r.len() != 2 {
                return false;
            }
            (l, r)
        } else {
            match body.len() {
                2 => (body, "00"),
                4 => (&body[..2], &body[2..]),
                _ => return false,
            }
        };
        if !hh_s.bytes().all(|c| c.is_ascii_digit()) || !mm_s.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let hh: u32 = hh_s.parse().unwrap_or(99);
        let mm: u32 = mm_s.parse().unwrap_or(99);
        return hh <= 23 && mm <= 59;
    }
    if s.contains('/') {
        return true;
    }
    false
}

/// Canonicalize a time zone identifier: UTC/GMT → "UTC", offsets → ±HH:MM.
fn canonicalize_time_zone(s: &str) -> String {
    let upper = s.to_ascii_uppercase();
    if upper == "UTC" || upper == "GMT" {
        return "UTC".to_string();
    }
    if (s.starts_with('+') || s.starts_with('-'))
        && s.chars()
            .all(|c| c.is_ascii_digit() || c == '+' || c == '-' || c == ':')
    {
        let (sign, body) = (&s[..1], &s[1..]);
        let digits: String = body.chars().filter(|c| *c != ':').collect();
        let (hh, mm) = if digits.len() == 4 {
            (&digits[..2], &digits[2..])
        } else {
            (digits.as_str(), "00")
        };
        // A zero offset ("-00", "+00", "-00:00", ...) is canonicalized to "+00:00".
        if hh == "00" && mm == "00" {
            return "+00:00".to_string();
        }
        return format!("{}{}:{}", sign, hh, mm);
    }
    s.to_string()
}

fn parse_field(ctx: &mut ScopedContext, v: JSValue) -> Result<Option<DateField>, i32> {
    if v.is_undefined() {
        return Ok(None);
    }
    let s = to_utf8_string(ctx, v)?;
    Ok(Some(match s.as_str() {
        "narrow" => DateField::Narrow,
        "short" => DateField::Short,
        "long" => DateField::Long,
        "2-digit" => DateField::TwoDigit,
        "numeric" => DateField::Numeric,
        _ => return Err(ctx.throw_range_error("Invalid date/time component value")),
    }))
}

/// Parse hour/minute/second, which only accept "numeric" or "2-digit".
fn parse_time_field(ctx: &mut ScopedContext, v: JSValue) -> Result<Option<DateField>, i32> {
    if v.is_undefined() {
        return Ok(None);
    }
    let s = to_utf8_string(ctx, v)?;
    Ok(Some(match s.as_str() {
        "numeric" => DateField::Numeric,
        "2-digit" => DateField::TwoDigit,
        _ => return Err(ctx.throw_range_error("Invalid date/time component value")),
    }))
}

fn js_date_time_format_format_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    if !is_date_time_format(&this_val) {
        return Err(ctx.throw_type_error("not a DateTimeFormat"));
    }
    // The bound `format` function is anonymous (name is the empty string).
    make_bound_method(ctx, this_val, js_date_time_format_format_bound, "", 1)
}

fn is_date_time_format(v: &JSValue) -> bool {
    v.as_object()
        .is_some_and(|o| o.payload().intl_date_time_format().is_some())
}

fn js_date_time_format_format_bound(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = cc.data(0);
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_date_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let arg = cc.arg(0);
    let provided = cc.length() > 0 && !arg.is_undefined();
    let s = format_date(ctx, this, arg, provided)?;
    Ok(make_str(ctx, s.into_bytes())?)
}

pub(crate) fn format_date(
    ctx: &mut ScopedContext,
    this: &DateTimeFormatData,
    v: JSValue,
    arg_provided: bool,
) -> Result<String, i32> {
    // When `format`/`formatToParts` is called with no argument, use the
    // current time (ECMA-402 `format`: "If date is not provided, let date be
    // Call(%Date.now%, undefined)").
    let ms = if arg_provided {
        v.to_number(ctx)?.to_f64()
    } else {
        crate::builtins::date::Datetime::now().timestamp() as f64
    };
    // ECMA-402 TimeClip: NaN or outside ±8.64e15 ms throws RangeError.
    if ms.is_nan() || ms.is_infinite() || ms < -8.64e15 || ms > 8.64e15 {
        return Err(ctx.throw_range_error("Invalid time value"));
    }
    // TimeClip applies ToInteger (truncate toward zero, with -0 → +0).
    let ms_clip = ms.trunc();
    let ms_clip = if ms_clip == 0.0 { 0.0 } else { ms_clip };
    let secs = (ms_clip / 1000.0).trunc() as i64;
    let millis = (ms_clip - (secs as f64 * 1000.0)) as i64;
    // Apply the (offset) time zone: `+HH:MM` means local time = UTC + offset.
    // IANA names are not resolved; they fall back to UTC. With no explicit
    // timeZone option, use the host-local zone like `Date` does.
    let offset_secs = if this.time_zone_is_local {
        -(crate::builtins::date::get_timezone_offset(secs * 1000)) as i64 * 60
    } else {
        time_zone_offset_seconds(&this.time_zone)
    };
    let local_secs = secs + offset_secs;

    let mut out = String::new();

    if let Some(weekday) = this.weekday {
        let wd = weekday_name(weekday, weekday_index(local_secs));
        out.push_str(&wd);
    }
    let year = year_from_epoch(local_secs);
    let month = month_from_epoch(local_secs);
    let day = day_from_epoch(local_secs);

    let cyclic = if is_cyclic_calendar(&this.calendar)
        && (this.year.is_some() || this.month.is_some() || this.day.is_some())
    {
        Some(cyclic_date_parts(local_secs))
    } else {
        None
    };

    let mut parts: Vec<String> = Vec::new();
    if this.era.is_some() {
        parts.push(if year <= 0 { "BC" } else { "AD" }.to_string());
    }
    if let Some(f) = this.year {
        match &cyclic {
            Some((rel, name, _, _)) => parts.push(format!("{rel}{name}年")),
            None => parts.push(format_year(f, year)),
        }
    }
    if let Some(f) = this.month {
        match &cyclic {
            Some((_, _, m, _)) => parts.push(format_lunar_month(f, *m)),
            None => parts.push(format_month(f, month)),
        }
    }
    if let Some(f) = this.day {
        match &cyclic {
            Some((_, _, _, d)) => parts.push(format_day(f, *d as u32)),
            None => parts.push(format_day(f, day)),
        }
    }
    out.push_str(&parts.join("/"));

    if this.hour.is_some() || this.minute.is_some() || this.second.is_some() {
        let (h, m, s) = time_from_epoch(local_secs, millis);
        let use_12h = match this.hour12 {
            Some(v) => v,
            None => match this.hour_cycle {
                Some("h11") | Some("h12") => true,
                Some("h23") | Some("h24") => false,
                _ => this.locale.id.language.to_string() != "ja",
            },
        };
        let (h12, suffix) = if use_12h {
            if h == 0 {
                (12, " AM")
            } else if h < 12 {
                (h, " AM")
            } else if h == 12 {
                (12, " PM")
            } else {
                (h - 12, " PM")
            }
        } else {
            (h, "")
        };
        let mut t = String::new();
        if let Some(f) = this.hour {
            t.push_str(&format_two(f, h12));
        }
        if let Some(_f) = this.minute {
            if !t.is_empty() {
                t.push(':');
            }
            // CLDR time patterns always render minutes/seconds 2-digit.
            t.push_str(&format!("{:02}", m));
        }
        if let Some(_f) = this.second {
            if !t.is_empty() {
                t.push(':');
            }
            t.push_str(&format!("{:02}", s));
            if let Some(fsd) = this.fractional_second_digits {
                t.push(fraction_separator(&this.numbering_system));
                let frac = millis / 10_i64.pow(3 - fsd as u32);
                t.push_str(&format!("{:0width$}", frac, width = fsd as usize));
            }
        }
        t.push_str(suffix);
        if !parts.is_empty() && !t.is_empty() {
            out.push(' ');
        }
        out.push_str(&t);
    }
    let out = super::number_format::translate_digits(&out, &this.numbering_system);
    Ok(out)
}

/// Decimal separator used when rendering fractional seconds: U+066B for the
/// Arabic-Indic digits, ASCII '.' elsewhere.
fn fraction_separator(ns: &str) -> char {
    if ns == "arab" { '\u{066b}' } else { '.' }
}

/// Parse a time zone into total offset seconds from UTC. Handles offset
/// notations (±HH[:MM]) and the GMT/Etc/GMT families (where Etc/GMT-3 means
/// UTC+3). IANA region names and UTC/GMT yield 0.
fn time_zone_offset_seconds(tz: &str) -> i64 {
    let upper = tz.to_ascii_uppercase();
    // `Etc/GMT-N` and `Etc/GMT+N` (note the reversed sign) and `GMT±N`.
    if let Some(rest) = upper.strip_prefix("ETC/GMT") {
        if rest.is_empty() {
            return 0;
        }
        if let Some(num) = rest.strip_prefix('-') {
            return parse_gmt_seconds(num); // Etc/GMT-3 => UTC+3
        }
        if let Some(num) = rest.strip_prefix('+') {
            return -parse_gmt_seconds(num); // Etc/GMT+3 => UTC-3
        }
        return 0;
    }
    if let Some(rest) = upper.strip_prefix("GMT") {
        if rest.is_empty() {
            return 0;
        }
        if let Some(num) = rest.strip_prefix('+') {
            return parse_gmt_seconds(num);
        }
        if let Some(num) = rest.strip_prefix('-') {
            return -parse_gmt_seconds(num);
        }
        return 0;
    }
    // Direct offset notation: ±HH, ±HHMM, ±HH:MM.
    if !(tz.starts_with('+') || tz.starts_with('-')) {
        return 0;
    }
    let (sign, body) = (&tz[..1], &tz[1..]);
    let digits: String = body.chars().filter(|c| *c != ':').collect();
    if digits.len() != 2 && digits.len() != 4 {
        return 0;
    }
    let hh: i64 = digits[..2].parse().unwrap_or(0);
    let mm: i64 = if digits.len() == 4 {
        digits[2..].parse().unwrap_or(0)
    } else {
        0
    };
    let secs = hh * 3600 + mm * 60;
    if sign == "-" { -secs } else { secs }
}

/// Parse "HH" or "HHMM" (optionally with colon) after a GMT/Etc/GMT prefix.
fn parse_gmt_seconds(num: &str) -> i64 {
    let digits: String = num.chars().filter(|c| *c != ':').collect();
    let hh: i64 = digits
        .get(..2.min(digits.len()))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mm: i64 = if digits.len() >= 4 {
        digits[2..4].parse().unwrap_or(0)
    } else {
        0
    };
    hh * 3600 + mm * 60
}

/// Calendars whose years are identified by a sexagenary name plus the related
/// ISO year (ECMA-402 `relatedYear`/`yearName`) instead of a year number.
fn is_cyclic_calendar(cal: &str) -> bool {
    cal == "chinese" || cal == "dangi"
}

/// Related ISO year, stem-branch name (e.g. 2019, "己亥"), and lunar month/day
/// of the Chinese calendar year containing the given epoch instant.
fn cyclic_date_parts(secs: i64) -> (i32, String, u8, u8) {
    const STEMS: [&str; 10] = ["甲", "乙", "丙", "丁", "戊", "己", "庚", "辛", "壬", "癸"];
    const BRANCHES: [&str; 12] = [
        "子", "丑", "寅", "卯", "辰", "巳", "午", "未", "申", "酉", "戌", "亥",
    ];
    let days = secs.div_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    let iso =
        icu::calendar::Date::try_new_iso(y as i32, m as u8, d as u8).expect("civil date is valid");
    let cn = iso.to_calendar(icu::calendar::cal::ChineseTraditional::new());
    let rel = cn.cyclic_year().related_iso;
    // The cycle is 1-based with 甲子 at ISO year 4.
    let idx = (rel - 4).rem_euclid(60) as usize;
    (
        rel,
        STEMS[idx % 10].to_string() + BRANCHES[idx % 12],
        cn.month().number(),
        cn.day_of_month().0,
    )
}

// ponytail: lunar month names (CLDR) unsupported; numeric fallback for
// narrow/short/long styles.
fn format_lunar_month(f: DateField, m: u8) -> String {
    match f {
        DateField::TwoDigit => format!("{:02}", m),
        _ => m.to_string(),
    }
}

fn format_year(f: DateField, y: i64) -> String {
    let _ = f;
    if y <= 0 {
        // Proleptic Gregorian: no year 0. Astronomical 0 = 1 BC, -1 = 2 BC, etc.
        format!("{} BC", 1 - y)
    } else {
        y.to_string()
    }
}

fn format_month(f: DateField, m: u32) -> String {
    match f {
        DateField::Narrow => month_names_narrow()[m as usize - 1].to_string(),
        DateField::Short => month_names_short()[m as usize - 1].to_string(),
        DateField::Long => month_names_long()[m as usize - 1].to_string(),
        DateField::TwoDigit => format!("{:02}", m),
        DateField::Numeric => m.to_string(),
    }
}

fn format_day(f: DateField, d: u32) -> String {
    match f {
        DateField::TwoDigit => format!("{:02}", d),
        _ => d.to_string(),
    }
}

fn format_two(f: DateField, v: i64) -> String {
    match f {
        DateField::TwoDigit => format!("{:02}", v),
        _ => v.to_string(),
    }
}

fn weekday_index(secs: i64) -> usize {
    // 1970-01-01 is Thursday. days since epoch mod 7.
    let days = secs.div_euclid(86400);
    let idx = ((days + 4).rem_euclid(7)) as usize; // 0=Sunday
    idx
}

fn weekday_name(f: DateField, idx: usize) -> String {
    match f {
        DateField::Narrow => ["S", "M", "T", "W", "T", "F", "S"][idx].to_string(),
        DateField::Short => ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][idx].to_string(),
        DateField::Long => [
            "Sunday",
            "Monday",
            "Tuesday",
            "Wednesday",
            "Thursday",
            "Friday",
            "Saturday",
        ][idx]
            .to_string(),
        _ => idx.to_string(),
    }
}

fn month_names_narrow() -> [&'static str; 12] {
    ["J", "F", "M", "A", "M", "J", "J", "A", "S", "O", "N", "D"]
}
fn month_names_short() -> [&'static str; 12] {
    [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
}
fn month_names_long() -> [&'static str; 12] {
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ]
}

// ── Simple epoch date arithmetic (proleptic Gregorian, in UTC) ──
fn year_from_epoch(secs: i64) -> i64 {
    let days = secs.div_euclid(86400);
    let year = 1970 + days.div_euclid(365);
    // refine using average 365.2425
    let (y, _m, _d) = civil_from_days(days);
    let _ = year;
    y
}

fn month_from_epoch(secs: i64) -> u32 {
    let days = secs.div_euclid(86400);
    civil_from_days(days).1
}

fn day_from_epoch(secs: i64) -> u32 {
    let days = secs.div_euclid(86400);
    civil_from_days(days).2
}

fn time_from_epoch(secs: i64, millis: i64) -> (i64, i64, i64) {
    let s = secs.rem_euclid(86400);
    let h = s / 3600;
    let m = (s % 3600) / 60;
    let sec = s % 60;
    let _ = millis;
    (h, m, sec)
}

/// Convert days since epoch to (year, month, day). Howard Hinnant algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn js_date_time_format_format_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_date_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let arg = cc.arg(0);
    let provided = cc.length() > 0 && !arg.is_undefined();
    let fields = format_fields(ctx, this, arg, provided)?;
    let mut parts = Vec::new();
    for (ty, val) in fields {
        let mut o = ctx.create_base_object()?;
        let _t = make_str(ctx, ty.as_bytes().to_vec())?;
        o.set_property_str(ctx, "type", _t)?;
        let _v = make_str(ctx, val.into_bytes())?;
        o.set_property_str(ctx, "value", _v)?;
        parts.push(o.into());
    }
    Ok(values_to_array(ctx, parts)?)
}

fn js_date_time_format_format_range_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_date_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let (x, y) = (cc.arg(0), cc.arg(1));
    if x.is_undefined() || y.is_undefined() {
        return Err(ctx.throw_type_error("formatRangeToParts requires two arguments"));
    }
    for v in [x, y] {
        let ms = v.to_number(ctx)?.to_f64();
        if ms.is_nan() || ms.is_infinite() || !(-8.64e15..=8.64e15).contains(&ms) {
            return Err(ctx.throw_range_error("Invalid time value"));
        }
    }
    let fields1 = format_fields(ctx, this, x, true)?;
    let fields2 = format_fields(ctx, this, y, true)?;

    // Merge the two part lists: a common prefix is "shared", the diverging
    // remainder is attributed to its own range.
    let mut merged: Vec<(String, String, &'static str)> = Vec::new();
    let mut i = 0;
    while i < fields1.len() && i < fields2.len() && fields1[i] == fields2[i] {
        merged.push((fields1[i].0.clone(), fields1[i].1.clone(), "shared"));
        i += 1;
    }
    for (ty, val) in &fields1[i..] {
        merged.push((ty.clone(), val.clone(), "startRange"));
    }
    let has_split = (!fields1[i..].is_empty()) && (!fields2[i..].is_empty());
    if has_split {
        merged.push(("literal".to_string(), " \u{2013} ".to_string(), "shared"));
    }
    for (ty, val) in &fields2[i..] {
        merged.push((ty.clone(), val.clone(), "endRange"));
    }

    let mut parts = Vec::new();
    for (ty, val, source) in merged {
        let mut o = ctx.create_base_object()?;
        let _t = make_str(ctx, ty.as_bytes().to_vec())?;
        o.set_property_str(ctx, "type", _t)?;
        let _v = make_str(ctx, val.into_bytes())?;
        o.set_property_str(ctx, "value", _v)?;
        let _s = make_str(ctx, source.as_bytes().to_vec())?;
        o.set_property_str(ctx, "source", _s)?;
        parts.push(o.into());
    }
    Ok(values_to_array(ctx, parts)?)
}

/// Produce `(type, value)` parts for a date/time value, mirroring
/// `format_date` but splitting each component into its own part with literal
/// separators between them.
fn format_fields(
    ctx: &mut ScopedContext,
    this: &DateTimeFormatData,
    v: JSValue,
    arg_provided: bool,
) -> Result<Vec<(String, String)>, i32> {
    let ms = if arg_provided {
        v.to_number(ctx)?.to_f64()
    } else {
        crate::builtins::date::Datetime::now().timestamp() as f64
    };
    if ms.is_nan() || ms.is_infinite() || ms < -8.64e15 || ms > 8.64e15 {
        return Err(ctx.throw_range_error("Invalid time value"));
    }
    let ms_clip = ms.trunc();
    let ms_clip = if ms_clip == 0.0 { 0.0 } else { ms_clip };
    let secs = (ms_clip / 1000.0).trunc() as i64;
    let millis = (ms_clip - (secs as f64 * 1000.0)) as i64;
    let offset_secs = if this.time_zone_is_local {
        -(crate::builtins::date::get_timezone_offset(secs * 1000)) as i64 * 60
    } else {
        time_zone_offset_seconds(&this.time_zone)
    };
    let local_secs = secs + offset_secs;

    let mut parts: Vec<(String, String)> = Vec::new();
    let mut date_parts: Vec<(String, String)> = Vec::new();
    if let Some(weekday) = this.weekday {
        date_parts.push((
            "weekday".to_string(),
            weekday_name(weekday, weekday_index(local_secs)),
        ));
    }
    let year = year_from_epoch(local_secs);
    let month = month_from_epoch(local_secs);
    let day = day_from_epoch(local_secs);
    let cyclic = if is_cyclic_calendar(&this.calendar)
        && (this.year.is_some() || this.month.is_some() || this.day.is_some())
    {
        Some(cyclic_date_parts(local_secs))
    } else {
        None
    };
    if this.era.is_some() {
        date_parts.push((
            "era".to_string(),
            if year <= 0 { "BC" } else { "AD" }.to_string(),
        ));
    }
    if let Some(f) = this.year {
        match &cyclic {
            Some((rel, name, _, _)) => {
                date_parts.push(("relatedYear".to_string(), rel.to_string()));
                date_parts.push(("yearName".to_string(), name.clone()));
                date_parts.push(("literal".to_string(), "年".to_string()));
            }
            None => date_parts.push(("year".to_string(), format_year(f, year))),
        }
    }
    if let Some(f) = this.month {
        match &cyclic {
            Some((_, _, m, _)) => date_parts.push(("month".to_string(), format_lunar_month(f, *m))),
            None => date_parts.push(("month".to_string(), format_month(f, month))),
        }
    }
    if let Some(f) = this.day {
        match &cyclic {
            Some((_, _, _, d)) => date_parts.push(("day".to_string(), format_day(f, *d as u32))),
            None => date_parts.push(("day".to_string(), format_day(f, day))),
        }
    }

    if !date_parts.is_empty() {
        for (i, (ty, val)) in date_parts.iter().enumerate() {
            // The relatedYear/yearName/literal(年) group is joined without
            // separators.
            if i > 0 && ty != "yearName" && ty != "literal" {
                parts.push(("literal".to_string(), "/".to_string()));
            }
            parts.push((ty.clone(), val.clone()));
        }
    }

    if this.hour.is_some() || this.minute.is_some() || this.second.is_some() {
        let (h, m, s) = time_from_epoch(local_secs, millis);
        // Use the locale's default 12-hour cycle (h12 for most, h11 for ja)
        // when no explicit hour12 / hourCycle is given.
        let use_12h = match this.hour12 {
            Some(v) => v,
            None => match this.hour_cycle {
                Some("h11") | Some("h12") => true,
                Some("h23") | Some("h24") => false,
                _ => this.locale.id.language.to_string() != "ja",
            },
        };
        let (h12, suffix) = if use_12h {
            if h == 0 {
                (12, " AM")
            } else if h < 12 {
                (h, " AM")
            } else if h == 12 {
                (12, " PM")
            } else {
                (h - 12, " PM")
            }
        } else {
            (h, "")
        };
        if !parts.is_empty() {
            parts.push(("literal".to_string(), " ".to_string()));
        }
        let mut time_parts: Vec<(String, String)> = Vec::new();
        if let Some(f) = this.hour {
            time_parts.push(("hour".to_string(), format_two(f, h12)));
        }
        if let Some(_f) = this.minute {
            if !time_parts.is_empty() {
                parts.push(("literal".to_string(), ":".to_string()));
            }
            // CLDR time patterns always render minutes/seconds 2-digit.
            time_parts.push(("minute".to_string(), format!("{:02}", m)));
        }
        if let Some(_f) = this.second {
            if !time_parts.is_empty() {
                parts.push(("literal".to_string(), ":".to_string()));
            }
            time_parts.push(("second".to_string(), format!("{:02}", s)));
            if let Some(fsd) = this.fractional_second_digits {
                parts.push((
                    "literal".to_string(),
                    fraction_separator(&this.numbering_system).to_string(),
                ));
                let frac = millis / 10_i64.pow(3 - fsd as u32);
                time_parts.push((
                    "fractionalSecondDigits".to_string(),
                    format!("{:0width$}", frac, width = fsd as usize),
                ));
            }
        }
        for (ty, val) in time_parts {
            parts.push((ty, val));
        }
        if !suffix.is_empty() {
            parts.push(("literal".to_string(), suffix.trim().to_string()));
        }
    }
    for (_, val) in parts.iter_mut() {
        *val = super::number_format::translate_digits(val, &this.numbering_system);
    }
    Ok(parts)
}

fn js_date_time_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_date_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let mut o = ctx.create_base_object()?;
    let _v3 = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", _v3, JS_PROP_C_W_E)?;
    let _v4 = make_str(ctx, this.calendar.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "calendar", _v4, JS_PROP_C_W_E)?;
    let _v5 = make_str(ctx, this.numbering_system.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "numberingSystem", _v5, JS_PROP_C_W_E)?;
    let _v13 = make_str(ctx, this.time_zone.clone().into_bytes())?;
    o.define_property_value_str(ctx, "timeZone", _v13, JS_PROP_C_W_E)?;
    // hourCycle / hour12 only appear when the hour component is present.
    if this.hour.is_some() {
        let lang = this.locale.id.language.to_string();
        let locale_default12 = || if lang == "ja" { "h11" } else { "h12" };
        // hour12 option takes precedence over hourCycle; otherwise use the
        // explicit hourCycle, else the locale default (h12 for most, h11 for ja).
        let hc = if let Some(h12) = this.hour12 {
            if h12 { locale_default12() } else { "h23" }
        } else if let Some(hc) = this.hour_cycle {
            hc
        } else {
            locale_default12()
        };
        let _vhc = make_str(ctx, hc.as_bytes().to_vec())?;
        o.define_property_value_str(ctx, "hourCycle", _vhc, JS_PROP_C_W_E)?;
        let h12 = match this.hour12 {
            Some(v) => v,
            None => matches!(hc, "h11" | "h12"),
        };
        o.define_property_value_str(ctx, "hour12", JSValue::from(h12), JS_PROP_C_W_E)?;
    }
    set_option_field(ctx, &mut o, "weekday", this.weekday)?;
    set_option_field(ctx, &mut o, "era", this.era)?;
    set_option_field(ctx, &mut o, "year", this.year)?;
    set_option_field(ctx, &mut o, "month", this.month)?;
    set_option_field(ctx, &mut o, "day", this.day)?;
    set_option_field(ctx, &mut o, "hour", this.hour)?;
    set_option_field(ctx, &mut o, "minute", this.minute)?;
    set_option_field(ctx, &mut o, "second", this.second)?;
    if let Some(tzn) = this.time_zone_name {
        let _vtzn = make_str(ctx, tzn.as_bytes().to_vec())?;
        o.define_property_value_str(ctx, "timeZoneName", _vtzn, JS_PROP_C_W_E)?;
    }
    Ok(o.into())
}

/// Set a component property only when it is present (so absent components do
/// not appear in resolvedOptions).
fn set_option_field(
    ctx: &mut ScopedContext,
    o: &mut crate::object::Object,
    name: &str,
    f: Option<DateField>,
) -> Result<(), i32> {
    if let Some(field) = f {
        let v = make_str(
            ctx,
            match field {
                DateField::Narrow => b"narrow".to_vec(),
                DateField::Short => b"short".to_vec(),
                DateField::Long => b"long".to_vec(),
                DateField::TwoDigit => b"2-digit".to_vec(),
                DateField::Numeric => b"numeric".to_vec(),
            },
        )?;
        o.define_property_value_str(ctx, name, v, JS_PROP_C_W_E)?;
    }
    Ok(())
}

const js_date_time_format_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("format", Some(js_date_time_format_format_getter), None, 0),
    FieldDef::FUNC("formatToParts", 1, js_date_time_format_format_to_parts, 0),
    FieldDef::FUNC(
        "formatRangeToParts",
        2,
        js_date_time_format_format_range_to_parts,
        0,
    ),
    FieldDef::FUNC(
        "resolvedOptions",
        0,
        js_date_time_format_resolved_options,
        0,
    ),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.DateTimeFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
