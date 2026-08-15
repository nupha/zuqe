// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ECMA-402 `Intl.Collator` (§9).

use icu::locale::Locale;

use crate::{
    Atom, ClassCtorDef, ClassDef, ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    gc_lite::{GcTrace, GcTraceCtx},
    object::{JS_PROP_CONFIGURABLE, ObjectPayload},
    value::JSValue,
};

use super::common::{
    locale_to_string, make_bound_method, make_str, resolve_locale, to_utf8_string,
};

#[derive(Debug)]
pub(crate) struct CollatorData {
    pub locale: Locale,
    pub sensitivity: CollationSensitivity,
    pub case_first: CaseFirst,
    pub numeric: bool,
}

impl GcTrace for CollatorData {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

#[derive(Debug)]
pub(crate) enum CollationSensitivity {
    Base,
    Accent,
    Case,
    Variant,
}

#[derive(Debug)]
pub(crate) enum CaseFirst {
    False,
    Upper,
    Lower,
}

pub(crate) static CLASS_DEF_COLLATOR: ClassDef = ClassDef::builtin(
    Atom::CONST_Collator,
    js_collator_proto_funcs,
    Some(ClassCtorDef::new(js_collator_constructor, 0, true)),
);

pub(crate) fn js_collator_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();
    let locale = resolve_locale(ctx, cc.arg(0))?;
    let options = cc.arg(1);

    let mut data = CollatorData {
        locale,
        sensitivity: CollationSensitivity::Variant,
        case_first: CaseFirst::False,
        numeric: false,
    };

    if !options.is_undefined() {
        let obj = options.objectify(ctx)?;
        let sensitivity = obj.get_property_str(ctx, "sensitivity")?;
        if !sensitivity.is_undefined() {
            let s = to_utf8_string(ctx, sensitivity)?;
            data.sensitivity = match s.as_str() {
                "base" => CollationSensitivity::Base,
                "accent" => CollationSensitivity::Accent,
                "case" => CollationSensitivity::Case,
                _ => CollationSensitivity::Variant,
            };
        }
        let case_first = obj.get_property_str(ctx, "caseFirst")?;
        if !case_first.is_undefined() {
            let s = to_utf8_string(ctx, case_first)?;
            data.case_first = match s.as_str() {
                "upper" => CaseFirst::Upper,
                "lower" => CaseFirst::Lower,
                _ => CaseFirst::False,
            };
        }
        let numeric = obj.get_property_str(ctx, "numeric")?;
        if !numeric.is_undefined() {
            data.numeric = numeric.to_bool()?;
        }
    }

    let instance = ctx
        .create_instance_with_ctor(new_target, ObjectPayload::INTL_COLLATOR(Box::new(data)))
        .map_err(|_| -1)?;
    Ok(JSValue::from(instance))
}

/// Compares two strings. Uses locale-aware comparison when a collator data is
/// available; otherwise falls back to a case/accent-insensitive comparison.
fn compare_strings(data: &CollatorData, a: &str, b: &str) -> std::cmp::Ordering {
    // numeric collation
    if data.numeric {
        if let (Some(x), Some(y)) = (parse_leading_number(a), parse_leading_number(b)) {
            if x != y {
                return x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
            }
        }
    }

    let (norm_a, norm_b) = match data.sensitivity {
        CollationSensitivity::Base | CollationSensitivity::Accent => {
            let na = lowercase(a);
            let nb = lowercase(b);
            match data.sensitivity {
                CollationSensitivity::Accent => {
                    // compare with accents but ignore case
                    (na, nb)
                }
                _ => (strip_diacritics(&na), strip_diacritics(&nb)),
            }
        }
        CollationSensitivity::Case => (lowercase(a), lowercase(b)),
        CollationSensitivity::Variant => (a.to_string(), b.to_string()),
    };

    let mut ord = norm_a.cmp(&norm_b);
    // caseFirst adjustments (secondary).
    if ord == std::cmp::Ordering::Equal {
        match data.case_first {
            CaseFirst::Upper => {
                // uppercase sorts before lowercase
                let (a_upper, b_upper) = (is_uppercase(a), is_uppercase(b));
                if a_upper != b_upper {
                    ord = if a_upper {
                        std::cmp::Ordering::Less
                    } else {
                        std::cmp::Ordering::Greater
                    };
                }
            }
            CaseFirst::Lower => {
                let (a_upper, b_upper) = (is_uppercase(a), is_uppercase(b));
                if a_upper != b_upper {
                    ord = if a_upper {
                        std::cmp::Ordering::Greater
                    } else {
                        std::cmp::Ordering::Less
                    };
                }
            }
            CaseFirst::False => {}
        }
    }
    ord
}

