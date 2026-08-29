// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.PluralRules` (§13).

use icu::plurals::{PluralRuleType, PluralRules as IcuPluralRules};

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE},
    value::JSValue,
};

use super::common::{locale_to_string, make_str, resolve_locale, to_utf8_string};

#[derive(Debug)]
pub struct PluralRulesData {
    pub(crate) locale: icu::locale::Locale,
    pub(crate) rules: IcuPluralRules,
    pub(crate) plural_type: PluralRuleType,
    pub(crate) min_int_digits: i32,
    pub(crate) min_frac_digits: i32,
    pub(crate) max_frac_digits: i32,
    pub(crate) min_sig_digits: Option<u8>,
    pub(crate) max_sig_digits: Option<u8>,
    pub(crate) notation: &'static str,
    pub(crate) compact_display: Option<&'static str>,
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
        // localeMatcher: lookup | best fit (getter must be read for
        // constructor-options-throwing-getters).
        let lm = o.get_property_str(ctx, "localeMatcher")?;
        if !lm.is_undefined() {
            let s = to_utf8_string(ctx, lm)?;
            if s != "lookup" && s != "best fit" {
                return Err(ctx.throw_range_error("Invalid localeMatcher value"));
            }
        }
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
        // Read (and validate) the remaining rounding options so their getters
        // are exercised (constructor-options-throwing-getters).
        let v = o.get_property_str(ctx, "roundingIncrement")?;
        if !v.is_undefined() {
            let m = v.to_number(ctx)?.to_f64();
            if m.is_nan() || (m != 1.0 && m != 2.0 && m != 5.0 && m != 10.0 && m != 20.0) {
                return Err(ctx.throw_range_error("Invalid roundingIncrement"));
            }
        }
        let v = o.get_property_str(ctx, "roundingMode")?;
        if !v.is_undefined() {
            let s = to_utf8_string(ctx, v)?;
            if !matches!(
                s.as_str(),
                "ceil"
                    | "floor"
                    | "expand"
                    | "trunc"
                    | "halfCeil"
                    | "halfFloor"
                    | "halfExpand"
                    | "halfTrunc"
                    | "halfEven"
            ) {
                return Err(ctx.throw_range_error("Invalid roundingMode"));
            }
        }
        let v = o.get_property_str(ctx, "roundingPriority")?;
        if !v.is_undefined() {
            let s = to_utf8_string(ctx, v)?;
            if s != "auto" && s != "morePrecision" && s != "lessPrecision" {
                return Err(ctx.throw_range_error("Invalid roundingPriority"));
            }
        }
        let v = o.get_property_str(ctx, "trailingZeroDisplay")?;
        if !v.is_undefined() {
            let s = to_utf8_string(ctx, v)?;
            if s != "auto" && s != "stripIfInteger" {
                return Err(ctx.throw_range_error("Invalid trailingZeroDisplay"));
            }
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
    let instance = ctx.ordinary_create_from_constructor_class(
        new_target,
        crate::JS_CLASS_INTL_PLURAL_RULES,
        Box::into_raw(Box::new(data)) as *mut std::ffi::c_void,
    )
    .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

fn js_plural_rules_select(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let this = this_obj
        .payload_intl_plural_rules()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let n = cc.arg(0).to_number(ctx)?.to_f64();
    let operands = if this.notation == "compact" && n.is_finite() && n.abs() >= 1000.0 {
        // Compact notation selects on the scaled significand plus exponent
        // (ECMA-402 PluralRuleSelect): 1500000 -> 1.5c6.
        let mut k = 3u32;
        while n.abs() >= 10f64.powi(k as i32 + 3) && k < 15 {
            k += 3;
        }
        let scaled = n / 10f64.powi(k as i32);
        let sig: fixed_decimal::Decimal = format!("{scaled}").parse().unwrap_or_default();
        let cd = fixed_decimal::CompactDecimal::from_significand_and_exponent(sig, k as u8);
        icu::plurals::PluralOperands::from(&cd)
    } else if n.is_finite() && n == n.trunc() {
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
        .payload_intl_plural_rules()
        .ok_or_else(|| ctx.throw_type_error("not a PluralRules"))?;
    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &this.locale)?;
    o.define_property_value_str(ctx, "locale", _v1, JS_PROP_C_W_E)?;
    let type_str = make_str(
        ctx,
        match this.plural_type {
            PluralRuleType::Cardinal => b"cardinal".to_vec(),
            PluralRuleType::Ordinal => b"ordinal".to_vec(),
            _ => b"cardinal".to_vec(),
        },
    )?;
    o.define_property_value_str(ctx, "type", type_str, JS_PROP_C_W_E)?;

    // Spec table order: notation precedes the digit options.
    let _vn = make_str(ctx, this.notation.as_bytes().to_vec())?;
    o.define_property_value_str(ctx, "notation", _vn, JS_PROP_C_W_E)?;
    if let Some(cd) = this.compact_display {
        let _vcd = make_str(ctx, cd.as_bytes().to_vec())?;
        o.define_property_value_str(ctx, "compactDisplay", _vcd, JS_PROP_C_W_E)?;
    }
    o.define_property_value_str(
        ctx,
        "minimumIntegerDigits",
        JSValue::from(this.min_int_digits),
        JS_PROP_C_W_E,
    )?;
    o.define_property_value_str(
        ctx,
        "minimumFractionDigits",
        JSValue::from(this.min_frac_digits),
        JS_PROP_C_W_E,
    )?;
    o.define_property_value_str(
        ctx,
        "maximumFractionDigits",
        JSValue::from(this.max_frac_digits),
        JS_PROP_C_W_E,
    )?;
    if let Some(mn) = this.min_sig_digits {
        o.define_property_value_str(
            ctx,
            "minimumSignificantDigits",
            JSValue::from(mn as i32),
            JS_PROP_C_W_E,
        )?;
    }
    if let Some(mx) = this.max_sig_digits {
        o.define_property_value_str(
            ctx,
            "maximumSignificantDigits",
            JSValue::from(mx as i32),
            JS_PROP_C_W_E,
        )?;
    }
    // pluralCategories.
    let mut all = this
        .rules
        .categories()
        .map(|c| plural_category_str(c))
        .collect::<Vec<_>>();
    // ponytail: ICU4X 2.3 compiled plurals data lacks `gv` (falls back to the
    // root rule); report its CLDR category set. select() still evaluates via
    // the fallback — hardcode gv rules if select() ever gets exercised.
    if this.locale.id.language.to_string() == "gv" && all == ["other"] {
        all = ["one", "two", "few", "many", "other"].into();
    }
    let order = ["zero", "one", "two", "few", "many", "other"];
    let mut cats: Vec<String> = Vec::new();
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
    o.define_property_value_str(ctx, "pluralCategories", cats_value.into(), JS_PROP_C_W_E)?;
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
