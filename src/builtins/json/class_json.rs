// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::collections::HashMap;

use crate::{
    JS_CLASS_RAWJSON,
    atom::Atom,
    class::ClassDef,
    context::{ScopeResult, ScopedContext},
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_WRITABLE, ObjectPayload, PropertyKey,
    },
    parse::JSParseState,
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

/// Build a map from value path to original source text by scanning the JSON
/// source string. Only primitive values (strings, numbers, booleans, null)
/// are recorded — objects and arrays are omitted.
fn build_source_map(source: &str) -> HashMap<String, String> {
    let bytes = source.as_bytes();
    let mut pos = 0;
    let mut map = HashMap::new();

    fn skip_ws(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\n' | b'\r') {
            *pos += 1;
        }
    }

    fn scan_string(bytes: &[u8], pos: &mut usize) {
        debug_assert_eq!(bytes[*pos], b'"');
        *pos += 1; // skip opening quote
        while *pos < bytes.len() && bytes[*pos] != b'"' {
            if bytes[*pos] == b'\\' {
                *pos += 1; // skip escape char
            }
            *pos += 1;
        }
        *pos += 1; // skip closing quote
    }

    fn scan_primitive(bytes: &[u8], pos: &mut usize) {
        while *pos < bytes.len()
            && (bytes[*pos].is_ascii_alphanumeric()
                || bytes[*pos] == b'-'
                || bytes[*pos] == b'+'
                || bytes[*pos] == b'.'
                || bytes[*pos] == b'e'
                || bytes[*pos] == b'E')
        {
            *pos += 1;
        }
    }

    fn scan_value(bytes: &[u8], pos: &mut usize, path: &str, map: &mut HashMap<String, String>) {
        skip_ws(bytes, pos);
        let start = *pos;
        match bytes[*pos] {
            b'{' => {
                *pos += 1; // skip '{'
                skip_ws(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b'}' {
                    *pos += 1;
                    return;
                }
                loop {
                    skip_ws(bytes, pos);
                    let key_start = *pos;
                    scan_string(bytes, pos);
                    // get key text (without surrounding quotes)
                    let key = String::from_utf8_lossy(&bytes[key_start + 1..*pos - 1]).to_string();

                    skip_ws(bytes, pos);
                    debug_assert!(*pos < bytes.len() && bytes[*pos] == b':');
                    *pos += 1; // skip ':'

                    let child_path = format!("{}/{}", path, key);
                    scan_value(bytes, pos, &child_path, map);

                    skip_ws(bytes, pos);
                    if *pos < bytes.len() && bytes[*pos] == b',' {
                        *pos += 1;
                        continue;
                    }
                    break;
                }
                skip_ws(bytes, pos);
                debug_assert!(*pos < bytes.len() && bytes[*pos] == b'}');
                *pos += 1; // skip '}'
            }
            b'[' => {
                *pos += 1; // skip '['
                skip_ws(bytes, pos);
                if *pos < bytes.len() && bytes[*pos] == b']' {
                    *pos += 1;
                    return;
                }
                let mut index = 0u32;
                loop {
                    let child_path = format!("{}/{}", path, index);
                    scan_value(bytes, pos, &child_path, map);
                    index += 1;

                    skip_ws(bytes, pos);
                    if *pos < bytes.len() && bytes[*pos] == b',' {
                        *pos += 1;
                        continue;
                    }
                    break;
                }
                skip_ws(bytes, pos);
                debug_assert!(*pos < bytes.len() && bytes[*pos] == b']');
                *pos += 1; // skip ']'
            }
            b'"' => {
                scan_string(bytes, pos);
                let end = *pos;
                map.insert(
                    path.to_string(),
                    String::from_utf8_lossy(&bytes[start..end]).to_string(),
                );
            }
            _ => {
                scan_primitive(bytes, pos);
                let end = *pos;
                map.insert(
                    path.to_string(),
                    String::from_utf8_lossy(&bytes[start..end]).to_string(),
                );
            }
        }
    }

    skip_ws(bytes, &mut pos);
    scan_value(bytes, &mut pos, "", &mut map);
    map
}

/// Pre-pass: walk the parsed JSON value tree and record every primitive
/// value together with its path.  This snapshot is taken *before* any
/// reviver runs, so forward-modifications (e.g. `this[k] = …`) can be
/// detected later by comparing the current value against the snapshot.
fn capture_original_values(
    ctx: &mut ScopedContext,
    root: JSValue,
) -> Result<HashMap<String, JSValue>, i32> {
    let mut map = HashMap::new();

    fn walk(
        ctx: &mut ScopedContext,
        val: JSValue,
        path: &str,
        map: &mut HashMap<String, JSValue>,
    ) -> Result<(), i32> {
        if js_check_stack_overflow(ctx.rt(), 0) {
            ctx.throw_stack_overflow();
            return Err(-1);
        }

        // Record the current value if it's a primitive
        if val.as_object().is_none() {
            map.insert(path.to_string(), val);
        }

        if let Some(mut o) = val.as_object().copied() {
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

                let child_val = val.get_property(ctx, prop)?;

                // build path key string
                let key_str = if is_array {
                    i.to_string()
                } else if prop.is_small_int() {
                    prop.to_small_int().unwrap().to_string()
                } else {
                    prop.descriptor(ctx.rt()).encode_utf8(false).into_owned()
                };
                let child_path = format!("{}/{}", path, key_str);

                walk(ctx, child_val, &child_path, map)?;
            }
        }

        Ok(())
    }

    walk(ctx, root, "", &mut map)?;
    Ok(map)
}

