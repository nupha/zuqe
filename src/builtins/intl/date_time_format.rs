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
    pub time_zone: String,
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
        time_zone: "UTC".to_string(),
    };

    let date_format = if options.is_undefined() {
        ctx.create_base_object()?
    } else {
        options.objectify(ctx)?
    };
    let weekday = date_format.get_property_str(ctx, "weekday")?;
    data.weekday = parse_field(ctx, weekday)?;
    let era = date_format.get_property_str(ctx, "era")?;
    data.era = parse_field(ctx, era)?;
    let year = date_format.get_property_str(ctx, "year")?;
    data.year = parse_field(ctx, year)?;
    let month = date_format.get_property_str(ctx, "month")?;
    data.month = parse_field(ctx, month)?;
    let day = date_format.get_property_str(ctx, "day")?;
    data.day = parse_field(ctx, day)?;
    let hour = date_format.get_property_str(ctx, "hour")?;
    data.hour = parse_field(ctx, hour)?;
    let minute = date_format.get_property_str(ctx, "minute")?;
    data.minute = parse_field(ctx, minute)?;
    let second = date_format.get_property_str(ctx, "second")?;
    data.second = parse_field(ctx, second)?;
    if let Some(f) = data.hour {
        let _ = f;
        // hour12 defaults to true when hour is present in many locales.
        let hour12 = date_format.get_property_str(ctx, "hour12")?;
        if !hour12.is_undefined() {
            data.hour12 = Some(hour12.to_bool()?);
        }
    }

    let tz = date_format.get_property_str(ctx, "timeZone")?;
    if !tz.is_undefined() {
        let s = to_utf8_string(ctx, tz)?;
        if !s.is_empty() {
            data.time_zone = s;
        }
    }

    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_DATE_TIME_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
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
        _ => DateField::Numeric,
    }))
}

fn js_date_time_format_format_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    if !is_date_time_format(&this_val) {
        return Err(ctx.throw_type_error("not a DateTimeFormat"));
    }
    make_bound_method(ctx, this_val, js_date_time_format_format_bound, "format", 1)
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
    if ms.is_nan() {
        return Err(ctx.throw_range_error("Invalid time value"));
    }
    // Only UTC supported in this implementation.
    let secs = (ms / 1000.0).floor() as i64;
    let millis = (ms - (secs as f64 * 1000.0)) as i64;

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
    let mut year = 1970 + days.div_euclid(365);
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
    o.set_property_str(ctx, "hour12", JSValue::from(this.hour12.unwrap_or(true)))?;
    let _v14 = option_field(ctx, this.weekday)?;
    o.set_property_str(ctx, "weekday", _v14)?;
    let _v6 = option_field(ctx, this.era)?;
    o.set_property_str(ctx, "era", _v6)?;
    let _v7 = option_field(ctx, this.year)?;
    o.set_property_str(ctx, "year", _v7)?;
    let _v8 = option_field(ctx, this.month)?;
    o.set_property_str(ctx, "month", _v8)?;
    let _v9 = option_field(ctx, this.day)?;
    o.set_property_str(ctx, "day", _v9)?;
    let _v10 = option_field(ctx, this.hour)?;
    o.set_property_str(ctx, "hour", _v10)?;
    let _v11 = option_field(ctx, this.minute)?;
    o.set_property_str(ctx, "minute", _v11)?;
    let _v12 = option_field(ctx, this.second)?;
    o.set_property_str(ctx, "second", _v12)?;
    Ok(o.into())
}

fn option_field(ctx: &mut ScopedContext, f: Option<DateField>) -> Result<JSValue, i32> {
    match f {
        None => Ok(JSValue::UNDEFINED),
        Some(field) => make_str(
            ctx,
            match field {
                DateField::Narrow => b"narrow".to_vec(),
                DateField::Short => b"short".to_vec(),
                DateField::Long => b"long".to_vec(),
                DateField::TwoDigit => b"2-digit".to_vec(),
                DateField::Numeric => b"numeric".to_vec(),
            },
        ),
    }
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
