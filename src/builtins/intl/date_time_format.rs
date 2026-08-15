// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.DateTimeFormat` (§12).

use icu::locale::Locale;

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
use super::number_format::is_valid_bcp47_type;

#[derive(Debug)]
pub(crate) struct DateTimeFormatData {
    pub locale: Locale,
    pub weekday: Option<DateField>,
    pub era: Option<DateField>,
    pub year: Option<DateField>,
    pub month: Option<DateField>,
    pub day: Option<DateField>,
    pub hour: Option<DateField>,
    pub minute: Option<DateField>,
    pub second: Option<DateField>,
    pub hour12: Option<bool>,
    pub hour_cycle: Option<&'static str>,
    pub time_zone: String,
    pub time_zone_name: Option<&'static str>,
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

pub(crate) fn js_date_time_format_constructor(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

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
        year: Some(DateField::Numeric),
        month: Some(DateField::Numeric),
        day: Some(DateField::Numeric),
        hour: None,
        minute: None,
        second: None,
        hour12: None,
        hour_cycle: None,
        time_zone: "UTC".to_string(),
        time_zone_name: None,
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
    let weekday = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "weekday")? };
    let era = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "era")? };
    let year = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "year")? };
    let month = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "month")? };
    let day = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "day")? };
    let hour = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "hour")? };
    let minute = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "minute")? };
    let second = if options_undefined { JSValue::UNDEFINED } else { date_format.get_property_str(ctx, "second")? };
    // Only override the defaults when the corresponding option is present.
    if !weekday.is_undefined() { data.weekday = parse_field(ctx, weekday)?; }
    if !era.is_undefined() { data.era = parse_field(ctx, era)?; }
    if !year.is_undefined() { data.year = parse_field(ctx, year)?; }
    if !month.is_undefined() { data.month = parse_field(ctx, month)?; }
    if !day.is_undefined() { data.day = parse_field(ctx, day)?; }
    if !hour.is_undefined() { data.hour = parse_time_field(ctx, hour)?; }
    if !minute.is_undefined() { data.minute = parse_time_field(ctx, minute)?; }
    if !second.is_undefined() { data.second = parse_time_field(ctx, second)?; }
    let has_explicit_components = !options_undefined && (!weekday.is_undefined()
        || !era.is_undefined()
        || !year.is_undefined()
        || !month.is_undefined()
        || !day.is_undefined()
        || !hour.is_undefined()
        || !minute.is_undefined()
        || !second.is_undefined()
        || !date_format.get_property_str(ctx, "fractionalSecondDigits")?.is_undefined()
        || !date_format.get_property_str(ctx, "dayPeriod")?.is_undefined()
        || !date_format.get_property_str(ctx, "timeZoneName")?.is_undefined());

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
    let date_style = date_format.get_property_str(ctx, "dateStyle")?;
    let time_style = date_format.get_property_str(ctx, "timeStyle")?;
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
    if !tz.is_undefined() {
        let s = to_utf8_string(ctx, tz)?;
        if !is_valid_time_zone(&s) {
            return Err(ctx.throw_range_error("Invalid time zone"));
        }
        data.time_zone = canonicalize_time_zone(&s);
    }

    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_DATE_TIME_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
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
        if !hh_s.bytes().all(|c| c.is_ascii_digit())
            || !mm_s.bytes().all(|c| c.is_ascii_digit())
        {
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
    if (s.starts_with('+') || s.starts_with('-')) && s.chars().all(|c| c.is_ascii_digit() || c == '+' || c == '-') {
        let (sign, body) = (&s[..1], &s[1..]);
        let digits: String = body.chars().filter(|c| *c != ':').collect();
        let (hh, mm) = if digits.len() == 4 {
            (&digits[..2], &digits[2..])
        } else {
            (digits.as_str(), "00")
        };
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
    let n = cc.arg(0);
    let s = format_date(ctx, this, n)?;
    Ok(make_str(ctx, s.into_bytes())?)
}

fn format_date(
    ctx: &mut ScopedContext,
    this: &DateTimeFormatData,
    v: JSValue,
) -> Result<String, i32> {
    let ms = v.to_number(ctx)?.to_f64();
    // ECMA-402 TimeClip: NaN or outside ±8.64e15 ms throws RangeError.
    if ms.is_nan() || ms.is_infinite() || ms < -8.64e15 || ms > 8.64e15 {
        return Err(ctx.throw_range_error("Invalid time value"));
    }
    // Only UTC supported in this implementation. TimeClip applies ToInteger
    // (truncate toward zero, with -0 → +0) to the millisecond value.
    let ms_clip = ms.trunc();
    let ms_clip = if ms_clip == 0.0 { 0.0 } else { ms_clip };
    let secs = (ms_clip / 1000.0).trunc() as i64;
    let millis = (ms_clip - (secs as f64 * 1000.0)) as i64;

    let mut out = String::new();

    if let Some(weekday) = this.weekday {
        let wd = weekday_name(weekday, weekday_index(secs));
        out.push_str(&wd);
    }
    let year = year_from_epoch(secs);
    let month = month_from_epoch(secs);
    let day = day_from_epoch(secs);

    let mut parts: Vec<String> = Vec::new();
    if let Some(era) = this.era {
        let _ = era;
        // era name (AD for positive years)
        parts.push("AD".to_string());
    }
    if let Some(f) = this.year {
        parts.push(format_year(f, year));
    }
    if let Some(f) = this.month {
        parts.push(format_month(f, month));
    }
    if let Some(f) = this.day {
        parts.push(format_day(f, day));
    }
    out.push_str(&parts.join("/"));

    if this.hour.is_some() || this.minute.is_some() || this.second.is_some() {
        let (h, m, s) = time_from_epoch(secs, millis);
        let (h12, suffix) = if this.hour12.unwrap_or(false) {
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
        if let Some(f) = this.minute {
            if !t.is_empty() {
                t.push(':');
            }
            t.push_str(&format_two(f, m));
        }
        if let Some(f) = this.second {
            if !t.is_empty() {
                t.push(':');
            }
            t.push_str(&format_two(f, s));
        }
        t.push_str(suffix);
        if !parts.is_empty() && !t.is_empty() {
            out.push(' ');
        }
        out.push_str(&t);
    }
    Ok(out)
}

fn format_year(f: DateField, y: i64) -> String {
    let _ = f;
    if y < 0 {
        format!("{} BC", -y)
    } else if y == 0 {
        "1 BC".to_string()
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
    // Simplified: returns a single "literal" part with the formatted string.
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_date_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a DateTimeFormat"))?;
    let s = format_date(ctx, this, cc.arg(0))?;
    let mut parts = Vec::new();
    let mut o = ctx.create_base_object()?;
    let _v1 = make_str(ctx, b"literal".to_vec())?;
    o.set_property_str(ctx, "type", _v1)?;
    let _v2 = make_str(ctx, s.into_bytes())?;
    o.set_property_str(ctx, "value", _v2)?;
    parts.push(o.into());
    Ok(values_to_array(ctx, parts)?)
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
    o.set_property_str(ctx, "locale", _v3)?;
    let _v4 = make_str(ctx, b"gregory".to_vec())?;
    o.set_property_str(ctx, "calendar", _v4)?;
    let _v5 = make_str(ctx, b"latn".to_vec())?;
    o.set_property_str(ctx, "numberingSystem", _v5)?;
    let _v13 = make_str(ctx, this.time_zone.clone().into_bytes())?;
    o.set_property_str(ctx, "timeZone", _v13)?;
    // hourCycle / hour12 only appear when the hour component is present.
    if this.hour.is_some() {
        // hour12 option takes precedence over hourCycle; otherwise use the
        // explicit hourCycle, else the locale default (h12 for most, h11 for ja).
        let hc = if let Some(h12) = this.hour12 {
            if h12 { "h12" } else { "h23" }
        } else if let Some(hc) = this.hour_cycle {
            hc
        } else {
            let lang = this.locale.id.language.to_string();
            if lang == "ja" { "h11" } else { "h12" }
        };
        let _vhc = make_str(ctx, hc.as_bytes().to_vec())?;
        o.set_property_str(ctx, "hourCycle", _vhc)?;
        let h12 = match this.hour12 {
            Some(v) => v,
            None => matches!(hc, "h11" | "h12"),
        };
        o.set_property_str(ctx, "hour12", JSValue::from(h12))?;
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
        o.set_property_str(ctx, "timeZoneName", _vtzn)?;
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
        o.set_property_str(ctx, name, v)?;
    }
    Ok(())
}

const js_date_time_format_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("format", Some(js_date_time_format_format_getter), None, 0),
    FieldDef::FUNC("formatToParts", 1, js_date_time_format_format_to_parts, 0),
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
