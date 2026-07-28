// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

// Human-readable printing of `JSValue`

use std::fmt;

use zuqe::{
    Atom, JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_GETSET, JSValue,
    Object, ScopedContext,
};

/// Default recursion depth before collapsing to `[ClassName]`.
const DEFAULT_MAX_DEPTH: usize = 2;
/// Default max total chars shown for a string value (node/bun use 10000).
const DEFAULT_MAX_STRING_LENGTH: usize = 10000;
/// Default max items shown before truncating a container with `...`.
const DEFAULT_MAX_ITEM_COUNT: usize = 100;

#[derive(Clone, Copy)]
struct PrintOptions {
    max_depth: usize,
    max_string_length: usize,
    max_item_count: usize,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions {
            max_depth: DEFAULT_MAX_DEPTH,
            max_string_length: DEFAULT_MAX_STRING_LENGTH,
            max_item_count: DEFAULT_MAX_ITEM_COUNT,
        }
    }
}

fn print_float(v: f64, out: &mut dyn fmt::Write) -> fmt::Result {
    if v.is_nan() {
        return out.write_str("NaN");
    }
    if v.is_infinite() {
        return out.write_str(if v < 0.0 { "-Infinity" } else { "Infinity" });
    }
    if v == 0.0 {
        return out.write_str(if v.is_sign_negative() { "-0" } else { "0" });
    }
    write!(out, "{}", v)
}

