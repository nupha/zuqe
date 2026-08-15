// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.PluralRules` (§13).

use icu::plurals::{PluralRuleType, PluralRules as IcuPluralRules};

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{locale_to_string, make_str, resolve_locale, to_utf8_string};

#[derive(Debug)]
pub(crate) struct PluralRulesData {
    pub locale: icu::locale::Locale,
    pub rules: IcuPluralRules,
    pub plural_type: PluralRuleType,
}

impl GcTrace for PluralRulesData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

pub(crate) static CLASS_DEF_PLURAL_RULES: ClassDef = ClassDef::builtin(
    Atom::CONST_PluralRules,
    js_plural_rules_proto_funcs,
    Some(ClassCtorDef::new(js_plural_rules_constructor, 0, true)),
);

pub(crate) fn js_plural_rules_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    // type: cardinal | ordinal (default cardinal)
    let plural_type = if options.is_undefined() {
        PluralRuleType::Cardinal
    } else {
        let obj = options.objectify(ctx)?;
        let v = obj.get_property_str(ctx, "type")?;
        if !v.is_undefined() {
            let s = to_utf8_string(ctx, v)?;
            if s == "ordinal" {
                PluralRuleType::Ordinal
            } else {
                PluralRuleType::Cardinal
            }
        } else {
            PluralRuleType::Cardinal
        }
    };

    let rules = IcuPluralRules::try_new(locale.clone().into(), Default::default())
        .map_err(|_| ctx.throw_range_error("Unable to load locale data"))?;

    let data = PluralRulesData {
        locale,
        rules,
        plural_type,
    };
    let instance = ctx
        .create_instance_with_ctor(new_target, ObjectPayload::INTL_PLURAL_RULES(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_plural_rules_select(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let this = this_obj
        .payload()
        .intl_plural_rules()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let n = cc.arg(0).to_number(ctx)?.to_f64();
    let operands = if n.is_finite() && n == n.trunc() {
        icu::plurals::PluralOperands::from(n as i64)
    } else {
        let d: fixed_decimal::Decimal = n.to_string().parse().unwrap_or_default();
        icu::plurals::PluralOperands::from(&d)
    };
    let cat = this.rules.category_for(operands);
    let s = plural_category_str(cat);
    Ok(make_str(ctx, s.as_bytes().to_vec())?)
}

fn plural_category_str(cat: icu::plurals::PluralCategory) -> &'static str {
    use icu::plurals::PluralCategory::*;
    match cat {
        Zero => "zero",
        One => "one",
        Two => "two",
        Few => "few",
        Many => "many",
        Other => "other",
    }
}

fn js_plural_rules_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let this = this_obj
        .payload()
        .intl_plural_rules()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &this.locale)?;
    o.set_property_str(ctx, "locale", _v1)?;
    let type_str = make_str(
        ctx,
        match this.plural_type {
            PluralRuleType::Cardinal => b"cardinal".to_vec(),
            PluralRuleType::Ordinal => b"ordinal".to_vec(),
            _ => b"cardinal".to_vec(),
        },
    )?;
    o.set_property_str(ctx, "type", type_str)?;
    Ok(o.into())
}

const js_plural_rules_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("select", 1, js_plural_rules_select, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_plural_rules_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.PluralRules",
        JS_PROP_CONFIGURABLE,
    ),
];
