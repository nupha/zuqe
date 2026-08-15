// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.DisplayNames` (§17).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{locale_to_string, make_str, resolve_locale, to_utf8_string};

#[derive(Debug)]
pub(crate) struct DisplayNamesData {
    pub locale: Locale,
    pub style: DisplayStyle,
    pub display_type: DisplayType,
}

impl GcTrace for DisplayNamesData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Debug)]
pub(crate) enum DisplayStyle {
    Narrow,
    Short,
    Long,
    Abbreviated,
}

#[derive(Debug)]
pub(crate) enum DisplayType {
    Language,
    Region,
    Script,
    Currency,
    Calendar,
    DateTimeField,
}

pub(crate) static CLASS_DEF_DISPLAY_NAMES: ClassDef = ClassDef::builtin(
    Atom::CONST_DisplayNames,
    js_display_names_proto_funcs,
    Some(ClassCtorDef::new(js_display_names_constructor, 0, true)),
);

pub(crate) fn js_display_names_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let mut data = DisplayNamesData {
        locale,
        style: DisplayStyle::Long,
        display_type: DisplayType::Language,
    };

    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let style = obj.get_property_str(ctx, "style")?;
        if !style.is_undefined() {
            let s = to_utf8_string(ctx, style)?;
            data.style = match s.as_str() {
                "narrow" => DisplayStyle::Narrow,
                "short" => DisplayStyle::Short,
                "abbreviated" => DisplayStyle::Abbreviated,
                _ => DisplayStyle::Long,
            };
        }
        let t = obj.get_property_str(ctx, "type")?;
        if !t.is_undefined() {
            let s = to_utf8_string(ctx, t)?;
            data.display_type = match s.as_str() {
                "region" => DisplayType::Region,
                "script" => DisplayType::Script,
                "currency" => DisplayType::Currency,
                "calendar" => DisplayType::Calendar,
                "dateTimeField" => DisplayType::DateTimeField,
                _ => DisplayType::Language,
            };
        }
    }

    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_DISPLAY_NAMES(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_display_names_of(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;
    let this = this_obj
        .payload()
        .intl_display_names()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;
    let code = to_utf8_string(ctx, cc.arg(0))?;
    let name = display_name(this, &code)?;
    Ok(make_str(ctx, name.into_bytes())?)
}

fn display_name(this: &DisplayNamesData, code: &str) -> Result<String, i32> {
    // Pragmatic fallback data; returns the code itself when no mapping exists.
    let upper = code.to_uppercase();
    match this.display_type {
        DisplayType::Language => language_name(&this.locale, code),
        DisplayType::Region => region_name(&upper),
        DisplayType::Script => Ok(code.to_string()),
        DisplayType::Currency => currency_name(&upper),
        DisplayType::Calendar => Ok(code.to_string()),
        DisplayType::DateTimeField => Ok(code.to_string()),
    }
}

fn language_name(locale: &Locale, code: &str) -> Result<String, i32> {
    // en: English, zh: Chinese, ja: Japanese, de: German, fr: French, es: Spanish
    let _ = locale;
    Ok(match code {
        "en" => "English",
        "zh" => "Chinese",
        "ja" => "Japanese",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "ru" => "Russian",
        "ar" => "Arabic",
        "pt" => "Portuguese",
        "it" => "Italian",
        "ko" => "Korean",
        "nl" => "Dutch",
        _ => return Ok(code.to_string()),
    }
    .to_string())
}

fn region_name(upper: &str) -> Result<String, i32> {
    Ok(match upper {
        "US" => "United States",
        "CN" => "China",
        "JP" => "Japan",
        "GB" => "United Kingdom",
        "DE" => "Germany",
        "FR" => "France",
        "ES" => "Spain",
        "RU" => "Russia",
        "CA" => "Canada",
        "IN" => "India",
        "BR" => "Brazil",
        _ => return Ok(upper.to_string()),
    }
    .to_string())
}

fn currency_name(upper: &str) -> Result<String, i32> {
    Ok(match upper {
        "USD" => "US Dollar",
        "EUR" => "Euro",
        "CNY" => "Chinese Yuan",
        "JPY" => "Japanese Yen",
        "GBP" => "British Pound",
        _ => return Ok(upper.to_string()),
    }
    .to_string())
}

fn js_display_names_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;
    let p = this_obj
        .payload()
        .intl_display_names()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;

    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &p.locale)?;
    o.define_property_value_str(ctx, "locale", _v1, JS_PROP_C_W_E)?;
    let _v4 = make_str(
        ctx,
        match p.style {
            DisplayStyle::Narrow => b"narrow".to_vec(),
            DisplayStyle::Short => b"short".to_vec(),
            DisplayStyle::Long => b"long".to_vec(),
            DisplayStyle::Abbreviated => b"abbreviated".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "style", _v4, JS_PROP_C_W_E)?;
    let _v5 = make_str(
        ctx,
        match p.display_type {
            DisplayType::Language => b"language".to_vec(),
            DisplayType::Region => b"region".to_vec(),
            DisplayType::Script => b"script".to_vec(),
            DisplayType::Currency => b"currency".to_vec(),
            DisplayType::Calendar => b"calendar".to_vec(),
            DisplayType::DateTimeField => b"dateTimeField".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "type", _v5, JS_PROP_C_W_E)?;
    let _v2 = make_str(ctx, b"code".to_vec())?;
    o.define_property_value_str(ctx, "fallback", _v2, JS_PROP_C_W_E)?;
    let _v3 = make_str(ctx, b"dialect".to_vec())?;
    o.define_property_value_str(ctx, "languageDisplay", _v3, JS_PROP_C_W_E)?;

    Ok(o.into())
}

const js_display_names_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("of", 1, js_display_names_of, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_display_names_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.DisplayNames",
        JS_PROP_CONFIGURABLE,
    ),
];
