// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.RelativeTimeFormat` (§15).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{
    locale_to_string, make_bound_method, make_str, resolve_locale, to_utf8_string, values_to_array,
};

#[derive(Debug)]
pub(crate) struct RelativeTimeFormatData {
    pub locale: Locale,
    pub style: RelStyle,
    pub numeric: RelNumeric,
    pub numbering_system: String,
}

impl GcTrace for RelativeTimeFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Debug)]
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

pub(crate) static CLASS_DEF_RELATIVE_TIME_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_RelativeTimeFormat,
    js_relative_time_format_proto_funcs,
    Some(ClassCtorDef::new(
        js_relative_time_format_constructor,
        0,
        true,
    )),
);

pub(crate) fn js_relative_time_format_constructor(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let mut data = RelativeTimeFormatData {
        locale,
        style: RelStyle::Long,
        numeric: RelNumeric::Always,
        numbering_system: "latn".to_string(),
    };

    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let style = obj.get_property_str(ctx, "style")?;
        if !style.is_undefined() {
            let s = to_utf8_string(ctx, style)?;
            data.style = match s.as_str() {
                "short" => RelStyle::Short,
                "narrow" => RelStyle::Narrow,
                _ => RelStyle::Long,
            };
        }
        let numeric = obj.get_property_str(ctx, "numeric")?;
        if !numeric.is_undefined() {
            let s = to_utf8_string(ctx, numeric)?;
            data.numeric = if s == "auto" {
                RelNumeric::Auto
            } else {
                RelNumeric::Always
            };
        }
    }

    // Effective numbering system: explicit valid option > `nu` extension > latn.
    let ns_opt: Option<String> = if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let ns = obj.get_property_str(ctx, "numberingSystem")?;
        if ns.is_undefined() {
            None
        } else {
            Some(to_utf8_string(ctx, ns)?)
        }
    } else {
        None
    };
    let ns_static: Option<&'static str> = ns_opt
        .as_deref()
        .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
    let nu_ext = super::common::locale_extension_value(&data.locale, "nu");
    let nu_ext_valid = nu_ext
        .as_deref()
        .is_some_and(|e| super::common::supported_extension_key_value("nu", e));
    data.numbering_system = if let Some(opt) = ns_static {
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
    data.locale = super::common::resolve_locale_keys(data.locale, &[("nu", ns_static)]);

    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_RELATIVE_TIME_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_relative_time_format_format_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    if !is_relative_time_format(&this_val) {
        return Err(ctx.throw_type_error("not a RelativeTimeFormat"));
    }
    make_bound_method(
        ctx,
        this_val,
        js_relative_time_format_format_bound,
        "format",
        2,
    )
}

fn is_relative_time_format(v: &JSValue) -> bool {
    v.as_object()
        .is_some_and(|o| o.payload().intl_relative_time_format().is_some())
}

fn js_relative_time_format_format_bound(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = cc.data(0);
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_relative_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let n = cc.arg(0).to_number(ctx)?.to_f64();
    let unit = to_utf8_string(ctx, cc.arg(1))?;
    let s = format_relative(this, n, &unit)?;
    Ok(make_str(ctx, s.into_bytes())?)
}

fn format_relative(this: &RelativeTimeFormatData, n: f64, unit: &str) -> Result<String, i32> {
    if n.is_nan() {
        return Ok("NaN".to_string());
    }
    // Accept both singular and plural unit names (e.g. "second" and "seconds").
    let (singular, plural): (&str, &str) = match unit {
        "second" | "seconds" => ("second", "seconds"),
        "minute" | "minutes" => ("minute", "minutes"),
        "hour" | "hours" => ("hour", "hours"),
        "day" | "days" => ("day", "days"),
        "week" | "weeks" => ("week", "weeks"),
        "month" | "months" => ("month", "months"),
        "quarter" | "quarters" => ("quarter", "quarters"),
        "year" | "years" => ("year", "years"),
        _ => return Err(0),
    };
    let singular = singular.to_string();
    let plural = plural.to_string();

    let abs = n.abs().round();
    let unit_word = if abs == 1.0 { singular.clone() } else { plural };
    // Group the integer part (en-US style) and apply the locale's numbering
    // system, mirroring the internal NumberFormat used by RTF.
    let format = |v: f64| -> String {
        let mut s = if v == v.trunc() {
            format!("{}", v as i64)
        } else {
            v.to_string()
        };
        if let Some(i) = s.find('.') {
            // group the integer portion
            let int = &s[..i];
            let frac = &s[i..];
            s = format!("{}{}", group_integer(int), frac);
        } else {
            s = group_integer(&s);
        }
        if this.numbering_system != "latn" {
            s = super::number_format::translate_digits(&s, &this.numbering_system);
        }
        s
    };
    let rel = if n < 0.0 {
        format!("{} {} ago", format(abs), unit_word)
    } else if n > 0.0 {
        format!("in {} {}", format(n), unit_word)
    } else {
        match this.numeric {
            RelNumeric::Auto => format!("this {}", singular),
            RelNumeric::Always => format!("in 0 {}", unit_word),
        }
    };
    Ok(rel)
}

/// Insert thousands separators into an integer digit string.
fn group_integer(int: &str) -> String {
    let chars: Vec<char> = int.chars().collect();
    let mut out = String::with_capacity(int.len() + int.len() / 3);
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

fn js_relative_time_format_format_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_relative_time_format()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let n = cc.arg(0).to_number(ctx)?.to_f64();
    let unit = to_utf8_string(ctx, cc.arg(1))?;
    let s = format_relative(this, n, &unit)?;
    let mut parts = Vec::new();
    let mut o = ctx.create_base_object()?;
    let _v1 = make_str(ctx, b"literal".to_vec())?;
    o.set_property_str(ctx, "type", _v1)?;
    let _v2 = make_str(ctx, s.into_bytes())?;
    o.set_property_str(ctx, "value", _v2)?;
    parts.push(o.into());
    Ok(values_to_array(ctx, parts)?)
}

fn js_relative_time_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a RelativeTimeFormat"))?;
    let this = this_obj
        .payload()
        .intl_relative_time_format()
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
    FieldDef::GETSET(
        "format",
        Some(js_relative_time_format_format_getter),
        None,
        0,
    ),
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