/// Quoted/escaped string, truncated with `... N more characters`.
/// `max_len` is the total budget including the surrounding quotes.
fn print_js_string(s: &str, out: &mut dyn fmt::Write, max_len: usize) -> fmt::Result {
    let content_budget = max_len.saturating_sub(2);
    let chars: Vec<char> = s.chars().collect();
    let truncated = chars.len() > content_budget;
    let shown: String = chars.iter().take(content_budget).collect();
    out.write_char('"')?;
    for c in shown.chars() {
        match c {
            '"' => out.write_str("\\\"")?,
            '\\' => out.write_str("\\\\")?,
            '\n' => out.write_str("\\n")?,
            '\r' => out.write_str("\\r")?,
            '\t' => out.write_str("\\t")?,
            '\u{08}' => out.write_str("\\b")?,
            '\u{0c}' => out.write_str("\\f")?,
            c if (c as u32) < 0x20 => write!(out, "\\u{:04x}", c as u32)?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')?;
    if truncated {
        write!(out, "... {} more characters", chars.len() - content_budget)?;
    }
    Ok(())
}

/// Read a string property; on error clear the exception and return None.
fn get_str_opt(ctx: &mut ScopedContext, obj: &Object, prop: &str) -> Option<String> {
    match obj.get_property_str(ctx, prop) {
        Ok(v) => v
            .as_string(false)
            .map(|s| s.encode_utf8(false).into_owned()),
        Err(_) => {
            ctx.clear_exception();
            None
        }
    }
}

fn class_name_str(obj: &Object, ctx: &mut ScopedContext) -> String {
    obj.class_name().to_utf8(ctx.rt()).to_string()
}

/// Write a property value. For accessor properties show `[Getter]`/`
/// [Getter/Setter]` *without* calling the getter/setter (mirrors the default
/// `getters: false` of Node/Deno/Bun `inspect`).
fn write_prop_value(
    ctx: &mut ScopedContext,
    obj: &mut Object,
    atom: Atom,
    out: &mut dyn fmt::Write,
    opts: &PrintOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> fmt::Result {
    match obj.get_own_property(ctx, atom) {
        Ok(Some(desc)) if desc.flags & JS_PROP_GETSET != 0 => {
            if desc.setter.is_undefined() {
                out.write_str("[Getter]")
            } else {
                out.write_str("[Getter/Setter]")
            }
        }
        Ok(Some(desc)) => print_value(ctx, desc.value, out, opts, level + 1, stack),
        _ => {
            ctx.clear_exception();
            out.write_char('?')
        }
    }
}

/// Depth-collapse placeholder: `[Object]`/`[Array]`, or `[Object ClassName]`
/// for other classes (keeps the class name, like Node's tag style).
fn collapse_label(name: &str) -> String {
    match name {
        "Object" => "[Object]".to_string(),
        "Array" => "[Array]".to_string(),
        other => format!("[Object {}]", other),
    }
}

fn print_value(
    ctx: &mut ScopedContext,
    val: JSValue,
    out: &mut dyn fmt::Write,
    opts: &PrintOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> fmt::Result {
    if val.is_undefined() {
        return out.write_str("undefined");
    }
    if val.is_null() {
        return out.write_str("null");
    }
    if val.is_bool() {
        return out.write_str(if val.as_bool().unwrap() {
            "true"
        } else {
            "false"
        });
    }
    if val.is_int() {
        return write!(out, "{}", val.as_int().unwrap());
    }
    if val.is_float64() {
        return print_float(val.as_float().unwrap(), out);
    }
    if val.is_string() {
        let s = val.as_string(false).unwrap();
        let text = s.encode_utf8(false);
        return print_js_string(&text, out, opts.max_string_length);
    }
    if val.is_symbol() {
        return match val.as_symbol(false) {
            Some(d) => write!(out, "Symbol({})", d.encode_utf8(false)),
            None => out.write_str("Symbol()"),
        };
    }
    if let Some(bc) = val.as_bytecode() {
        let name = bc.func_name(ctx.rt());
        if name.is_empty() {
            return out.write_str("[Function: (anonymous)]");
        }
        return write!(out, "[Function: {}]", name);
    }
    if val.is_exception() {
        return out.write_str("[exception]");
    }
    if val.as_module().is_some() {
        return out.write_str("[module]");
    }

    if let Some(mut obj) = val.as_object() {
        if stack.iter().any(|o| *o == obj) {
            return out.write_str("[circular]");
        }
        if level > opts.max_depth {
            return write!(out, "{}", collapse_label(&class_name_str(&obj, ctx)));
        }
        stack.push(obj);
        let r = print_object(ctx, val, &mut obj, out, opts, level, stack);
        stack.pop();
        return r;
    }

    write!(out, "[unknown]")
}

/// Dispatch by payload type, mirroring QuickJS `js_print_object`.
fn print_object(
    ctx: &mut ScopedContext,
    val: JSValue,
    obj: &mut Object,
    out: &mut dyn fmt::Write,
    opts: &PrintOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> fmt::Result {
    // Array: [ e0, e1, ... ]  (sparse holes merge into <N empty items>, like Node)
    if let Some(arr) = obj.payload().array() {
        // Use the real array length, not just the dense buffer: a sparse array
        // keeps its elements in the shape, so arr.values() may be empty.
        let len = obj.get_length_prop(ctx).unwrap_or(arr.len() as i64) as usize;
        out.write_char('[')?;
        let n = len.min(opts.max_item_count);
        let mut i = 0;
        while i < n {
            if i > 0 {
                out.write_str(", ")?;
            }
            let idx = Atom::from_small_int(i as u32);
            // A hole is absent as a property even though length counts it.
            let is_hole = !matches!(obj.has_property(ctx, idx), Ok(true));
            if is_hole {
                // Merge consecutive holes: <1 empty item>, <2 empty items>, ...
                let mut j = i + 1;
                while j < n {
                    let idxj = Atom::from_small_int(j as u32);
                    if matches!(obj.has_property(ctx, idxj), Ok(true)) {
                        break;
                    }
                    j += 1;
                }
                let cnt = j - i;
                if cnt == 1 {
                    out.write_str("<empty>")?;
                } else {
                    write!(out, "<{} empty items>", cnt)?;
                }
                i = j;
            } else {
                match obj.get_property(ctx, idx) {
                    Ok(v) => print_value(ctx, v, out, opts, level + 1, stack)?,
                    Err(_) => {
                        ctx.clear_exception();
                        out.write_char('?')?;
                    }
                }
                i += 1;
            }
        }

        if len > opts.max_item_count {
            write!(out, ", ... {} more items", len - opts.max_item_count)?;
        }
        out.write_char(']')?;

        return Ok(());
    }

    // TypedArray: Uint8Array(N) [ e0, e1, ... ]
    if let Some(ta) = obj.payload().typed_array() {
        let count = ta.count();
        let name = class_name_str(obj, ctx);
        write!(out, "{}(", name)?;
        write!(out, "{}", count)?;
        out.write_str(") [")?;
        for i in 0..count {
            if i >= opts.max_item_count {
                write!(out, ", ... {} more items", count - opts.max_item_count)?;
                break;
            }
            if i > 0 {
                out.write_str(", ")?;
            }
            match obj.get_property_str(ctx, &i.to_string()) {
                Ok(v) => print_value(ctx, v, out, opts, level + 1, stack)?,
                Err(_) => {
                    ctx.clear_exception();
                    out.write_char('?')?;
                }
            }
        }
        out.write_str("]")?;
        return Ok(());
    }

    // Map/Set/WeakMap/WeakSet
    if let Some(ms) = obj.payload().mapset() {
        let name = class_name_str(obj, ctx);
        let len = ms.len();
        // Weak collections keep weak refs; just show the size.
        if ms.is_weak() {
            return write!(out, "{}(", name)
                .and_then(|_| write!(out, "{}", len))
                .and_then(|_| out.write_str(")"));
        }
        // Map/Set: expand live entries.
        write!(out, "{}(", name)?;
        write!(out, "{}", len)?;
        out.write_str(") {")?;
        let mut count = 0;
        for e in ms.iter() {
            if count >= opts.max_item_count {
                break;
            }
            if count > 0 {
                out.write_str(", ")?;
            }
            if name == "Map" {
                print_value(ctx, e.key(), out, opts, level + 1, stack)?;
                out.write_str(" => ")?;
                print_value(ctx, e.value(), out, opts, level + 1, stack)?;
            } else {
                // Set stores the element as the key.
                print_value(ctx, e.key(), out, opts, level + 1, stack)?;
            }
            count += 1;
        }
        if len > opts.max_item_count {
            write!(out, ", ... {} more items", len - opts.max_item_count)?;
        }
        out.write_char('}')?;
        return Ok(());
    }

    // RegExp: /source/flags
    if obj.payload().regexp().is_some() {
        let source = get_str_opt(ctx, obj, "source").unwrap_or_default();
        let flags = get_str_opt(ctx, obj, "flags").unwrap_or_default();
        out.write_char('/')?;
        out.write_str(&source)?;
        out.write_char('/')?;
        out.write_str(&flags)?;
        return Ok(());
    }

    // Date: ISO string via toISOString, fall back to class name.
    if obj.payload().datetime().is_some() {
        match obj.get_property_str(ctx, "toISOString") {
            Ok(f) if f.is_callable() => match f.call(ctx, val, &[]) {
                Ok(s) => {
                    if let Some(sr) = s.as_string(false) {
                        return out.write_str(&sr.encode_utf8(false));
                    }
                    return print_value(ctx, s, out, opts, level, stack);
                }
                Err(_) => ctx.clear_exception(),
            },
            _ => {}
        }
        let name = class_name_str(obj, ctx);
        return write!(out, "{}", name);
    }

    // Callable: [Function: name] / [Function: (anonymous)]
    if obj.is_callable() {
        return match get_str_opt(ctx, obj, "name") {
            Some(name) if !name.is_empty() => write!(out, "[Function: {}]", name),
            _ => out.write_str("[Function: (anonymous)]"),
        };
    }

    // Error (detected via `stack` property): Name: message\nstack
    if get_str_opt(ctx, obj, "stack").is_some() {
        let name = get_str_opt(ctx, obj, "name")
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| class_name_str(obj, ctx));
        let message = get_str_opt(ctx, obj, "message").unwrap_or_default();
        write!(out, "{}", name)?;
        if !message.is_empty() {
            write!(out, ": {}", message)?;
        }
        if let Some(stack_str) = get_str_opt(ctx, obj, "stack") {
            if !stack_str.is_empty() {
                write!(out, "\n{}", stack_str)?;
            }
        }
        return Ok(());
    }

    // Plain object: { key: value, ... } (incl. symbol keys; no getter invocation)
    match obj.get_own_property_names(
        ctx,
        JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK,
    ) {
        Ok(keys) => {
            out.write_char('{')?;
            let n = keys.len();
            for (i, pk) in keys.iter().enumerate() {
                if i >= opts.max_item_count {
                    write!(out, ", ... {} more items", n - opts.max_item_count)?;
                    break;
                }
                if i > 0 {
                    out.write_str(", ")?;
                }

                // 符号键渲染为 Symbol(name)，数字键不加引号，字符串键加引号
                // (与 Node util.inspect 一致：数字键裸写、字符串键带双引号)。
                if !pk.atom.is_small_int() {
                    if let Ok(kv) = pk.atom.to_value(ctx) {
                        if kv.is_symbol() {
                            match kv.as_symbol(false) {
                                Some(d) => write!(out, "Symbol({})", d.encode_utf8(false))?,
                                None => out.write_str("Symbol()")?,
                            }
                            out.write_str(": ")?;
                            write_prop_value(ctx, obj, pk.atom, out, opts, level, stack)?;
                            continue;
                        }
                    }
                    ctx.clear_exception();
                    // 字符串键：加双引号并转义
                    let key_str = pk.atom.to_utf8(ctx.rt());
                    print_js_string(key_str.as_ref(), out, opts.max_string_length)?;
                    out.write_str(": ")?;
                    write_prop_value(ctx, obj, pk.atom, out, opts, level, stack)?;
                    continue;
                }

                out.write_str(pk.atom.to_utf8(ctx.rt()).as_ref())?;
                out.write_str(": ")?;
                write_prop_value(ctx, obj, pk.atom, out, opts, level, stack)?;
            }
            out.write_char('}')?;
        }
        Err(_) => {
            ctx.clear_exception();
            out.write_char('{')?;
            out.write_str("...")?;
            out.write_char('}')?;
        }
    }
    Ok(())
}

/// Print `val` in human-readable form (QuickJS `JS_PrintValue` equivalent).
pub fn js_print_value(
    ctx: &mut ScopedContext,
    val: JSValue,
    out: &mut dyn fmt::Write,
) -> fmt::Result {
    let opts = PrintOptions::default();
    let mut stack: Vec<Object> = Vec::new();
    print_value(ctx, val, out, &opts, 0, &mut stack)
}

/// Convenience wrapper returning a `String`.
pub fn js_print_value_to_string(ctx: &mut ScopedContext, val: JSValue) -> String {
    let mut s = String::new();
    let _ = js_print_value(ctx, val, &mut s);
    s
}
