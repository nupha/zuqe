use std::{
    ops::RangeInclusive,
    os::unix::raw::time_t,
    time::{SystemTime, UNIX_EPOCH},
};

use gc_lite::Collect;

use crate::{
    atom::Atom,
    class::ClassID,
    context::{ContextImpl, ContextRef},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{ObjectPayload, ObjectRef},
    string::{StringBuf, StringChar, StringRef},
    value::{HINT_FORCE_ORDINARY, HINT_NONE, HINT_NUMBER, HINT_STRING, JSValue},
};

const MONTH_DAYS: [u8; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
const MONTH_NAMES: &str = "JanFebMarAprMayJunJulAugSepOctNovDec";
const WEEKDAY_NAMES: &str = "SunMonTueWedThuFriSat";

const INVALID_TIMESTAMP: i64 = i64::MIN;
const VALID_TIMESTAMP_RANGE: RangeInclusive<i64> = -8640000000000000..=8640000000000000;

const fn floor_div(a: i64, b: i64) -> i64 {
    let m = a % b;
    (a - (m + if m < 0 { b } else { 0 })) / b
}

const fn math_mod(a: i64, b: i64) -> i64 {
    let m = a % b;
    m + if m < 0 { b } else { 0 }
}

/// returns NaN if the timestamp is invalid
fn time_clip(t: f64) -> f64 {
    if (-8.64e15..=8.64e15).contains(&t) {
        t.trunc() + 0.0 // convert -0 to +0
    } else {
        f64::NAN
    }
}

#[inline(always)]
const fn days_from_year(y: i64) -> i64 {
    365 * (y - 1970) + floor_div(y - 1969, 4) - floor_div(y - 1901, 100) + floor_div(y - 1601, 400)
}

#[inline(always)]
const fn days_in_year(y: i64) -> i64 {
    365 + (y % 4 == 0) as i64 - (y % 100 == 0) as i64 + (y % 400 == 0) as i64
}

/// return the year, update days
const fn year_from_days(days: &mut i64) -> i64 {
    let d: i64 = *days;
    let mut y = floor_div(d * 10000, 3652425) + 1970;
    let mut d1: i64;

    // the initial approximation is very good, so only a few
    // iterations are necessary
    loop {
        d1 = d - days_from_year(y);

        if d1 < 0 {
            y -= 1;
        } else {
            let nd = days_in_year(y);
            if d1 < nd {
                break;
            }
            y += 1;
        }
    }

    *days = d1;
    y
}

/// make unix timestamp from fields of [year, mon, day, hour, min, sec, ms]
fn make_timestamp(fields: &[f64; 7], is_local: bool) -> Option<i64> {
    let mut m = (fields[1] % 12.0) as i32;
    if m < 0 {
        m += 12;
    }
    let y = fields[0] as i64 + (fields[1] / 12.0).floor() as i64;
    let mut days = days_from_year(y) as f64;

    for i in 0..m {
        let mut md = MONTH_DAYS[i as usize] as i64;
        if i == 1 {
            // Feb: test leap year
            md += days_in_year(y) - 365;
        }
        days += md as f64;
    }
    days += fields[2] - 1.0;

    let h = fields[3] * 3600000.0 + fields[4] * 60000.0 + fields[5] * 1000.0 + fields[6];

    let mut ts = days * 86400000.0 + h;
    if is_local {
        ts += get_timezone_offset(ts as i64) as f64 * 60000.0;
    }

    let r = time_clip(ts);
    if r.is_finite() { Some(r as i64) } else { None }
}

fn timestamp_to_fields(tm_val: i64, is_local: bool) -> Option<[i64; 9]> {
    if !VALID_TIMESTAMP_RANGE.contains(&tm_val) {
        return None; // invalid
    }

    let mut d: i64 = tm_val;
    let mut tz: i64 = 0;

    if is_local {
        tz = -get_timezone_offset(d) as i64;
        d += tz * 60000;
    }

    let h = math_mod(d, 86400000);
    let mut days = (d - h) / 86400000;
    let ms = h % 1000;
    let h = (h - ms) / 1000;
    let s = h % 60;
    let h = (h - s) / 60;
    let m = h % 60;
    let h = (h - m) / 60;
    let wd = math_mod(days + 4, 7); /* week day */
    let y = year_from_days(&mut days);

    let mut mon = 0;
    for i in 0..12 {
        mon = i;
        let mut md = MONTH_DAYS[i as usize] as i64;
        if i == 1 {
            md += days_in_year(y) - 365;
        }
        if days < md {
            break;
        }
        days -= md;
    }

    Some([y, mon as i64, days + 1, h, m, s, ms, wd, tz])
}

// OS dependent. d = argv[0] is in ms from 1970. Return the difference
// between UTC time and local time 'd' in minutes
fn get_timezone_offset(time: i64) -> i32 {
    let time = time / 1000; // convert to seconds

    #[cfg(target_pointer_width = "32")]
    {
        // on 32-bit systems, we need to clamp the time value to the
        // range of `time_t`. This is better than truncating values to
        // 32 bits and hopefully provides the same result as 64-bit
        // implementation of localtime_r.
        if (-(1 as time_t)) < 0 {
            if time < i32::MIN as i64 {
                time = i32::MIN as i64;
            } else if time > i32::MAX as i64 {
                time = i32::MAX as i64;
            }
        } else {
            if time < 0 {
                time = 0;
            } else if time > u32::MAX as i64 {
                time = u32::MAX as i64;
            }
        }
    }

    let ti: time_t = time as time_t;

    #[cfg(target_os = "windows")]
    let res = unsafe {
        let tm = gmtime(&ti);
        let gm_ti = mktime(tm);

        let tm = localtime(&ti);
        let loc_ti = mktime(tm);

        ((gm_ti - loc_ti) / 60) as i32
    };
    #[cfg(not(target_os = "windows"))]
    let res = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&ti, &mut tm);
        (-(tm.tm_gmtoff) / 60) as i32
    };

    res
}

