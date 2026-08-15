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
pub(crate) struct LocaleData {
    pub locale: IcuLocale,
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

    Ok(())
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
        _ => unreachable!(),
    })
}

const js_locale_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("toString", 0, js_locale_getters, 0),
    FieldDef::GETSET("language", Some(js_locale_getters), None, 1),
    FieldDef::GETSET("script", Some(js_locale_getters), None, 2),
    FieldDef::GETSET("region", Some(js_locale_getters), None, 3),
    FieldDef::GETSET("baseName", Some(js_locale_getters), None, 4),
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
