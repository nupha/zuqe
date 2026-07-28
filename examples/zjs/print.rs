// SPDX-License-Identifier: Apache-2.0
//! Human-readable printing of `JSValue`, modeled after QuickJS `JS_PrintValue`.
//! Side-effect free (no user toString/getter), except Date's `toISOString`.

use std::fmt;

use zuqe::{JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JSValue, Object, ScopedContext};

/// Defaults match QuickJS: depth 2, string 1000, items 100.
#[derive(Clone, Copy)]
pub struct PrintOptions {
    pub max_depth: usize,
    pub max_string_length: usize,
    pub max_item_count: usize,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions {
            max_depth: 2,
            max_string_length: 1000,
            max_item_count: 100,
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
fn print_js_string(s: &str, out: &mut dyn fmt::Write, max_len: usize) -> fmt::Result {
    let chars: Vec<char> = s.chars().collect();
    let truncated = chars.len() > max_len;
    let shown: String = chars.iter().take(max_len).collect();
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
        write!(out, "... {} more characters", chars.len() - max_len)?;
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
            return out.write_str("[Function (anonymous)]");
        }
        return write!(out, "[Function {}]", name);
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
            return write!(out, "[{}]", class_name_str(&obj, ctx));
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
    // Array: [ e0, e1, ... ]
    if let Some(arr) = obj.payload().array() {
        let values = arr.values();
        let len = values.len();
        out.write_char('[')?;
        for (i, item) in values.iter().enumerate() {
            if i >= opts.max_item_count {
                write!(out, ", ... {} more items", len - opts.max_item_count)?;
                break;
            }
            if i > 0 {
                out.write_str(", ")?;
            }
            print_value(ctx, *item, out, opts, level + 1, stack)?;
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

    // Map/Set: only size is printed (entries are not publicly iterable here).
    if let Some(ms) = obj.payload().mapset() {
        let len = ms.len();
        let name = class_name_str(obj, ctx);
        return write!(out, "{}(", name)
            .and_then(|_| write!(out, "{}", len))
            .and_then(|_| out.write_str(")"));
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

    // Callable: [Function name] / [Function (anonymous)]
    if obj.is_callable() {
        return match get_str_opt(ctx, obj, "name") {
            Some(name) if !name.is_empty() => write!(out, "[Function {}]", name),
            _ => out.write_str("[Function (anonymous)]"),
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

    // Plain object: { key: value, ... }
    match obj.get_own_property_names(ctx, JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK) {
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
                out.write_str(pk.atom.to_utf8(ctx.rt()).as_ref())?;
                out.write_str(": ")?;
                match obj.get_property(ctx, pk.atom) {
                    Ok(v) => print_value(ctx, v, out, opts, level + 1, stack)?,
                    Err(_) => {
                        ctx.clear_exception();
                        out.write_char('?')?;
                    }
                }
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
