// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_RAWJSON,
    atom::Atom,
    class::ClassDef,
    context::{ScopeResult, ScopedContext},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_WRITABLE, PropertyKey,
    },
    parse::{JSONParseRecord, JSParseState},
    runtime::js_check_stack_overflow,
    value::JSValue,
};

pub(crate) static CLASS_DEF_RAWJSON: ClassDef = ClassDef::builtin(Atom::CONST_RawJSON, &[], None);

fn js_json_stringify(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);
    let replacer = cc.arg(1);
    let space = cc.arg(2);

    ctx.with_new_scope(|cx| {
        let res = cx.json_stringify(obj, replacer, space);
        res
    })
}

// ---------------------------------------------------------------------------
// JSON.parse reviver context (json-parse-with-source proposal)
// ---------------------------------------------------------------------------
//
// Implementation mirrors QuickJS: during JSON parsing we build a
// `JSONParseRecord` tree that parallels the value tree.  Each node
// records:
//   • `value` — the original parsed JSValue (for forward-modification
//     detection via SameValue comparison).
//   • `source_pos` / `source_len` — the exact byte range in the source
//     text for primitive values (string, number, boolean, null).
//   • `children` / `elements` — child records for objects/arrays.
//
// In `internalize_json_property` we thread the parent record alongside
// `text_str` through the recursive walk.  Before calling the reviver we
// navigate from the parent record to the child record via `find_child`,
// check whether the current value still matches the original (forward
// modification detection), and if so, extract `context.source` from
// the raw source string using the recorded byte range.

/// Recursively promote all `JSValue`s inside a `JSONParseRecord` tree
/// from the (soon-to-be-destroyed) child parsing scope up to the parent
/// scope so that the GC nodes survive the scope transition.
fn promote_json_record_tree(record: &mut JSONParseRecord, scope: &ScopedContext<'_>) {
    record.value = record.value.promote(scope);
    for (_, child) in &mut record.children {
        promote_json_record_tree(child, scope);
    }
    for child in &mut record.elements {
        promote_json_record_tree(child, scope);
    }
}

fn internalize_json_property(
    ctx: &mut ScopedContext,
    holder: JSValue,
    name: Atom,
    reviver: JSValue,
    text_str: &str,
    parent_record: Option<&JSONParseRecord>,
) -> JSResult {
    if js_check_stack_overflow(ctx.rt(), 0) {
        return Err(ctx.throw_stack_overflow());
    }

    let mut val = holder.get_property(ctx, name)?;

    // Navigate from the parent record to the child record and detect
    // forward modification (the value was replaced by an earlier
    // reviver call via `this[k] = …`).
    let record = parent_record
        .and_then(|pr| pr.find_child(name))
        .filter(|pr| pr.value.is_same(val));

    if let Some(mut o) = val.as_object() {
        let is_array = o.is_array(ctx)?;
        let len: u32;
        let pks: Box<[PropertyKey]>;

        if is_array {
            len = o.get_length_prop(ctx)? as u32;
            pks = [].into();
        } else {
            pks = o.get_own_property_names(ctx, JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK)?;
            len = pks.len() as u32;
        }

        for i in 0..len {
            let prop = if is_array {
                Atom::from_i64(ctx, i as i64)?
            } else {
                unsafe { pks.get_unchecked(i as usize).atom }
            };

            let new_el = internalize_json_property(
                ctx, val, prop, reviver, text_str,
                record, // child's parent record (the current value's record)
            )?;
            if new_el.is_undefined() {
                val.delete_property(ctx, prop, 0)
            } else {
                val.define_property_value(ctx, prop, new_el, JS_PROP_C_W_E)
            }?;
        }
    }

    // Build the context argument (json-parse-with-source proposal)
    let context: JSValue = ctx.create_base_object()?.into();

    // Provide source only when:
    // 1. the record exists (value was a primitive in the original JSON)
    // 2. the current value matches the original snapshot (no forward modification)
    if let Some(pr) = record
        && pr.is_primitive()
    {
        let source_slice = unsafe {
            text_str
                .get_unchecked(pr.source_pos as usize..)
                .get_unchecked(..pr.source_len as usize)
        };
        let source_val = JSValue::make_string(ctx.create_string_from_str(source_slice)?);
        let mut context_obj = context.as_object().unwrap();
        context_obj.define_property_value(ctx, Atom::CONST_source, source_val, JS_PROP_C_W_E)?;
    }

    let name_val = name.to_value(ctx)?;
    reviver.call(ctx, holder, &[name_val, val, context])
}

