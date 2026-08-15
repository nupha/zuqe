// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Shared helpers for the Intl implementation.

use icu::locale::{Locale, LocaleCanonicalizer};

use crate::{Array, ScopedContext, value::JSValue};

/// Canonicalize a single locale string into its canonical BCP-47 form, or
/// return `None` if it cannot be parsed as a valid language tag.
pub(crate) fn canonicalize_locale(ctx: &mut ScopedContext, s: &str) -> Result<Option<Locale>, i32> {
    let mut loc: Locale = match s.parse() {
        Ok(l) => l,
        Err(_) => return Ok(None),
    };
    let canonicalizer = LocaleCanonicalizer::new_extended();
    canonicalizer.canonicalize(&mut loc);
    Ok(Some(loc))
}

/// Convert a JS string value to an owned UTF-8 `String`.
pub(crate) fn to_utf8_string(ctx: &mut ScopedContext, v: JSValue) -> Result<String, i32> {
    let s = v.to_string(ctx)?;
    Ok(s.content().encode_utf8(false).into_owned())
}

/// ECMA-402 §8.2.1 `CanonicalizeLocaleList`: coerce `locales` to an array of
/// canonical locale strings (may contain duplicates, not deduplicated).
pub(crate) fn canonicalize_locale_list(
    ctx: &mut ScopedContext,
    locales: JSValue,
) -> Result<JSValue, i32> {
    // `ToObject(locales)` then iterate as an array-like, per spec §8.1
    // (`Locales` abstract operation iterates `CreateArrayFromList`).
    let obj = locales.objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;

    let mut out: Vec<JSValue> = Vec::with_capacity(len as usize);
    for i in 0..len {
        let v = obj.get_property_i64(ctx, i)?;
        let tag = to_utf8_string(ctx, v)?;
        if let Some(canonical) = canonicalize_locale(ctx, &tag)? {
            let str_ref = ctx.create_string_from_utf8(canonical.to_string().into_bytes())?;
            out.push(JSValue::make_string(str_ref));
        }
    }
    Ok(Array::from_values(ctx, out)?.into())
}

/// Build a JS string value from a `Locale` (its canonical string form).
pub(crate) fn locale_to_string(ctx: &mut ScopedContext, l: &Locale) -> Result<JSValue, i32> {
    Ok(JSValue::make_string(
        ctx.create_string_from_utf8(l.to_string().into_bytes())?,
    ))
}

/// Create a JS string value from UTF-8 bytes.
pub(crate) fn make_str(ctx: &mut ScopedContext, bytes: Vec<u8>) -> Result<JSValue, i32> {
    Ok(JSValue::make_string(ctx.create_string_from_utf8(bytes)?))
}

/// Convert a `Vec<JSValue>` into a JS array value.
pub(crate) fn values_to_array(
    ctx: &mut ScopedContext,
    values: Vec<JSValue>,
) -> Result<JSValue, i32> {
    Ok(Array::from_values(ctx, values)?.into())
}

/// Create a native function bound to `bound` (accessible via `cc.data(0)`
/// inside the callback). Used for the ECMA-402 accessor methods like
/// `NumberFormat.prototype.format`.
pub(crate) fn make_bound_method(
    ctx: &mut ScopedContext,
    bound: JSValue,
    callback: crate::function::CallbackFn,
    name: &str,
    length: u8,
) -> Result<JSValue, i32> {
    let f = ctx.create_native_function_with_data(
        crate::Callback::Func(callback),
        name,
        length,
        0,
        Box::new([bound]),
    )?;
    Ok(f.into())
}

/// Resolve the effective locale for an Intl constructor. Uses the first
/// canonical locale of `locales`; falls back to the runtime default locale.
pub(crate) fn resolve_locale(ctx: &mut ScopedContext, locales: JSValue) -> Result<Locale, i32> {
    if locales.is_undefined() || locales.is_null() {
        return default_locale();
    }
    let obj = locales.objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    if len == 0 {
        return default_locale();
    }
    let first = obj.get_property_i64(ctx, 0)?;
    let tag = to_utf8_string(ctx, first)?;
    if let Some(l) = canonicalize_locale(ctx, &tag)? {
        Ok(l)
    } else {
        default_locale()
    }
}

/// The runtime default locale (uses `en-US` unless overridden by the host).
fn default_locale() -> Result<Locale, i32> {
    Ok("en-US".parse().unwrap())
}
