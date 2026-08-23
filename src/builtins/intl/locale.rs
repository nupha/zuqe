// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.Locale` (§10).

use icu::locale::Locale as IcuLocale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{canonicalize_locale, locale_to_string, make_str, to_utf8_string};

/// Opaque payload holding the ICU4X locale for a `Locale` instance.
#[derive(Debug)]
pub struct LocaleData {
    pub(crate) locale: IcuLocale,
}

impl GcTrace for LocaleData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

pub(crate) static CLASS_DEF_LOCALE: ClassDef = ClassDef::builtin(
    Atom::CONST_Locale,
    js_locale_proto_funcs,
    Some(ClassCtorDef::new(js_locale_constructor, 1, true)),
);

pub(crate) fn js_locale_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    let tag = to_utf8_string(ctx, cc.arg(0))?;
    let canonical = canonicalize_locale(ctx, &tag)?
        .ok_or_else(|| ctx.throw_range_error("Invalid language tag"))?;

    let mut data = LocaleData { locale: canonical };

    // `options` extension keys: calendar, collation, hourCycle, caseFirst,
    // numeric, numberingSystem, language, script, region, baseName, ...
    let options = cc.arg(1);
    if !options.is_undefined() {
        apply_locale_options(ctx, &mut data, options)?;
    }

    let instance = ctx
        .create_instance_with_ctor(new_target, ObjectPayload::INTL_LOCALE(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn apply_locale_options(
    ctx: &mut ScopedContext,
    data: &mut LocaleData,
    options: JSValue,
) -> Result<(), i32> {
    let obj = options.objectify(ctx)?;

    // language / script / region override the base subtags.
    let language = obj.get_property_str(ctx, "language")?;
    if !language.is_undefined() {
        if let Ok(l) = to_utf8_string(ctx, language)?.parse() {
            data.locale.id.language = l;
        }
    }
    let script = obj.get_property_str(ctx, "script")?;
    if !script.is_undefined() {
        if let Ok(s) = to_utf8_string(ctx, script)?.parse() {
            data.locale.id.script = Some(s);
        }
    }
    let region = obj.get_property_str(ctx, "region")?;
    if !region.is_undefined() {
        if let Ok(r) = to_utf8_string(ctx, region)?.parse() {
            data.locale.id.region = Some(r);
        }
    }

    // Unicode extension keywords: per ECMA-402, Locale option values are
    // validated against the BCP-47 `type` grammar (availability filtering is
    // left to the individual constructors). Calendar aliases are canonicalized
    // first (e.g. `islamicc` → `islamic-civil`).
    let calendar = obj.get_property_str(ctx, "calendar")?;
    if !calendar.is_undefined() {
        let s = to_utf8_string(ctx, calendar)?;
        if !super::number_format::is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid calendar"));
        }
        let canon = super::common::canonicalize_calendar_id(&s);
        set_unicode_keyword(&mut data.locale, "ca", canon)?;
    }
    let collation = obj.get_property_str(ctx, "collation")?;
    if !collation.is_undefined() {
        let s = to_utf8_string(ctx, collation)?;
        if !super::number_format::is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid collation"));
        }
        set_unicode_keyword(&mut data.locale, "co", &s)?;
    }
    let numbering_system = obj.get_property_str(ctx, "numberingSystem")?;
    if !numbering_system.is_undefined() {
        let s = to_utf8_string(ctx, numbering_system)?;
        if !super::number_format::is_valid_bcp47_type(&s) {
            return Err(ctx.throw_range_error("Invalid numberingSystem"));
        }
        set_unicode_keyword(&mut data.locale, "nu", &s)?;
    }

    Ok(())
}

/// Set a Unicode extension keyword on the locale (e.g. `ca` → `-u-ca-...`).
fn set_unicode_keyword(locale: &mut IcuLocale, key: &str, value: &str) -> Result<(), i32> {
    use icu::locale::extensions::unicode::{Key, Value};
    match (key.parse::<Key>(), Value::try_from_str(value)) {
        (Ok(k), Ok(v)) => {
            locale.extensions.unicode.keywords.set(k, v);
            Ok(())
        }
        _ => Err(-1),
    }
}

fn js_locale_getters(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let this = this_obj
        .payload()
        .intl_locale()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let l = &this.locale;
    Ok(match cc.magic() {
        0 => locale_to_string(ctx, l)?, // toString
        1 => make_str(ctx, l.id.language.to_string().into_bytes())?, // language
        2 => match l.id.script {
            // script
            Some(s) => make_str(ctx, s.to_string().into_bytes())?,
            None => JSValue::UNDEFINED,
        },
        3 => match l.id.region {
            // region
            Some(r) => make_str(ctx, r.to_string().into_bytes())?,
            None => JSValue::UNDEFINED,
        },
        4 => make_str(ctx, l.to_string().into_bytes())?, // baseName
        5 => keyword_getter(ctx, l, "ca")?,              // calendar
        6 => keyword_getter(ctx, l, "co")?,              // collation
        7 => keyword_getter(ctx, l, "nu")?,              // numberingSystem
        _ => unreachable!(),
    })
}

/// Read a Unicode extension keyword from the locale (`None` → undefined).
fn keyword_getter(
    ctx: &mut ScopedContext,
    l: &IcuLocale,
    key: &str,
) -> Result<JSValue, i32> {
    Ok(match super::common::locale_extension_value(l, key) {
        Some(v) => make_str(ctx, v.into_bytes())?,
        None => JSValue::UNDEFINED,
    })
}

const js_locale_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_locale_getters, 0),
    FieldDef::GETSET("language", Some(js_locale_getters), None, 1),
    FieldDef::GETSET("script", Some(js_locale_getters), None, 2),
    FieldDef::GETSET("region", Some(js_locale_getters), None, 3),
    FieldDef::GETSET("baseName", Some(js_locale_getters), None, 4),
    FieldDef::GETSET("calendar", Some(js_locale_getters), None, 5),
    FieldDef::GETSET("collation", Some(js_locale_getters), None, 6),
    FieldDef::GETSET("numberingSystem", Some(js_locale_getters), None, 7),
    FieldDef::FUNC("maximize", 0, js_locale_maximize, 0),
    FieldDef::FUNC("minimize", 0, js_locale_minimize, 0),
    FieldDef::STR("[Symbol.toStringTag]", "Intl.Locale", JS_PROP_CONFIGURABLE),
];

fn js_locale_maximize(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let this = this_obj
        .payload()
        .intl_locale()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let mut l = this.locale.clone();
    let expander = icu::locale::LocaleExpander::new_common();
    expander.maximize(&mut l.id);
    let data = LocaleData { locale: l };
    let instance = ctx
        .create_instance_with_ctor(cc.this(), ObjectPayload::INTL_LOCALE(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_locale_minimize(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let this = this_obj
        .payload()
        .intl_locale()
        .ok_or_else(|| ctx.throw_type_error("not a Locale"))?;
    let mut l = this.locale.clone();
    let expander = icu::locale::LocaleExpander::new_common();
    expander.minimize(&mut l.id);
    let data = LocaleData { locale: l };
    let instance = ctx
        .create_instance_with_ctor(cc.this(), ObjectPayload::INTL_LOCALE(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}
