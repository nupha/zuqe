// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.DisplayNames` (§17).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
    value::JSValue,
};

use super::common::{
    get_options_object, get_string_option, locale_to_string, make_str, require_new_target,
    resolve_locale, to_utf8_string,
};

#[derive(Debug)]
pub struct DisplayNamesData {
    pub(crate) locale: Locale,
    pub(crate) style: DisplayStyle,
    pub(crate) display_type: DisplayType,
    /// `fallback: "none"` → `of()` returns undefined for unknown codes.
    pub(crate) fallback_none: bool,
    /// Resolved `fallback` option ("code" | "none"), reported verbatim.
    pub(crate) fallback_str: &'static str,
    /// Resolved `languageDisplay` option ("dialect" | "standard").
    pub(crate) language_display: &'static str,
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
    Some(ClassCtorDef::new(js_display_names_constructor, 2, true)),
);

pub(crate) fn js_display_names_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    require_new_target(ctx, cc.this())?;

    // ECMA-402 §17.3.1 step 2: OrdinaryCreateFromConstructor BEFORE
    // argument validation — the prototype getter must fire before any
    // ToObject / GetOption calls that may throw.
    let default_data = DisplayNamesData {
        locale: "und".parse().unwrap(),
        style: DisplayStyle::Long,
        display_type: DisplayType::Language,
        fallback_none: false,
        fallback_str: "code",
        language_display: "dialect",
    };
    let mut instance = match ctx.ordinary_create_from_constructor_class(
        cc.this(),
        crate::JS_CLASS_INTL_DISPLAY_NAMES,
        Box::into_raw(Box::new(default_data)) as *mut std::ffi::c_void,
    ) {
        Ok(o) => o,
        // ordinary_create_from_constructor  may leave a pending JS exception when
        // reading `new_target.prototype` fails (e.g. a poisoned getter).
        // Propagate the pending exception.
        Err(_) => {
            return Err(-1);
        }
    };

    // Step 3+: argument validation
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = get_options_object(ctx, cc.arg(1))?;

    let mut data = DisplayNamesData {
        locale,
        style: DisplayStyle::Long,
        display_type: DisplayType::Language,
        fallback_none: false,
        fallback_str: "code",
        language_display: "dialect",
    };

    // The `type` option is REQUIRED even when the whole options object is
    // absent/undefined.
    let mut style_opt = None;
    let mut fallback_opt = None;
    let mut lang_display_opt = None;
    let mut type_opt = None;
    if let Some(obj) = options {
        // Spec read order: localeMatcher, style, type, fallback, languageDisplay.
        let _matcher = get_string_option(ctx, &obj, "localeMatcher", &["lookup", "best fit"])?;
        style_opt = get_string_option(
            ctx,
            &obj,
            "style",
            &["narrow", "short", "long", "abbreviated"],
        )?;
        type_opt = get_string_option(
            ctx,
            &obj,
            "type",
            &[
                "language",
                "region",
                "script",
                "currency",
                "calendar",
                "dateTimeField",
            ],
        )?;
        fallback_opt = get_string_option(ctx, &obj, "fallback", &["code", "none"])?;
        lang_display_opt =
            get_string_option(ctx, &obj, "languageDisplay", &["dialect", "standard"])?;
    }
    let t = type_opt.ok_or_else(|| ctx.throw_type_error("type option is required"))?;
    data.display_type = match t.as_str() {
        "region" => DisplayType::Region,
        "script" => DisplayType::Script,
        "currency" => DisplayType::Currency,
        "calendar" => DisplayType::Calendar,
        "dateTimeField" => DisplayType::DateTimeField,
        _ => DisplayType::Language,
    };
    if let Some(s) = style_opt {
        data.style = match s.as_str() {
            "narrow" => DisplayStyle::Narrow,
            "short" => DisplayStyle::Short,
            "abbreviated" => DisplayStyle::Abbreviated,
            _ => DisplayStyle::Long,
        };
    }
    if let Some(s) = fallback_opt {
        data.fallback_none = s == "none";
        data.fallback_str = if s == "none" { "none" } else { "code" };
    }
    if let Some(s) = lang_display_opt {
        data.language_display = if s == "standard" {
            "standard"
        } else {
            "dialect"
        };
    }

    // Update the instance payload with validated data
    *instance
        .payload_intl_display_names_mut()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))? = data;

    Ok(JSValue::from(instance))
}

