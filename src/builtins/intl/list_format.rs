// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.ListFormat` (§14).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{locale_to_string, make_str, resolve_locale, to_utf8_string, values_to_array};

#[derive(Debug)]
pub struct ListFormatData {
    pub(crate) locale: Locale,
    pub(crate) style: ListStyle,
    pub(crate) list_type: ListType,
}

impl GcTrace for ListFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Clone, Copy, Debug)]
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
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let mut data = ListFormatData {
        locale,
        style: ListStyle::Long,
        list_type: ListType::Conjunction,
    };

    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let style = obj.get_property_str(ctx, "style")?;
        if !style.is_undefined() {
            let s = to_utf8_string(ctx, style)?;
            data.style = match s.as_str() {
                "short" => ListStyle::Short,
                "narrow" => ListStyle::Narrow,
                _ => ListStyle::Long,
            };
        }
        let list_type = obj.get_property_str(ctx, "type")?;
        if !list_type.is_undefined() {
            let s = to_utf8_string(ctx, list_type)?;
            data.list_type = match s.as_str() {
                "disjunction" => ListType::Disjunction,
                "unit" => ListType::Unit,
                _ => ListType::Conjunction,
            };
        }
    }

    let instance = ctx
        .create_instance_with_ctor(new_target, ObjectPayload::INTL_LIST_FORMAT(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_list_format_format(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let this = this_obj
        .payload()
        .intl_list_format()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let list = to_string_list(ctx, cc.arg(0))?;
    let s = join_list(ctx, this, &list)?;
    Ok(make_str(ctx, s.into_bytes())?)
}

fn to_string_list(ctx: &mut ScopedContext, v: JSValue) -> Result<Vec<String>, i32> {
    let obj = v.objectify(ctx)?;
    let len = obj.get_length_prop(ctx)?;
    let mut out = Vec::with_capacity(len as usize);
    for i in 0..len {
        let v = obj.get_property_i64(ctx, i)?;
        out.push(to_utf8_string(ctx, v)?);
    }
    Ok(out)
}

fn join_list(
    ctx: &mut ScopedContext,
    this: &ListFormatData,
    list: &[String],
) -> Result<String, i32> {
    let _ = ctx;
    if list.is_empty() {
        return Ok(String::new());
    }
    if list.len() == 1 {
        return Ok(list[0].clone());
    }
    if list.len() == 2 {
        return Ok(match (this.style, &this.list_type) {
            (ListStyle::Narrow, _) => format!("{} & {}", list[0], list[1]),
            _ => match this.list_type {
                ListType::Conjunction => format!("{} and {}", list[0], list[1]),
                ListType::Disjunction => format!("{} or {}", list[0], list[1]),
                ListType::Unit => format!("{} {}", list[0], list[1]),
            },
        });
    }
    let last = &list[list.len() - 1];
    let joined = list[..list.len() - 1].join(", ");
    Ok(match (this.style, &this.list_type) {
        (ListStyle::Narrow, _) => format!("{} & {}", joined, last),
        _ => match this.list_type {
            ListType::Conjunction => format!("{}, and {}", joined, last),
            ListType::Disjunction => format!("{}, or {}", joined, last),
            ListType::Unit => format!("{}, {}", joined, last),
        },
    })
}

fn js_list_format_format_to_parts(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let this = this_obj
        .payload()
        .intl_list_format()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;
    let list = to_string_list(ctx, cc.arg(0))?;
    let mut parts = Vec::new();
    if list.is_empty() {
        return Ok(values_to_array(ctx, parts)?);
    }
    for i in 0..list.len() {
        parts.push(make_part(ctx, "element", &list[i])?);
        if i < list.len() - 1 {
            let literal = literal_for(ctx, this)?;
            parts.push(make_part(ctx, "literal", &literal)?);
        }
    }
    Ok(values_to_array(ctx, parts)?)
}

fn literal_for(ctx: &mut ScopedContext, this: &ListFormatData) -> Result<String, i32> {
    let _ = ctx;
    Ok(match this.list_type {
        ListType::Conjunction => ", ",
        ListType::Disjunction => ", ",
        ListType::Unit => ", ",
    }
    .to_string())
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
        .payload()
        .intl_list_format()
        .ok_or_else(|| ctx.throw_type_error("not a ListFormat"))?;

    let mut o = ctx.create_base_object()?;
    let _v3 = locale_to_string(ctx, &p.locale)?;
    o.define_property_value_str(ctx, "locale", _v3, JS_PROP_C_W_E)?;
    let style = make_str(
        ctx,
        match p.style {
            ListStyle::Long => b"long".to_vec(),
            ListStyle::Short => b"short".to_vec(),
            ListStyle::Narrow => b"narrow".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "style", style, JS_PROP_C_W_E)?;
    let list_type = make_str(
        ctx,
        match p.list_type {
            ListType::Conjunction => b"conjunction".to_vec(),
            ListType::Disjunction => b"disjunction".to_vec(),
            ListType::Unit => b"unit".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "type", list_type, JS_PROP_C_W_E)?;
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
