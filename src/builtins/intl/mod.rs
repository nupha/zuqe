// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl` namespace object and helpers.
//!
//! Each Intl constructor is implemented as a native class (opaque payload)
//! wrapping an ICU4X formatter, registered lazily under the `Intl` namespace.

pub(crate) mod collator;
pub(crate) mod common;
pub(crate) mod date_time_format;
pub(crate) mod display_names;
pub(crate) mod list_format;
pub(crate) mod locale;
pub(crate) mod number_format;
pub(crate) mod plural_rules;
pub(crate) mod relative_time_format;
pub(crate) mod segmenter;

use crate::{
    Callback, JSValue, Object, context::ScopedContext, error::JSResult, function::CallContext,
    object::JS_PROP_CONFIGURABLE,
};

pub(crate) use common::*;

/// `Intl.getCanonicalLocales(locales)` (402 §8.2.1)
fn js_intl_getCanonicalLocales(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let locales = cc.arg(0);
    let arr = canonicalize_locale_list(ctx, locales)?;
    Ok(arr)
}

/// Build the `Intl` namespace object and install each constructor, including
/// its static `supportedLocalesOf` method. Called during context setup.
pub(crate) fn js_intl_init(ctx: &mut ScopedContext) -> Result<Object, i32> {
    use crate::object::JS_PROP_WRITABLE;

    let mut intl = ctx.create_base_object()?;

    // Intl[@@toStringTag] = "Intl" (non-writable, non-enumerable, configurable).
    {
        let tag = make_str(ctx, b"Intl".to_vec())?;
        intl.define_property_value_str(ctx, "[Symbol.toStringTag]", tag, JS_PROP_CONFIGURABLE)?;
    }

    // getCanonicalLocales
    let fn_obj = ctx.create_native_function(
        Callback::Func(js_intl_getCanonicalLocales),
        "getCanonicalLocales",
        1,
        0,
    )?;
    intl.define_property_value_str(
        ctx,
        "getCanonicalLocales",
        fn_obj.into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;

    // install each sub-constructor. Class ids are statically determined and
    // contiguous, so we reference the generated constants directly.
    let classes: &[(&str, crate::ClassID)] = &[
        ("Collator", crate::JS_CLASS_INTL_COLLATOR),
        ("DateTimeFormat", crate::JS_CLASS_INTL_DATE_TIME_FORMAT),
        ("DisplayNames", crate::JS_CLASS_INTL_DISPLAY_NAMES),
        ("ListFormat", crate::JS_CLASS_INTL_LIST_FORMAT),
        ("Locale", crate::JS_CLASS_INTL_LOCALE),
        ("NumberFormat", crate::JS_CLASS_INTL_NUMBER_FORMAT),
        ("PluralRules", crate::JS_CLASS_INTL_PLURAL_RULES),
        (
            "RelativeTimeFormat",
            crate::JS_CLASS_INTL_RELATIVE_TIME_FORMAT,
        ),
        ("Segmenter", crate::JS_CLASS_INTL_SEGMENTER),
    ];
    for (name, class_id) in classes {
        let mut ctor = ctx
            .class_constructor(*class_id)
            .ok_or_else(|| ctx.throw_internal_error("Intl class not initialized"))?;
        // static supportedLocalesOf(locales, options)
        let fn_obj = ctx.create_native_function(
            Callback::Func(js_intl_supported_locales_of),
            "supportedLocalesOf",
            1,
            0,
        )?;
        ctor.define_property_value_str(
            ctx,
            "supportedLocalesOf",
            fn_obj.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
        intl.define_property_value_str(
            ctx,
            name,
            ctor.into(),
            JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
        )?;
    }
    Ok(intl)
}

/// Shared static `supportedLocalesOf(locales, options)` for all Intl
/// constructors. Validates `localeMatcher`, then returns the canonicalized
/// locales that are actually supported (e.g. excluding `zxx`, "no linguistic
/// content").
fn js_intl_supported_locales_of(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    // Validate the `localeMatcher` option (ECMA-402 §8.2.2 step 5).
    let options = cc.arg(1);
    if options.is_object() {
        let obj = options.objectify(ctx)?;
        let matcher = obj.get_property_str(ctx, "localeMatcher")?;
        if !matcher.is_undefined() {
            let s = to_utf8_string(ctx, matcher)?;
            if s != "lookup" && s != "best fit" {
                return Err(ctx.throw_range_error("Invalid localeMatcher value"));
            }
        }
    }

    let arr = canonicalize_locale_list(ctx, cc.arg(0))?;
    // Filter out locales that are not supported by any Intl constructor.
    // The "no linguistic content" locale `zxx` is not supported.
    let mut out: Vec<JSValue> = Vec::new();
    if let Some(obj) = arr.as_object() {
        let len = obj.get_length_prop(ctx)?;
        for i in 0..len {
            let v = obj.get_property_i64(ctx, i)?;
            let tag = to_utf8_string(ctx, v)?;
            let language = tag.split(['-', '_']).next().unwrap_or("");
            if language == "zxx" {
                continue;
            }
            out.push(v);
        }
    }
    Ok(values_to_array(ctx, out)?.into())
}