fn js_json_parse(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;
    let has_reviver = cc.length() > 1 && cc.arg(1).is_callable();

    // Extract source string for reviver context (json-parse-with-source)
    let source_bytes: String =
        String::from_utf8_lossy(s.content().encode_utf8_bytes(true).as_ref()).to_string();

    let mut obj;
    let mut record = JSONParseRecord::default();
    let mut has_record = false;

    {
        let mut cp = ctx.ctx_ptr;
        let child_scope = cp.new_scope();
        let source = s.content().encode_utf8_bytes(true);

        let mut parser = JSParseState::new_for_json(child_scope, &source, "<input>");
        parser.ext_json = false;

        let res = if has_reviver {
            parser.json_parse(Some(&mut record))?
        } else {
            parser.json_parse(None)?
        };
        let (scope_back, _) = parser.finish(());
        let res = res.promote(&scope_back);

        if has_reviver {
            // Promote all JSValues inside the record tree so they survive
            // the child scope being dropped.
            promote_json_record_tree(&mut record, &scope_back);
            has_record = true;
        }

        obj = res;
    }

    if has_record {
        let reviver = cc.arg(1);

        // QuickJS style: wrap the parsed record in a synthetic root-object
        // record so that `internalize_json_property` can navigate from
        // `root[""]` to the top-level value via `find_child("")`.
        let mut root_record = JSONParseRecord::default();
        root_record
            .children
            .push((Atom::CONST_empty_string, record));

        let mut root: JSValue = ctx.create_base_object()?.into();
        root.define_property_value(ctx, Atom::CONST_empty_string, obj, JS_PROP_C_W_E)?;
        obj = internalize_json_property(
            ctx,
            root,
            Atom::CONST_empty_string,
            reviver,
            &source_bytes,
            Some(&root_record),
        )?;
    }

    Ok(obj)
}

// ---------------------------------------------------------------------------
// RawJSON
// ---------------------------------------------------------------------------

fn js_json_isRawJSON(_ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let val = cc.arg(0);
    let is_raw = val
        .as_object()
        .is_some_and(|o| o.class_id() == JS_CLASS_RAWJSON);
    Ok(JSValue::from(is_raw))
}

fn js_json_rawJSON(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.arg(0).to_string(ctx)?;
    let content = str.content().encode_utf8_bytes(true);
    let bytes = content.as_ref();

    // Spec: empty string → SyntaxError
    // Spec: first/last char is \t(0x09), \n(0x0A), \r(0x0D), space(0x20) → SyntaxError
    if bytes.is_empty() {
        return Err(ctx.throw_syntax_error("invalid rawJSON string"));
    }
    let first = bytes[0];
    let last = bytes[bytes.len() - 1];
    if first == 0x09
        || first == 0x0A
        || first == 0x0D
        || first == 0x20
        || last == 0x09
        || last == 0x0A
        || last == 0x0D
        || last == 0x20
    {
        return Err(ctx.throw_syntax_error("invalid rawJSON string"));
    }

    // Validate the string is parseable as JSON
    let result = {
        let mut cp = ctx.ctx_ptr;
        let child_scope = cp.new_scope();
        let mut parser = JSParseState::new_for_json(child_scope, bytes, "<input>");
        parser.ext_json = false;
        let res = parser.json_parse(None);
        let (scope_back, _) = parser.finish(());
        res.promote(&scope_back)
    };
    if result.is_err() {
        return Err(ctx.throw_syntax_error("invalid rawJSON string"));
    }

    // Create a RAWJSON object
    let mut obj = ctx
        .create_instance_with_class(JS_CLASS_RAWJSON, std::ptr::null_mut())
        .map_err(|_| -1)?;

    // Set the rawJSON property (enumerable, non-writable, non-configurable)
    obj.define_property_value(
        ctx,
        Atom::CONST_rawJSON,
        JSValue::make_string(str),
        JS_PROP_ENUMERABLE,
    )?;

    // Make the object non-extensible
    obj.prevent_extensions(ctx)?;

    Ok(JSValue::from(obj))
}

// ---------------------------------------------------------------------------
// Rust-side JSON API
// ---------------------------------------------------------------------------

/// Entry point for native code that wants the engine's JSON semantics
/// without going through the `JSON` global object (which may be absent in
/// a custom realm).  Both methods behave exactly like `JSON.parse` /
/// `JSON.stringify`, including throwing `SyntaxError` on malformed input.
pub struct JSON;

impl JSON {
    /// `JSON.parse(text)` — no reviver.
    pub fn parse(ctx: &mut ScopedContext, text: &str) -> JSResult {
        let mut cp = ctx.ctx_ptr;
        let child_scope = cp.new_scope();
        let mut parser = JSParseState::new_for_json(child_scope, text.as_bytes(), "<json>");
        parser.ext_json = false;
        let res = parser.json_parse(None);
        let (scope_back, _) = parser.finish(());
        // Promote out of the child scope before it is dropped.
        res.promote(&scope_back)
    }

    /// `JSON.stringify(value, replacer, space)`.
    /// Returns `undefined` for values that are not serializable, same as JS.
    pub fn stringify(
        ctx: &mut ScopedContext,
        value: JSValue,
        replacer: JSValue,
        space: JSValue,
    ) -> JSResult {
        ctx.with_new_scope(|cx| cx.json_stringify(value, replacer, space))
    }
}

const js_json_funcs: &[FieldDef] = &[
    FieldDef::FUNC("parse", 2, js_json_parse, 0),
    FieldDef::FUNC("stringify", 3, js_json_stringify, 0),
    FieldDef::FUNC("isRawJSON", 1, js_json_isRawJSON, 0),
    FieldDef::FUNC("rawJSON", 1, js_json_rawJSON, 0),
    FieldDef::STR("[Symbol.toStringTag]", "JSON", JS_PROP_CONFIGURABLE),
];

pub const js_json_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "JSON",
    js_json_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
