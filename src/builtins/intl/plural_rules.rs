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
    pub min_int_digits: i32,
    pub min_frac_digits: i32,
    pub max_frac_digits: i32,
    pub min_sig_digits: Option<u8>,
    pub max_sig_digits: Option<u8>,
    pub notation: &'static str,
    pub compact_display: Option<&'static str>,
}

impl GcTrace for PluralRulesData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

pub(crate) static CLASS_DEF_PLURAL_RULES: ClassDef = ClassDef::builtin(
    Atom::CONST_PluralRules,
    js_plural_rules_proto_funcs,
    Some(ClassCtorDef::new(js_plural_rules_constructor, 0, false)),
);

pub(crate) fn js_plural_rules_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let obj = if options.is_undefined() {
        None
    } else {
        Some(options.objectify(ctx)?)
    };

    // type: cardinal | ordinal (default cardinal), invalid -> RangeError.
    let plural_type = {
        let v = if let Some(o) = &obj {
            o.get_property_str(ctx, "type")?
        } else {
            JSValue::UNDEFINED
        };
        if v.is_undefined() {
            PluralRuleType::Cardinal
        } else {
            let s = to_utf8_string(ctx, v)?;
            match s.as_str() {
                "cardinal" => PluralRuleType::Cardinal,
                "ordinal" => PluralRuleType::Ordinal,
                _ => return Err(ctx.throw_range_error("Invalid type value")),
            }
        }
    };

    let rules = IcuPluralRules::try_new(locale.clone().into(), Default::default())
        .map_err(|_| ctx.throw_range_error("Unable to load locale data"))?;

    // Rounding options.
    let mut min_int_digits = 1i32;
    let mut min_frac_digits = 0i32;
    let mut max_frac_digits = 3i32;
    let mut min_sig_digits = None;
    let mut max_sig_digits = None;
    let mut notation = "standard";
    let mut compact_display = None;

    if let Some(o) = &obj {
        let v = o.get_property_str(ctx, "minimumIntegerDigits")?;
        if !v.is_undefined() {
            let m = v.to_number(ctx)?.to_f64();
            if m.is_nan() || m < 1.0 || m > 21.0 {
                return Err(ctx.throw_range_error("Invalid minimumIntegerDigits"));
            }
            min_int_digits = m as i32;
        }
        let v = o.get_property_str(ctx, "minimumFractionDigits")?;
        if !v.is_undefined() {
            let m = v.to_number(ctx)?.to_f64();
            if m.is_nan() || m < 0.0 || m > 100.0 {
                return Err(ctx.throw_range_error("Invalid minimumFractionDigits"));
            }
            min_frac_digits = m as i32;
        }
        let v = o.get_property_str(ctx, "maximumFractionDigits")?;
        if !v.is_undefined() {
            let m = v.to_number(ctx)?.to_f64();
            if m.is_nan() || m < 0.0 || m > 100.0 {
                return Err(ctx.throw_range_error("Invalid maximumFractionDigits"));
            }
            max_frac_digits = m as i32;
        }
        let vs = o.get_property_str(ctx, "minimumSignificantDigits")?;
        if !vs.is_undefined() {
            let m = vs.to_number(ctx)?.to_f64();
            if m.is_nan() || m < 1.0 || m > 21.0 {
                return Err(ctx.throw_range_error("Invalid significant digits"));
            }
            min_sig_digits = Some(m as u8);
        }
        let vs = o.get_property_str(ctx, "maximumSignificantDigits")?;
        if !vs.is_undefined() {
            let m = vs.to_number(ctx)?.to_f64();
            if m.is_nan() || m < 1.0 || m > 21.0 {
                return Err(ctx.throw_range_error("Invalid significant digits"));
            }
            max_sig_digits = Some(m as u8);
        }
        let v = o.get_property_str(ctx, "notation")?;
        if !v.is_undefined() {
            let s = to_utf8_string(ctx, v)?;
            match s.as_str() {
                "standard" => notation = "standard",
                "compact" => {
                    notation = "compact";
                    compact_display = Some("short");
                }
                "scientific" => notation = "scientific",
                "engineering" => notation = "engineering",
                _ => return Err(ctx.throw_range_error("Invalid notation value")),
            }
        }
        // compactDisplay is only meaningful (and only reported) for compact.
        let v = o.get_property_str(ctx, "compactDisplay")?;
        if !v.is_undefined() {
            let s = to_utf8_string(ctx, v)?;
            match s.as_str() {
                "short" => compact_display = Some("short"),
                "long" => compact_display = Some("long"),
                _ => return Err(ctx.throw_range_error("Invalid compactDisplay value")),
            }
        }
        if notation != "compact" {
            compact_display = None;
        }
    }

    let data = PluralRulesData {
        locale,
        rules,
        plural_type,
        min_int_digits,
        min_frac_digits,
        max_frac_digits,
        min_sig_digits,
        max_sig_digits,
        notation,
        compact_display,
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
    o.set_property_str(
        ctx,
        "minimumIntegerDigits",
        JSValue::from(this.min_int_digits),
    )?;
    o.set_property_str(
        ctx,
        "minimumFractionDigits",
        JSValue::from(this.min_frac_digits),
    )?;
    o.set_property_str(
        ctx,
        "maximumFractionDigits",
        JSValue::from(this.max_frac_digits),
    )?;
    if let Some(mn) = this.min_sig_digits {
        o.set_property_str(ctx, "minimumSignificantDigits", JSValue::from(mn as i32))?;
    }
    if let Some(mx) = this.max_sig_digits {
        o.set_property_str(ctx, "maximumSignificantDigits", JSValue::from(mx as i32))?;
    }
    // notation / compactDisplay.
    let _vn = make_str(ctx, this.notation.as_bytes().to_vec())?;
    o.set_property_str(ctx, "notation", _vn)?;
    if let Some(cd) = this.compact_display {
        let _vcd = make_str(ctx, cd.as_bytes().to_vec())?;
        o.set_property_str(ctx, "compactDisplay", _vcd)?;
    }
    // pluralCategories.
    let mut cats: Vec<String> = Vec::new();
    let all = this
        .rules
        .categories()
        .map(|c| plural_category_str(c))
        .collect::<Vec<_>>();
    let order = ["zero", "one", "two", "few", "many", "other"];
    for want in order.iter() {
        if all.contains(want) {
            cats.push(want.to_string());
        }
    }
    let mut cat_arr = Vec::new();
    for c in cats {
        cat_arr.push(make_str(ctx, c.into_bytes())?);
    }
    let cats_value = crate::object::Array::from_values(ctx, cat_arr)?;
    o.set_property_str(ctx, "pluralCategories", cats_value.into())?;
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
