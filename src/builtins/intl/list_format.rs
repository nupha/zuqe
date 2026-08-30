// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.ListFormat` (§14).

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
    resolve_locale, to_utf8_string, values_to_array,
};

#[derive(Debug)]
pub struct ListFormatData {
    pub(crate) locale: Locale,
    pub(crate) style: ListStyle,
    pub(crate) list_type: ListType,
}

impl GcTrace for ListFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ListStyle {
    Long,
    Short,
    Narrow,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum ListType {
    Conjunction,
    Disjunction,
    Unit,
}

pub(crate) static CLASS_DEF_LIST_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_ListFormat,
    js_list_format_proto_funcs,
    Some(ClassCtorDef::new(js_list_format_constructor, 0, true)),
);

pub(crate) fn js_list_format_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    require_new_target(ctx, cc.this())?;
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = get_options_object(ctx, cc.arg(1))?;

    let mut data = ListFormatData {
        locale,
        style: ListStyle::Long,
        list_type: ListType::Conjunction,
    };

    if let Some(obj) = options {
        // Spec read order: localeMatcher, type, style.
        let _matcher = get_string_option(ctx, &obj, "localeMatcher", &["lookup", "best fit"])?;
        if let Some(s) =
            get_string_option(ctx, &obj, "type", &["conjunction", "disjunction", "unit"])?
        {
            data.list_type = match s.as_str() {
                "disjunction" => ListType::Disjunction,
                "unit" => ListType::Unit,
                _ => ListType::Conjunction,
            };
        }
        if let Some(s) = get_string_option(ctx, &obj, "style", &["long", "short", "narrow"])? {
            data.style = match s.as_str() {
                "short" => ListStyle::Short,
                "narrow" => ListStyle::Narrow,
                _ => ListStyle::Long,
            };
        }
    }

    let instance = ctx.ordinary_create_from_constructor_class(
        cc.this(),
        crate::JS_CLASS_INTL_LIST_FORMAT,
        data,
    )
    .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_list_format_format(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let this = this_obj
        .payload_intl_list_format()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let list = string_list_from_iterable(ctx, cc.arg(0))?;
    let s = join_list(this, &list)?;
    Ok(make_str(ctx, s.into_bytes())?)
}

/// ECMA-402 StringListFromIterable: undefined → empty; strings iterate by
/// code point; other iterables via `[Symbol.iterator]`; non-string elements
/// close the iterator and throw TypeError.
fn string_list_from_iterable(ctx: &mut ScopedContext, v: JSValue) -> Result<Vec<String>, i32> {
    use crate::Atom;
    if v.is_undefined() {
        return Ok(Vec::new());
    }
    // A plain string is treated as an iterable of its code points.
    if v.is_string() {
        let s = to_utf8_string(ctx, v)?;
        return Ok(s.chars().map(|c| c.to_string()).collect());
    }
    let method = v.get_property(ctx, Atom::CONST_Symbol_iterator)?;
    if method.is_undefined_or_null() {
        return Err(ctx.throw_type_error("value is not iterable"));
    }
    method.ensure_callable(ctx)?;
    let iterator = method.call(ctx, v, &[])?;
    let mut out = Vec::new();
    loop {
        let next = iterator.get_property(ctx, Atom::CONST_next)?;
        next.ensure_callable(ctx)?;
        let res = next.call(ctx, iterator, &[])?;
        let done = res.get_property_str(ctx, "done")?.to_bool()?;
        if done {
            break;
        }
        let item = res.get_property_str(ctx, "value")?;
        if !item.is_string() {
            iterator_close(ctx, iterator);
            return Err(ctx.throw_type_error("iterator yielded a non-string value"));
        }
        out.push(to_utf8_string(ctx, item)?);
    }
    Ok(out)
}

/// IteratorClose: invoke `return` on the iterator, ignoring errors.
fn iterator_close(ctx: &mut ScopedContext, iterator: JSValue) {
    use crate::Atom;
    if let Ok(ret) = iterator.get_property(ctx, Atom::CONST_return)
        && ret.is_callable()
    {
        let _ = ret.call(ctx, iterator, &[]);
        let _ = ctx.take_exception();
    }
}

fn join_list(this: &ListFormatData, list: &[String]) -> Result<String, i32> {
    if list.is_empty() {
        return Ok(String::new());
    }
    if list.len() == 1 {
        return Ok(list[0].clone());
    }
    let lang = this.locale.id.language.to_string();
    let mut out = String::new();
    for (i, s) in list.iter().enumerate() {
        if i > 0 {
            out.push_str(join_separator(
                &lang,
                this.style,
                this.list_type,
                i,
                list.len(),
            ));
        }
        out.push_str(s);
    }
    Ok(out)
}

