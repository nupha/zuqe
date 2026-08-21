// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.DurationFormat` (§15).
//!
//! ponytail: constructor + resolvedOptions only; duration formatting
//! (`format`/`formatToParts`) is not implemented yet.

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::{
    common::{locale_to_string, make_str, resolve_locale, to_utf8_string},
    number_format::is_valid_bcp47_type,
};

#[derive(Debug)]
pub struct DurationFormatData {
    pub(crate) locale: Locale,
    pub(crate) numbering_system: String,
}

impl GcTrace for DurationFormatData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

pub(crate) static CLASS_DEF_DURATION_FORMAT: ClassDef = ClassDef::builtin(
    Atom::CONST_DurationFormat,
    js_duration_format_proto_funcs,
    Some(ClassCtorDef::new(js_duration_format_constructor, 0, true)),
);

fn js_duration_format_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;

    let mut data = DurationFormatData {
        locale,
        numbering_system: "latn".to_string(),
    };

    // numberingSystem option must match the BCP47 `type` nonterminal.
    let options = cc.arg(1);
    let numbering_opt = if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let v = obj.get_property_str(ctx, "numberingSystem")?;
        if v.is_undefined() {
            None
        } else {
            let s = to_utf8_string(ctx, v)?;
            if !is_valid_bcp47_type(&s) {
                return Err(ctx.throw_range_error("Invalid numberingSystem value"));
            }
            Some(s)
        }
    } else {
        None
    };
    let nu_static = numbering_opt
        .as_deref()
        .map(|s| Box::leak(s.to_owned().into_boxed_str()) as &'static str);
    data.locale = super::common::resolve_locale_keys(data.locale, &[("nu", nu_static)]);

    // Effective numbering system: explicit valid option > `nu` extension > latn.
    let nu_ext = super::common::locale_extension_value(&data.locale, "nu");
    data.numbering_system = if let Some(opt) = nu_static {
        if super::common::supported_extension_key_value("nu", opt) {
            opt.to_string()
        } else if let Some(ext) = nu_ext {
            ext
        } else {
            "latn".to_string()
        }
    } else if let Some(ext) = nu_ext {
        ext
    } else {
        "latn".to_string()
    };

    let instance = ctx
        .create_instance_with_ctor(
            new_target,
            ObjectPayload::INTL_DURATION_FORMAT(Box::new(data)),
        )
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_duration_format_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let this = this_obj
        .payload()
        .intl_duration_format()
        .ok_or_else(|| ctx.throw_type_error("not a DurationFormat"))?;
    let mut o = ctx.create_base_object()?;
    let loc = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", loc, JS_PROP_C_W_E)?;
    let ns = make_str(ctx, this.numbering_system.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "numberingSystem", ns, JS_PROP_C_W_E)?;
    Ok(o.into())
}

const js_duration_format_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("resolvedOptions", 0, js_duration_format_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.DurationFormat",
        JS_PROP_CONFIGURABLE,
    ),
];