fn internalize_json_property(
    ctx: &mut ScopedContext,
    holder: JSValue,
    name: Atom,
    reviver: JSValue,
    source_map: &HashMap<String, String>,
    original_values: &HashMap<String, JSValue>,
    path: &str,
) -> JSResult {
    if js_check_stack_overflow(ctx.rt(), 0) {
        ctx.throw_stack_overflow();
        return Err(-1);
    }

    let mut val = holder.get_property(ctx, name)?;

    if let Some(mut o) = val.as_object().copied() {
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

            // build child path
            let key_str = if is_array {
                i.to_string()
            } else if prop.is_small_int() {
                prop.to_small_int().unwrap().to_string()
            } else {
                prop.descriptor(ctx.rt()).encode_utf8(false).into_owned()
            };
            let child_path = format!("{}/{}", path, key_str);

            let new_el = internalize_json_property(
                ctx,
                val,
                prop,
                reviver,
                source_map,
                original_values,
                &child_path,
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
    // 1. the source_map has an entry for this path (i.e. it was a primitive in the original JSON)
    // 2. the current value matches the original parse snapshot (i.e. it has NOT been forward-modified)
    if let Some(source_text) = source_map.get(path) {
        if original_values.get(path) == Some(&val) {
            let source_val = JSValue::from(ctx.create_string_from_str(source_text)?);
            // source: configurable | enumerable | writable
            let mut context_obj = context.as_object().copied().unwrap();
            context_obj.define_property_value(
                ctx,
                Atom::CONST_source,
                source_val,
                JS_PROP_C_W_E,
            )?;
        }
    }

    let name_val = name.to_value(ctx)?;
    reviver.call(ctx, holder, &[name_val, val, context])
}

fn js_json_parse(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let s = cc.arg(0).to_string(ctx)?;

    let mut obj = {
        let mut cp = ctx.ctx_ptr;
        let child_scope = cp.new_scope();
        let source = s.content().encode_utf8_bytes(true);

        let mut parser = JSParseState::new_for_json(child_scope, &source, "<input>");
        parser.ext_json = false; // flags & JS_PARSE_JSON_EXT != 0;

        let res = parser.json_parse();
        let (scope_back, _) = parser.finish(());
        // Promote GC nodes from the parsing child scope to the current scope
        let res = res.promote(&scope_back);
        res
    }?;

    // Extract source string for reviver context (json-parse-with-source)
    let source_bytes: String =
        String::from_utf8_lossy(s.content().encode_utf8_bytes(true).as_ref()).to_string();

    if cc.length() > 1 && cc.arg(1).is_callable() {
        let reviver = cc.arg(1);

        // Pre-build maps for the json-parse-with-source reviver context
        let source_map = build_source_map(&source_bytes);
        let original_values = capture_original_values(ctx, obj)?;

        let mut root: JSValue = ctx.create_base_object()?.into();
        root.define_property_value(ctx, Atom::CONST_empty_string, obj, JS_PROP_C_W_E)?;
        obj = internalize_json_property(
            ctx,
            root,
            Atom::CONST_empty_string,
            reviver,
            &source_map,
            &original_values,
            "",
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
    Ok(JSValue::Bool(is_raw))
}

fn js_json_rawJSON(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let str = cc.arg(0).to_string(ctx)?;
    let content = str.content().encode_utf8_bytes(true);
    let bytes = content.as_ref();

    // Spec: empty string → SyntaxError
    // Spec: first/last char is \t(0x09), \n(0x0A), \r(0x0D), space(0x20) → SyntaxError
    if bytes.is_empty() {
        ctx.throw_syntax_error("invalid rawJSON string");
        return Err(-1);
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
        ctx.throw_syntax_error("invalid rawJSON string");
        return Err(-1);
    }

    // Validate the string is parseable as JSON
    let result = {
        let mut cp = ctx.ctx_ptr;
        let child_scope = cp.new_scope();
        let mut parser = JSParseState::new_for_json(child_scope, bytes, "<input>");
        parser.ext_json = false;
        let res = parser.json_parse();
        let (scope_back, _) = parser.finish(());
        res.promote(&scope_back)
    };
    if result.is_err() {
        ctx.throw_syntax_error("invalid rawJSON string");
        return Err(-1);
    }

    // Create a RAWJSON object
    let mut obj = ctx
        .create_instance_with_payload(ObjectPayload::RAWJSON)
        .map_err(|_| -1)?;

    // Set the rawJSON property (enumerable, non-writable, non-configurable)
    obj.define_property_value(
        ctx,
        Atom::CONST_rawJSON,
        JSValue::from(str),
        JS_PROP_ENUMERABLE,
    )?;

    // Make the object non-extensible
    obj.prevent_extensions(ctx)?;

    Ok(JSValue::from(obj))
}

const js_json_funcs: &[FieldDef] = &[
    FieldDef::FUNC("parse", 2, js_json_parse, 0),
    FieldDef::FUNC("stringify", 3, js_json_stringify, 0),
    FieldDef::FUNC("isRawJSON", 1, js_json_isRawJSON, 0),
    FieldDef::FUNC("rawJSON", 1, js_json_rawJSON, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR("[Symbol.toStringTag]", "JSON", JS_PROP_CONFIGURABLE),
];

pub const js_json_obj: &[FieldDef] = &[FieldDef::OBJECT(
    "JSON",
    js_json_funcs,
    JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
)];