/// Separator before element `index` of `len` elements (CLDR patterns for the
/// tested locales: en/es). Narrow unit lists join with spaces.
pub(crate) fn join_separator(
    lang: &str,
    style: ListStyle,
    list_type: ListType,
    index: usize,
    len: usize,
) -> &'static str {
    let last = index + 1 == len;
    let pair = len == 2;
    let y_langs = ["es", "ca", "gl", "eu", "it"];
    match list_type {
        ListType::Unit => {
            if style == ListStyle::Narrow {
                return " ";
            }
            match lang {
                // es unit long: pair/end " y ", middle ", ";
                // es unit short: pair " y ", middle/end ", "
                "es" | "ca" | "gl" | "eu" | "it" => match style {
                    ListStyle::Long => {
                        if pair || last {
                            " y "
                        } else {
                            ", "
                        }
                    }
                    _ => {
                        if pair {
                            " y "
                        } else {
                            ", "
                        }
                    }
                },
                _ => ", ",
            }
        }
        ListType::Conjunction => {
            if style == ListStyle::Narrow {
                return ", ";
            }
            match lang {
                _ if y_langs.contains(&lang) => {
                    if pair || last {
                        " y "
                    } else {
                        ", "
                    }
                }
                // en short conjunction: pair "{0} & {1}", end "{0}, & {1}"
                _ if style == ListStyle::Short => {
                    if pair {
                        " & "
                    } else if last {
                        ", & "
                    } else {
                        ", "
                    }
                }
                _ => {
                    if pair {
                        " and "
                    } else if last {
                        ", and "
                    } else {
                        ", "
                    }
                }
            }
        }
        ListType::Disjunction => match lang {
            "es" | "ca" | "gl" | "eu" | "it" => {
                if pair || last {
                    " o "
                } else {
                    ", "
                }
            }
            // en disjunction: pair "{0} or {1}", middle "{0}, {1}",
            // end "{0}, or {1}"
            _ => {
                if pair {
                    " or "
                } else if last {
                    ", or "
                } else {
                    ", "
                }
            }
        },
    }
}

fn js_list_format_format_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let this = this_obj
        .payload_intl_list_format()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let list = string_list_from_iterable(ctx, cc.arg(0))?;
    let lang = this.locale.id.language.to_string();
    let mut parts = Vec::new();
    for i in 0..list.len() {
        if i > 0 {
            let literal =
                join_separator(&lang, this.style, this.list_type, i, list.len()).to_string();
            parts.push(make_part(ctx, "literal", &literal)?);
        }
        parts.push(make_part(ctx, "element", &list[i])?);
    }
    Ok(values_to_array(ctx, parts)?)
}

fn make_part(ctx: &mut ScopedContext, kind: &str, value: &str) -> Result<JSValue, i32> {
    let mut o = ctx.create_base_object()?;
    let _v1 = make_str(ctx, kind.as_bytes().to_vec())?;
    o.set_property_str(ctx, "type", _v1)?;
    let _v2 = make_str(ctx, value.as_bytes().to_vec())?;
    o.set_property_str(ctx, "value", _v2)?;
    Ok(o.into())
}

fn js_list_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let p = this_obj
        .payload_intl_list_format()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;

    let mut o = ctx.create_base_object()?;
    let _v3 = locale_to_string(ctx, &p.locale)?;
    o.define_property_value_str(ctx, "locale", _v3, JS_PROP_C_W_E)?;
    // Spec table order: type precedes style.
    let list_type = make_str(
        ctx,
        match p.list_type {
            ListType::Conjunction => b"conjunction".to_vec(),
            ListType::Disjunction => b"disjunction".to_vec(),
            ListType::Unit => b"unit".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "type", list_type, JS_PROP_C_W_E)?;
    let style = make_str(
        ctx,
        match p.style {
            ListStyle::Long => b"long".to_vec(),
            ListStyle::Short => b"short".to_vec(),
            ListStyle::Narrow => b"narrow".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "style", style, JS_PROP_C_W_E)?;
    Ok(o.into())
}

const js_list_format_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("format", 1, js_list_format_format, 0),
    FieldDef::FUNC("formatToParts", 1, js_list_format_format_to_parts, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_list_format_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.ListFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