fn string_get_digits(s: &[u8]) -> (i64, usize) {
    let mut v: i64 = 0;
    for (i, c) in s.iter().enumerate() {
        if c.is_ascii_digit() {
            v = v * 10 + (*c - b'0') as i64;
        } else {
            return (v, i);
        }
    }
    (v, s.len())
}

fn string_get_signed_digits(buf: &[u8]) -> Option<(i64, usize)> {
    let mut minus = false;
    let start = match buf[0] {
        b'+' => 1,
        b'-' => {
            minus = true;
            1
        }
        _ => 0,
    };

    if start < buf.len() {
        let (num, adv) = string_get_digits(&buf[start..]);
        if adv > 0 {
            if minus && num == 0 {
                None // reject '-0'
            } else {
                Some((if minus { -num } else { num }, adv + start))
            }
        } else {
            None
        }
    } else {
        None
    }
}

// parse a fixed width numeric field
fn string_get_fixed_width_digits(sp: &[u8], n: usize) -> Option<(i64, usize)> {
    debug_assert!(n <= sp.len());
    let mut v: i64 = 0;
    let mut cnt = n;
    for c in sp.iter().take(n) {
        if c.is_ascii_digit() {
            v = v * 10 + (*c - b'0') as i64;
            cnt -= 1;
        }
    }

    if cnt == 0 { Some((v, n)) } else { None }
}

/// parse milliseconds as a fractional part, round to nearest
fn string_get_milliseconds(buf: &[u8]) -> Option<(i64, usize)> {
    let mut mul = 1000_i32;
    let mut ms = 0_i32;
    let mut adv = 0_usize;

    for (i, c) in buf.iter().enumerate() {
        if c.is_ascii_digit() {
            if mul == 1 && *c >= b'5' {
                ms += 1;
            }
            mul /= 10;
            ms += (*c - b'0') as i32 * mul;
        } else if i == 0 {
            return None; // invalid
        } else {
            adv = i;
            break;
        }
    }

    Some((ms as i64, if adv == 0 { buf.len() } else { adv }))
}