fn js_display_names_of(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;
    let this = this_obj
        .payload_intl_display_names()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;
    let code = to_utf8_string(ctx, cc.arg(0))?;
    let canon = canonical_code_for(ctx, this, &code)?;
    match display_name(this, canon.as_deref().unwrap_or(&code))? {
        Some(name) => Ok(make_str(ctx, name.into_bytes())?),
        // fallback: "none" — unknown code yields undefined.
        None => Ok(JSValue::UNDEFINED),
    }
}

/// CanonicalCodeForDisplayNames: validate + normalize `code` per the instance's
/// type. RangeError for malformed codes; `None` when the code is well-formed
/// but has no display data.
fn canonical_code_for(
    ctx: &mut ScopedContext,
    this: &DisplayNamesData,
    code: &str,
) -> Result<Option<String>, i32> {
    let is_ascii_alpha = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphabetic());
    match this.display_type {
        DisplayType::Language => {
            // Engines reject attribute-shaped u-sections here (`en-u-hebrew`).
            if !super::common::is_structurally_valid_language_tag_allow_attrs(code, false) {
                return Err(ctx.throw_range_error("Invalid language tag"));
            }
            Ok(super::common::canonicalize_locale_string(ctx, code)?)
        }
        DisplayType::Region => {
            let ok = (code.len() == 2 && is_ascii_alpha(code))
                || (code.len() == 3 && code.bytes().all(|b| b.is_ascii_digit()));
            if !ok {
                return Err(ctx.throw_range_error("Invalid region code"));
            }
            Ok(Some(code.to_uppercase()))
        }
        DisplayType::Script => {
            if code.len() != 4 || !is_ascii_alpha(code) {
                return Err(ctx.throw_range_error("Invalid script code"));
            }
            let mut c = code.to_lowercase();
            c[..1].make_ascii_uppercase();
            Ok(Some(c))
        }
        DisplayType::Currency => {
            if code.len() != 3 || !is_ascii_alpha(code) {
                return Err(ctx.throw_range_error("Invalid currency code"));
            }
            Ok(Some(code.to_uppercase()))
        }
        DisplayType::Calendar => {
            if !super::number_format::is_valid_bcp47_type(code) {
                return Err(ctx.throw_range_error("Invalid calendar code"));
            }
            Ok(Some(code.to_lowercase()))
        }
        DisplayType::DateTimeField => match code {
            "era" | "year" | "quarter" | "month" | "weekOfYear" | "weekday" | "day"
            | "dayPeriod" | "hour" | "minute" | "second" | "timeZoneName" => Ok(Some(code.into())),
            _ => Err(ctx.throw_range_error("Invalid dateTimeField code")),
        },
    }
}

/// `None` when the code has no data (honored per the `fallback` option).
fn display_name(this: &DisplayNamesData, code: &str) -> Result<Option<String>, i32> {
    let upper = code.to_uppercase();
    let known = |s: String| Some(s);
    let unknown = || {
        if this.fallback_none {
            None
        } else {
            Some(code.to_string())
        }
    };
    Ok(match this.display_type {
        DisplayType::Language => language_name(&this.locale, code)
            .map(known)
            .unwrap_or_else(unknown),
        DisplayType::Region => region_name(&upper).map(known).unwrap_or_else(unknown),
        DisplayType::Script => known(code.to_string()),
        DisplayType::Currency => currency_name(&upper).map(known).unwrap_or_else(unknown),
        DisplayType::Calendar => known(code.to_string()),
        DisplayType::DateTimeField => known(code.to_string()),
    })
}

fn language_name(locale: &Locale, code: &str) -> Option<String> {
    // en: English, zh: Chinese, ja: Japanese, de: German, fr: French, es: Spanish
    let _ = locale;
    let name = match code {
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
        _ => return None,
    };
    Some(name.to_string())
}

fn region_name(upper: &str) -> Option<String> {
    let name = match upper {
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
        _ => return None,
    };
    Some(name.to_string())
}

/// Currency display names come from the shared ISO 4217 table.
fn currency_name(upper: &str) -> Option<String> {
    super::common::SUPPORTED_CURRENCIES
        .iter()
        .find(|(c, _)| *c == upper)
        .map(|(_, n)| n.to_string())
}

fn js_display_names_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DisplayNames"))?;
    let p = this_obj
        .payload_intl_display_names()
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
    let _v2 = make_str(ctx, p.fallback_str.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "fallback", _v2, JS_PROP_C_W_E)?;
    let _v3 = make_str(ctx, p.language_display.as_bytes().to_vec())?;
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