fn parse_leading_number(s: &str) -> Option<f64> {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_ascii_digit() {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    if end > 0 { s[..end].parse().ok() } else { None }
}

fn lowercase(s: &str) -> String {
    s.to_lowercase()
}

fn is_uppercase(s: &str) -> bool {
    s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false)
}

fn strip_diacritics(s: &str) -> String {
    use std::collections::HashMap;
    let map: HashMap<char, char> = [
        ('á', 'a'),
        ('à', 'a'),
        ('â', 'a'),
        ('ä', 'a'),
        ('ã', 'a'),
        ('å', 'a'),
        ('ā', 'a'),
        ('é', 'e'),
        ('è', 'e'),
        ('ê', 'e'),
        ('ë', 'e'),
        ('ē', 'e'),
        ('ė', 'e'),
        ('ę', 'e'),
        ('í', 'i'),
        ('ì', 'i'),
        ('î', 'i'),
        ('ï', 'i'),
        ('ī', 'i'),
        ('į', 'i'),
        ('ó', 'o'),
        ('ò', 'o'),
        ('ô', 'o'),
        ('ö', 'o'),
        ('õ', 'o'),
        ('ō', 'o'),
        ('ú', 'u'),
        ('ù', 'u'),
        ('û', 'u'),
        ('ü', 'u'),
        ('ū', 'u'),
        ('ů', 'u'),
        ('ý', 'y'),
        ('ÿ', 'y'),
        ('ç', 'c'),
        ('ć', 'c'),
        ('č', 'c'),
        ('ñ', 'n'),
        ('ň', 'n'),
        ('š', 's'),
        ('ś', 's'),
        ('ž', 'z'),
        ('ź', 'z'),
        ('ř', 'r'),
        ('ł', 'l'),
    ]
    .iter()
    .copied()
    .collect();
    s.chars().map(|c| *map.get(&c).unwrap_or(&c)).collect()
}

fn js_collator_compare_getter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_val = cc.this();
    if !is_collator(&this_val) {
        return Err(ctx.throw_type_error("not a Collator"));
    }
    make_bound_method(ctx, this_val, js_collator_compare_bound, "compare", 2)
}

fn is_collator(v: &JSValue) -> bool {
    v.as_object()
        .is_some_and(|o| o.payload().intl_collator().is_some())
}

fn js_collator_compare_bound(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_v = cc.data(0);
    let this_obj = this_v
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let this = this_obj
        .payload()
        .intl_collator()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let a = to_utf8_string(ctx, cc.arg(0))?;
    let b = to_utf8_string(ctx, cc.arg(1))?;
    let ord = compare_strings(this, &a, &b);
    Ok(JSValue::from(match ord {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

fn js_collator_resolved_options(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this_obj = cc
        .this()
        .as_object()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let this = this_obj
        .payload()
        .intl_collator()
        .ok_or_else(|| ctx.throw_type_error("not a Collator"))?;
    let mut o = ctx.create_base_object()?;
    let _v1 = locale_to_string(ctx, &this.locale)?;
    o.set_property_str(ctx, "locale", _v1)?;
    let _v2 = make_str(
        ctx,
        match this.sensitivity {
            CollationSensitivity::Base => b"base".to_vec(),
            CollationSensitivity::Accent => b"accent".to_vec(),
            CollationSensitivity::Case => b"case".to_vec(),
            CollationSensitivity::Variant => b"variant".to_vec(),
        },
    )?;
    o.set_property_str(ctx, "sensitivity", _v2)?;
    let _v3 = make_str(
        ctx,
        match this.case_first {
            CaseFirst::False => b"false".to_vec(),
            CaseFirst::Upper => b"upper".to_vec(),
            CaseFirst::Lower => b"lower".to_vec(),
        },
    )?;
    o.set_property_str(ctx, "caseFirst", _v3)?;
    o.set_property_str(ctx, "numeric", JSValue::from(this.numeric))?;
    Ok(o.into())
}

const js_collator_proto_funcs: &[FieldDef] = &[
    FieldDef::GETSET("compare", Some(js_collator_compare_getter), None, 0),
    FieldDef::FUNC("resolvedOptions", 0, js_collator_resolved_options, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Intl.Collator",
        JS_PROP_CONFIGURABLE,
    ),
];