fn find_substring(needle: &[u8], haystack: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn string_get_month(sp: &[u8]) -> Option<(i64, usize)> {
    let p = skip_spaces(sp);
    if p < sp.len() - 3 {
        find_substring(&sp[p..p + 3], MONTH_NAMES.as_bytes()).map(|n| ((n / 3) as i64, p + 3))
    } else {
        None
    }
}

fn skip_spaces(buf: &[u8]) -> usize {
    let mut p = 0;
    while let Some(c) = buf.get(p)
        && *c == b' '
    {
        p += 1;
    }
    p
}

fn skip_non_spaces(buf: &[u8]) -> usize {
    let mut p = 0;
    while let Some(c) = buf.get(p)
        && *c != b' '
    {
        p += 1;
    }
    p
}

fn datetime_parse(buf: &[u8]) -> Option<i64> {
    let mut fields: [f64; 7] = [0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    let mut is_local = false;
    let mut tz: i64;

    match buf[0] {
        b'0'..=b'9' | b'+' | b'-' => {
            // ISO format
            // year field can be negative
            let (n, mut idx) = string_get_signed_digits(buf)?;
            fields[0] = n as f64;

            for i in 1..7 {
                if idx >= buf.len() {
                    break;
                }

                let expect = match i {
                    1 | 2 => b'-',
                    3 => b'T',
                    4 | 5 => b':',
                    6 => b'.',
                    _ => {
                        unreachable!()
                    }
                };
                if buf[idx] != expect {
                    break;
                }

                idx += 1;
                if i > 3 {
                    is_local = true;
                }

                if i == 6 {
                    let (n, adv) = string_get_milliseconds(&buf[idx..])?;
                    fields[6] = n as f64;
                    idx += adv;
                } else {
                    let (n, adv) = string_get_digits(&buf[idx..]);
                    if adv == 0 {
                        return None;
                    };
                    fields[i as usize] = n as f64;
                    idx += adv;
                }
            }

            fields[1] -= 1.0; // month is 0-based

            // parse the time zone offset if present: [+-]HH:mm or [+-]HHmm
            tz = 0;
            if idx < buf.len() {
                match buf[idx] {
                    sgn @ (b'+' | b'-') => {
                        idx += 1;

                        let l = buf.len() - idx;
                        if !matches!(l, 4 | 5) {
                            return None;
                        }

                        let (hh, adv) = string_get_fixed_width_digits(&buf[idx..], 2)?;
                        idx += adv;

                        if l == 5 {
                            if buf[idx] != b':' {
                                return None;
                            }
                            idx += 1;
                        }

                        let (mm, adv) = string_get_fixed_width_digits(buf, 2)?;
                        idx += adv;

                        tz = hh * 60 + mm;
                        if sgn == b'-' {
                            tz = -tz;
                        }
                        is_local = false;
                    }
                    b'Z' => {
                        idx += 1;
                        is_local = false;
                    }
                    _ => {
                        return None;
                    }
                }

                if idx != buf.len() {
                    // error if extraneous characters
                    return None;
                }
            }
        }
        _ => {
            // toString or toUTCString format
            // skip the day of the week
            let mut p = skip_non_spaces(buf);
            p += skip_spaces(&buf[p..]);

            if p >= buf.len() {
                return None;
            }

            if buf[p].is_ascii_digit() {
                // day of month first
                let (n, adv) = string_get_digits(&buf[p..]);
                if adv == 0 {
                    return None;
                };
                fields[2] = n as f64;
                p += adv;

                let (n, adv) = string_get_month(&buf[p..])?;
                fields[1] = n as f64;
                p += adv;
            } else {
                // month first
                let (n, adv) = string_get_month(&buf[p..])?;
                fields[1] = n as f64;
                p += adv;

                p += skip_spaces(&buf[p..]);

                if p < buf.len() {
                    let (n, adv) = string_get_digits(&buf[p..]);
                    if adv == 0 {
                        return None;
                    }
                    fields[2] = n as f64;
                    p += adv;
                }
            }

            // year
            if p < buf.len() {
                p += skip_spaces(&buf[p..]);

                if let Some((n, adv)) = string_get_signed_digits(&buf[p..]) {
                    fields[0] = n as f64;
                    p += adv;
                } else {
                    return None;
                }

                // hour, min, seconds
                p += skip_spaces(&buf[p..]);
                for i in 0..3 {
                    if p < buf.len() {
                        if matches!(i, 1 | 2) {
                            if buf[p] != b':' {
                                return None;
                            } else {
                                p += 1;
                            }
                        }

                        let (n, adv) = string_get_digits(&buf[p..]);
                        if adv > 0 {
                            fields[3 + i] = n as f64;
                            p += adv;
                        } else {
                            return None;
                        }
                    }
                }
            }

            // XXX: parse optional milliseconds?

            // parse the time zone offset if present: [+-]HHmm
            tz = 0;
            while p < buf.len() {
                if let sgn @ (b'+' | b'-') = buf[p] {
                    p += 1;
                    let (hh, adv) = string_get_fixed_width_digits(&buf[p..], 2)?;
                    p += adv;

                    let (mm, adv) = string_get_fixed_width_digits(&buf[p..], 2)?;
                    p += adv;

                    tz = hh * 60 + mm;
                    if sgn == b'-' {
                        tz = -tz;
                    }
                }
                p += 1;
            }
        }
    }

    make_timestamp(&fields, is_local).map(|ts| ts - tz * 60000)
}

/// fmt:
/// - 0: toUTCString: "Tue, 02 Jan 2018 23:04:46 GMT"
/// - 1: toString: "Wed Jan 03 2018 00:05:22 GMT+0100 (CET)"
/// - 2: toISOString: "2018-01-02T23:02:56.927Z"
/// - 3: toLocaleString: "1/2/2018, 11:40:40 PM"
/// - part: 1=date, 2=time 3=all
fn datetime_format(ctx: &ContextImpl, tm_val: i64, flags: i32) -> Result<StringRef, i32> {
    let fmt = (flags >> 4) & 0x0F;
    let part = flags & 0x0F;

    let Some(fields) = timestamp_to_fields(tm_val, fmt & 1 != 0) else {
        return if fmt == 2 {
            ctx.throw_range_error("Date value is NaN");
            Err(-1)
        } else {
            ctx.create_string_from_str("Invalid Date")
        };
    };

    let y = fields[0] as i32;
    let mon = fields[1] as i32;
    let d = fields[2] as i32;
    let h = fields[3] as i32;
    let m = fields[4] as i32;
    let s = fields[5] as i32;
    let ms = fields[6] as i32;
    let wd = fields[7] as i32;
    let tz = fields[8] as i32;

    let mut buf = StringBuf::with_capacity(64, false).map_err(|_| {
        ctx.throw_out_of_memory();
        -1
    })?;

    if part & 1 != 0 {
        // date part
        match fmt {
            0 => {
                let s = format!(
                    "{:3}, {d:02} {:3} {y:0width$}",
                    &WEEKDAY_NAMES[wd as usize * 3..(wd as usize * 3 + 3)],
                    &MONTH_NAMES[mon as usize * 3..(mon as usize * 3 + 3)],
                    width = if y < 0 { 5 } else { 4 }
                );
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                if part == 3 {
                    // space beteween date and time
                    buf.push_char(' ').map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
            }
            1 => {
                let s = format!(
                    "{:3} {:3} {d:02} {y:0width$}",
                    &WEEKDAY_NAMES[wd as usize * 3..(wd as usize * 3 + 3)],
                    &MONTH_NAMES[mon as usize * 3..(mon as usize * 3 + 3)],
                    width = if y < 0 { 5 } else { 4 }
                );
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                if part == 3 {
                    // space beteween date and time
                    buf.push_char(' ').map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
            }
            2 => {
                let s = if (0..10000).contains(&y) {
                    format!("{y:04}-{:02}-{d:02}T", mon + 1)
                } else {
                    format!("{y:+07}-{:02}-{d:02}T", mon + 1)
                };
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            3 => {
                let s = format!(
                    "{:02}/{d:02}/{y:0width$}",
                    mon + 1,
                    width = if y < 0 { 5 } else { 4 }
                );
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
                if part == 3 {
                    buf.push_str(", ").map_err(|_| {
                        ctx.throw_out_of_memory();
                        -1
                    })?;
                }
            }
            _ => {}
        }
    }

    if part & 2 != 0 {
        // time part
        match fmt {
            0 => {
                let s = format!("{h:02}:{m:02}:{s:02} GMT");
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            1 => {
                let s = format!("{h:02}:{m:02}:{s:02} GMT");
                buf.push_chars_iter(
                    s.chars()
                        .chain(std::iter::once(if tz < 0 { '-' } else { '+' }))
                        .chain(format!("{:02}{:02}", tz.abs() / 60, tz.abs() % 60).chars())
                        .map(StringChar::Char),
                )
                .map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            2 => {
                let s = format!("{h:02}:{m:02}:{s:02}.{ms:03}Z");
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            3 => {
                let s = format!(
                    "{:02}:{m:02}{s:02} {}M",
                    (h + 11) % 12 + 1,
                    if h < 12 { 'A' } else { 'P' }
                );
                buf.push_str(s.as_str()).map_err(|_| {
                    ctx.throw_out_of_memory();
                    -1
                })?;
            }
            _ => {}
        }
    }

    ctx.create_string_with_buf(buf)
}

#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Datetime {
    /// millis since UNIX_EPOCH
    timestamp_ms: i64,
}

unsafe impl Collect for Datetime {
    #[inline(always)]
    fn trace(&self, _: &mut gc_lite::Tracer) {}
}

impl Datetime {
    pub const INVALID: Self = Self {
        timestamp_ms: INVALID_TIMESTAMP,
    };

    pub fn class_initialize(mut ctx: ContextRef, _: ClassID) -> Result<Option<ObjectRef>, i32> {
        let mut obj = ctx.create_base_object()?;
        obj.install_field_defs(&ctx, &js_date_proto_funcs).unwrap();

        let mut ctor =
            ctx.create_global_constructor_or_func("Date", js_date_constructor, 7, obj.into(), 0)?;
        ctor.install_field_defs(&ctx, &js_date_funcs)?;

        //self.0.borrow_mut().class_proto[JS_CLASS_DATE as usize] = obj;
        Ok(Some(obj))
    }

    #[inline(always)]
    pub fn from_timestamp_ms(ts: i64) -> Self {
        Self {
            timestamp_ms: if VALID_TIMESTAMP_RANGE.contains(&ts) {
                ts
            } else {
                INVALID_TIMESTAMP
            },
        }
    }

    pub fn from_system_time(system_time: SystemTime) -> Self {
        let duration = system_time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_else(|e| e.duration());

        Self {
            timestamp_ms: duration.as_millis() as i64,
        }
    }

    #[inline(always)]
    pub fn now() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    pub fn from_fields(fields: &[f64; 7], is_local: bool) -> Self {
        if let Some(ts) = make_timestamp(fields, is_local) {
            Self { timestamp_ms: ts }
        } else {
            Self::INVALID
        }
    }

    pub fn parse(str: &[u8]) -> Option<Self> {
        datetime_parse(str).map(Self::from_timestamp_ms)
    }

    #[inline(always)]
    pub const fn timestamp(&self) -> i64 {
        self.timestamp_ms
    }

    pub fn system_time(&self) -> SystemTime {
        if self.timestamp_ms >= 0 {
            UNIX_EPOCH + std::time::Duration::from_millis(self.timestamp_ms as u64)
        } else {
            UNIX_EPOCH - std::time::Duration::from_millis((-self.timestamp_ms) as u64)
        }
    }

    #[inline(always)]
    pub fn is_invalid(&self) -> bool {
        !VALID_TIMESTAMP_RANGE.contains(&self.timestamp_ms)
    }
}

impl From<i64> for Datetime {
    #[inline(always)]
    fn from(val: i64) -> Self {
        Self::from_timestamp_ms(val)
    }
}

impl From<f64> for Datetime {
    fn from(val: f64) -> Self {
        let ts = time_clip(val);
        if ts.is_finite() {
            Self::from_timestamp_ms(ts as i64)
        } else {
            Self::INVALID
        }
    }
}

impl From<SystemTime> for Datetime {
    #[inline(always)]
    fn from(system_time: SystemTime) -> Self {
        Self::from_system_time(system_time)
    }
}

impl From<Datetime> for SystemTime {
    #[inline(always)]
    fn from(ts: Datetime) -> Self {
        ts.system_time()
    }
}

impl From<Datetime> for i64 {
    #[inline(always)]
    fn from(ts: Datetime) -> Self {
        ts.timestamp()
    }
}

impl From<Datetime> for JSValue {
    fn from(tm: Datetime) -> Self {
        if tm.is_invalid() {
            Self::from(f64::NAN)
        } else {
            Self::from(tm.timestamp())
        }
    }
}

pub fn js_date_constructor(ctx: ContextRef, cc: CallContext) -> JSResult {
    let new_target = *cc.this();

    let nargs = if new_target.is_undefined() {
        0 // invoked as function
    } else {
        cc.length()
    };

    let tm: Datetime = match nargs {
        0 => Datetime::now(),
        1 => {
            if let Some(ts) = cc.arg(0).as_object().and_then(|p| {
                if let ObjectPayload::DATE(dt) = p.payload() {
                    Some(dt)
                } else {
                    None
                }
            }) {
                *ts
            } else {
                match cc.arg(0).to_primitive(&ctx, HINT_NONE)? {
                    JSValue::String(s) => Datetime::parse(&s.content().encode_utf8_bytes())
                        .unwrap_or(Datetime::INVALID),
                    v => v.to_f64(&ctx)?.into(),
                }
            }
        }
        n => {
            let mut fields: [f64; 7] = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            let mut has_invalid = false;

            for i in 0..std::cmp::min(n, 7) {
                let d = cc.arg(i).to_f64(&ctx)?;
                if d.is_finite() {
                    fields[i] = d.trunc();
                    if i == 0 && (0.0..100.0).contains(&fields[0]) {
                        fields[0] += 100.0;
                    }
                } else {
                    has_invalid = true;
                    break;
                }
            }
            if has_invalid {
                Datetime::INVALID
            } else {
                Datetime::from_fields(&fields, true)
            }
        }
    };

    if new_target.is_undefined() {
        // invoked as a function, returns toString()
        datetime_format(&ctx, tm.into(), 0x13).map(JSValue::String)
    } else {
        ctx.create_object_from_ctor(new_target, ObjectPayload::DATE(tm))
            .map(JSValue::Object)
            .map_err(|_| -1)
    }
}

fn JS_ThisTimeValue(ctx: ContextRef, this: &JSValue) -> Result<Datetime, i32> {
    if let Some(t) = this.as_object().and_then(|o| {
        if let ObjectPayload::DATE(dt) = o.payload() {
            Some(*dt)
        } else {
            None
        }
    }) {
        Ok(t)
    } else {
        ctx.throw_type_error("not a Date object");
        Err(-1)
    }
}

fn js_date_getTime(ctx: ContextRef, cc: CallContext) -> JSResult {
    JS_ThisTimeValue(ctx, cc.this()).map(JSValue::from)
}

fn js_date_setTime(ctx: ContextRef, mut cc: CallContext) -> JSResult {
    let _ = JS_ThisTimeValue(ctx, cc.this())?;

    let a = cc.arg(0).to_f64(&ctx)?;
    let dt2 = if a.is_finite() {
        Datetime::from_timestamp_ms(a.trunc() as i64) // will clip inside
    } else {
        Datetime::INVALID
    };

    if let ObjectPayload::DATE(dt) = cc.this_mut().as_object_mut().unwrap().payload_mut() {
        *dt = dt2;
        Ok(dt2.into())
    } else {
        ctx.throw_type_error("not a Date object");
        Err(-1)
    }
}

fn get_date_string(ctx: ContextRef, cc: CallContext) -> JSResult {
    let tm = JS_ThisTimeValue(ctx, cc.this())?;
    datetime_format(&ctx, tm.into(), cc.magic()).map(JSValue::String)
}

fn js_date_Symbol_toPrimitive(ctx: ContextRef, cc: CallContext) -> JSResult {
    // Symbol_toPrimitive(hint)
    let Some(obj) = cc.this().as_object() else {
        ctx.throw_type_error_not_an_object();
        return Err(-1);
    };

    let hint: Atom = if let JSValue::String(s) = cc.arg(0) {
        s.to_atom(ctx.rt())?
    } else {
        Atom::CONST_empty_string
    };

    let hint_num = match hint {
        Atom::CONST_number => HINT_NUMBER,
        Atom::CONST_string | Atom::CONST_default => HINT_STRING,
        _ => {
            ctx.throw_type_error("invalid hint ");
            return Err(-1);
        }
    };
    obj.to_primitive(&ctx, hint_num | HINT_FORCE_ORDINARY)
}

fn js_date_getTimezoneOffset(ctx: ContextRef, cc: CallContext) -> JSResult {
    let tm = JS_ThisTimeValue(ctx, cc.this())?;
    if tm.is_invalid() {
        Ok(JSValue::Float64(f64::NAN))
    } else {
        Ok(JSValue::Int(get_timezone_offset(tm.into())))
    }
}

fn get_date_field(ctx: ContextRef, cc: CallContext) -> JSResult {
    let tm = JS_ThisTimeValue(ctx, cc.this())?;

    let magic = cc.magic();
    let is_local = magic & 0x0F != 0;
    let n = (magic >> 4) & 0x0F;

    if let Some(mut fields) = timestamp_to_fields(tm.into(), is_local) {
        if magic & 0x100 != 0 {
            // getYear
            fields[0] -= 1900;
        }
        Ok(JSValue::from(fields[n as usize]))
    } else {
        Ok(JSValue::Float64(f64::NAN))
    }
}

fn set_date_field_internal(
    ctx: ContextRef,
    mut this: JSValue,
    args: &[JSValue],
    magic: i32,
) -> JSResult {
    let first_field = (magic >> 8) & 0x0F;
    let end_field = (magic >> 4) & 0x0F;
    let is_local = magic & 0x0F != 0;

    let (mut fields, this_is_bad) =
        if let Some(ff) = timestamp_to_fields(JS_ThisTimeValue(ctx, &this)?.into(), is_local) {
            let mut ff2: [f64; 7] = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            for (i, n) in ff.into_iter().take(7).enumerate() {
                ff2[i] = n as f64;
            }
            (Some(ff2), false)
        } else if first_field == 0 {
            // set year will re-vive an invalid date
            (Some([0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0]), true)
        } else {
            (None, true)
        };

    let field_range = first_field as usize..end_field as usize;
    for (v, i) in args.iter().zip(field_range.into_iter()) {
        let a = v.to_f64(&ctx)?;
        if a.is_finite() {
            if let Some(f) = &mut fields {
                f[i] = a.trunc();
            }
        } else {
            fields.take();
        }
    }

    let dt2: Datetime = if let Some(fields) = fields
        && !args.is_empty()
    {
        make_timestamp(&fields, is_local).map_or(Datetime::INVALID, Datetime::from_timestamp_ms)
    } else {
        Datetime::INVALID
    };

    if let ObjectPayload::DATE(dt) = this.as_object_mut().unwrap().payload_mut() {
        // Note: to pass test262
        // only `this` timestamp is invalid, and not setting `year`, do update the timestamp.
        // `this` timestamp could have been updated in valueOf() method.
        if first_field == 0 || !this_is_bad {
            *dt = dt2;
        }

        Ok(dt2.into())
    } else {
        ctx.throw_type_error("not a Date object");
        Err(-1)
    }
}

fn set_date_field(ctx: ContextRef, cc: CallContext) -> JSResult {
    set_date_field_internal(ctx, *cc.this(), cc.arguments(), cc.magic())
}

fn js_date_setYear(ctx: ContextRef, cc: CallContext) -> JSResult {
    let mut y = cc.arg(0).to_f64(&ctx)?;
    if y.is_finite() {
        y = y.trunc();
        if (0.0..100.0).contains(&y) {
            y += 1900.0;
        }
    }
    set_date_field_internal(ctx, *cc.this(), &[JSValue::from(y)], 0x011)
}

fn js_date_toJSON(ctx: ContextRef, cc: CallContext) -> JSResult {
    let obj = cc.this().objectify(&ctx)?;

    let tv = obj.to_primitive(&ctx, HINT_NUMBER)?;
    if tv.is_number() {
        let d = tv.to_f64(&ctx)?;
        if !d.is_finite() {
            return Ok(JSValue::NULL);
        }
    }

    let method = obj.get_property_str(&ctx, "toISOString")?;
    if method.is_function() {
        method.call(&ctx, obj.into(), &[])
    } else {
        ctx.throw_type_error("object needs toISOString method");
        Err(-1)
    }
}

pub static js_date_proto_funcs: [FieldDef; 47] = [
    FieldDef::FUNCTION("valueOf", 0, js_date_getTime, 0),
    FieldDef::FUNCTION("toString", 0, get_date_string, 0x13),
    FieldDef::FUNCTION("[Symbol.toPrimitive]", 1, js_date_Symbol_toPrimitive, 0),
    FieldDef::FUNCTION("toUTCString", 0, get_date_string, 0x03),
    FieldDef::ALIAS("toGMTString", "toUTCString"),
    FieldDef::FUNCTION("toISOString", 0, get_date_string, 0x23),
    FieldDef::FUNCTION("toDateString", 0, get_date_string, 0x11),
    FieldDef::FUNCTION("toTimeString", 0, get_date_string, 0x12),
    FieldDef::FUNCTION("toLocaleString", 0, get_date_string, 0x33),
    FieldDef::FUNCTION("toLocaleDateString", 0, get_date_string, 0x31),
    FieldDef::FUNCTION("toLocaleTimeString", 0, get_date_string, 0x32),
    FieldDef::FUNCTION("getTimezoneOffset", 0, js_date_getTimezoneOffset, 0),
    FieldDef::FUNCTION("getTime", 0, js_date_getTime, 0),
    FieldDef::FUNCTION("getYear", 0, get_date_field, 0x101),
    FieldDef::FUNCTION("getFullYear", 0, get_date_field, 0x01),
    FieldDef::FUNCTION("getUTCFullYear", 0, get_date_field, 0x00),
    FieldDef::FUNCTION("getMonth", 0, get_date_field, 0x11),
    FieldDef::FUNCTION("getUTCMonth", 0, get_date_field, 0x10),
    FieldDef::FUNCTION("getDate", 0, get_date_field, 0x21),
    FieldDef::FUNCTION("getUTCDate", 0, get_date_field, 0x20),
    FieldDef::FUNCTION("getHours", 0, get_date_field, 0x31),
    FieldDef::FUNCTION("getUTCHours", 0, get_date_field, 0x30),
    FieldDef::FUNCTION("getMinutes", 0, get_date_field, 0x41),
    FieldDef::FUNCTION("getUTCMinutes", 0, get_date_field, 0x40),
    FieldDef::FUNCTION("getSeconds", 0, get_date_field, 0x51),
    FieldDef::FUNCTION("getUTCSeconds", 0, get_date_field, 0x50),
    FieldDef::FUNCTION("getMilliseconds", 0, get_date_field, 0x61),
    FieldDef::FUNCTION("getUTCMilliseconds", 0, get_date_field, 0x60),
    FieldDef::FUNCTION("getDay", 0, get_date_field, 0x71),
    FieldDef::FUNCTION("getUTCDay", 0, get_date_field, 0x70),
    FieldDef::FUNCTION("setTime", 1, js_date_setTime, 0),
    FieldDef::FUNCTION("setMilliseconds", 1, set_date_field, 0x671),
    FieldDef::FUNCTION("setUTCMilliseconds", 1, set_date_field, 0x670),
    FieldDef::FUNCTION("setSeconds", 2, set_date_field, 0x571),
    FieldDef::FUNCTION("setUTCSeconds", 2, set_date_field, 0x570),
    FieldDef::FUNCTION("setMinutes", 3, set_date_field, 0x471),
    FieldDef::FUNCTION("setUTCMinutes", 3, set_date_field, 0x470),
    FieldDef::FUNCTION("setHours", 4, set_date_field, 0x371),
    FieldDef::FUNCTION("setUTCHours", 4, set_date_field, 0x370),
    FieldDef::FUNCTION("setDate", 1, set_date_field, 0x231),
    FieldDef::FUNCTION("setUTCDate", 1, set_date_field, 0x230),
    FieldDef::FUNCTION("setMonth", 2, set_date_field, 0x131),
    FieldDef::FUNCTION("setUTCMonth", 2, set_date_field, 0x130),
    FieldDef::FUNCTION("setYear", 1, js_date_setYear, 0),
    FieldDef::FUNCTION("setFullYear", 3, set_date_field, 0x031),
    FieldDef::FUNCTION("setUTCFullYear", 3, set_date_field, 0x030),
    FieldDef::FUNCTION("toJSON", 1, js_date_toJSON, 0),
];

fn js_Date_now(_: ContextRef, _: CallContext) -> JSResult {
    let ts = Datetime::now();
    Ok(JSValue::from(ts.timestamp()))
}

fn js_Date_parse(ctx: ContextRef, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(&ctx)?;
    let tm = Datetime::parse(&s.content().encode_utf8_bytes()).unwrap_or(Datetime::INVALID);
    Ok(JSValue::from(tm))
}

fn js_Date_UTC(ctx: ContextRef, cc: CallContext) -> JSResult {
    let dt = if cc.length() == 0 {
        Datetime::INVALID
    } else {
        let mut fields: [f64; 7] = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        for (i, a) in cc.arguments().iter().take(7).enumerate() {
            let v = a.to_f64(&ctx)?;
            if !v.is_finite() {
                return Ok(JSValue::Float64(f64::NAN));
            }
            fields[i] = v.trunc();
            if i == 0 && (0.0..100.0).contains(&fields[0]) {
                fields[0] += 1900.0;
            }
        }
        Datetime::from_fields(&fields, false)
    };
    Ok(dt.into())
}

pub static js_date_funcs: [FieldDef; 3] = [
    FieldDef::FUNCTION("now", 0, js_Date_now, 0),
    FieldDef::FUNCTION("parse", 1, js_Date_parse, 0),
    FieldDef::FUNCTION("UTC", 7, js_Date_UTC, 0),
];
