// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

// A Node.js `util.inspect` compatible value formatter for zuqe.
//
// Implements the main behaviors of Node's `util.inspect(val, opts)`:
//   - depth / maxArrayLength / maxStringLength / breakLength
//   - compact (single-line vs multi-line)
//   - sorted (boolean)
//   - colors (ANSI)
//   - customInspect (Symbol.for('nodejs.util.inspect.custom'))
//   - getters (false | 'get' | 'set' | true)
//   - showHidden (non-enumerable properties)
//   - numericSeparator (thousands separators)
//   - string escaping / quoting (single quotes, Node style)
//   - Map/Set/WeakMap/WeakSet, RegExp, Date, Error, functions, symbols,
//     sparse arrays, circular references, depth collapse.

use zuqe::{
    Atom, JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_GETSET, JSValue,
    Object, ScopedContext,
};

/// When to evaluate accessors (getter/setter).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GettersOpt {
    /// Do not evaluate; show `[Getter]` / `[Setter]` / `[Getter/Setter]`.
    Off,
    /// Evaluate only getters; pure setters show `[Setter]`.
    Get,
    /// Evaluate only setters; pure getters show `[Getter]` (value unreadable, placeholder only).
    Set,
    /// Evaluate all accessible accessors.
    All,
}

/// Options for `util.inspect` (based on Node's defaults).
#[derive(Clone, Debug)]
pub struct InspectOptions {
    /// Recursion depth; `None` means unlimited (`Infinity`). Default `Some(2)`.
    pub depth: Option<usize>,
    /// Max entries to expand for arrays/Map/Set. Default `100`.
    pub max_array_length: usize,
    /// String truncation limit. Default `10000`.
    pub max_string_length: usize,
    /// Wrap to multiple lines when a single line exceeds this length and not compact. Default `80`.
    pub break_length: usize,
    /// Single-line (compact) or multi-line indented. Node default `true` (approx).
    pub compact: bool,
    /// Sort properties by key. Default `false`.
    pub sorted: bool,
    /// ANSI colors. Default `false`.
    pub colors: bool,
    /// Invoke the object's custom inspect method. Default `true`.
    pub custom_inspect: bool,
    /// Accessor evaluation policy. Default `Off`.
    pub getters: GettersOpt,
    /// Include non-enumerable properties. Default `false`.
    pub show_hidden: bool,
    /// Add `_` as thousands separator in numbers. Default `false`.
    pub numeric_separator: bool,
    /// Expand a Proxy's target/handler. Falls back to a marker if the engine does not expose them. Default `false`.
    pub show_proxy: bool,
}

impl Default for InspectOptions {
    fn default() -> Self {
        InspectOptions {
            depth: Some(2),
            max_array_length: 100,
            max_string_length: 10000,
            break_length: 80,
            compact: true,
            sorted: false,
            colors: false,
            custom_inspect: true,
            getters: GettersOpt::Off,
            show_hidden: false,
            numeric_separator: false,
            show_proxy: false,
        }
    }
}

const C_STRING: u8 = 32; // green
const C_NUMBER: u8 = 33; // yellow
const C_DATE: u8 = 35; // magenta
const C_REGEXP: u8 = 31; // red
const C_FUNC: u8 = 36; // cyan
const C_KEY: u8 = 36; // cyan
const C_SYMBOL: u8 = 32; // green
const C_SPECIAL: u8 = 90; // grey

fn colorize(s: &str, code: u8, opts: &InspectOptions) -> String {
    if !opts.colors {
        return s.to_string();
    }
    format!("\x1b[{}m{}\x1b[0m", code, s)
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn format_int(n: i64, opts: &InspectOptions) -> String {
    if !opts.numeric_separator {
        return n.to_string();
    }
    let neg = n < 0;
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (cnt, &b) in s.as_bytes().iter().rev().enumerate() {
        if cnt > 0 && cnt % 3 == 0 {
            out.push('_');
        }
        out.push(b as char);
    }
    let mut res: String = out.chars().rev().collect();
    if neg {
        res.insert(0, '-');
    }
    res
}

fn format_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_string()
    } else if f.is_infinite() {
        (if f < 0.0 { "-Infinity" } else { "Infinity" }).to_string()
    } else if f == 0.0 && f.is_sign_negative() {
        "-0".to_string()
    } else {
        format!("{}", f)
    }
}

// ---- string escaping / quoting (Node style) ----

/// Choose the quote: single by default; switch to double if the string contains more single quotes (minimize escaping).
fn choose_quote(s: &str) -> char {
    let sq = s.chars().filter(|&c| c == '\'').count();
    let dq = s.chars().filter(|&c| c == '"').count();
    if sq > dq { '"' } else { '\'' }
}

fn escape_char(ch: char, q: char, out: &mut String) {
    match ch {
        '\\' => out.push_str("\\\\"),
        '\'' if ch == q => out.push_str("\\'"),
        '"' if ch == q => out.push_str("\\\""),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{08}' => out.push_str("\\b"),
        '\u{0c}' => out.push_str("\\f"),
        '\u{0b}' => out.push_str("\\v"),
        c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
            out.push_str(&format!("\\x{:02x}", c as u32));
        }
        c => out.push(c),
    }
}

fn quote_string(s: &str, budget: usize, _sep: char) -> String {
    let len = s.chars().count();
    let q = choose_quote(s);
    let mut out = String::new();
    out.push(q);
    let limit = if len <= budget { len } else { budget };

    for (shown, ch) in s.chars().enumerate() {
        if shown >= limit {
            break;
        }
        escape_char(ch, q, &mut out);
    }
    if len > budget {
        out.push_str(&format!("... {} more characters", len - budget));
    }
    out.push(q);
    out
}

fn get_str_opt(ctx: &mut ScopedContext, obj: &Object, key: &str) -> Option<String> {
    obj.get_property_str(ctx, key)
        .ok()
        .and_then(|v| v.as_string(false))
        .map(|s| s.encode_utf8(false).into_owned())
}

/// Lay out a group of entries as `open ... close`, choosing single-line or multi-line based on compact / breakLength.
fn format_block(
    open: &str,
    entries: &[String],
    close: &str,
    opts: &InspectOptions,
    level: usize,
) -> String {
    if entries.is_empty() {
        return format!("{}{}", open, close);
    }
    let inner_len: usize =
        entries.iter().map(|e| e.len()).sum::<usize>() + entries.len().saturating_sub(1) * 2;
    let total = inner_len + open.len() + close.len();
    let has_newline = entries.iter().any(|e| e.contains('\n'));
    let fit = opts.compact || (!has_newline && total <= opts.break_length);
    if fit {
        let inner = entries.join(", ");
        format!("{}{}{}", open, inner, close)
    } else {
        let indent = "  ".repeat(level + 1);
        let close_indent = "  ".repeat(level);
        let mut s = String::new();
        s.push_str(open);
        for e in entries {
            s.push('\n');
            s.push_str(&indent);
            s.push_str(e);
            s.push(',');
        }
        s.push('\n');
        s.push_str(&close_indent);
        s.push_str(close);
        s
    }
}

// ---- key rendering ----

/// Returns (sort key, display string).
fn key_rendering(ctx: &mut ScopedContext, atom: &Atom, opts: &InspectOptions) -> (String, String) {
    if atom.is_small_int() {
        let s = atom.to_utf8(ctx.rt()).to_string();
        let n: i64 = s.parse().unwrap_or(0);
        let display = colorize(&s, C_KEY, opts);
        return (format!("0:{:020}", n), display);
    }
    if let Ok(kv) = atom.to_value(ctx)
        && kv.is_symbol()
    {
        let d = kv
            .as_symbol(false)
            .map(|x| x.encode_utf8(false).into_owned())
            .unwrap_or_default();
        let raw = format!("Symbol({})", d);
        let display = colorize(&raw, C_KEY, opts);
        return (format!("2:{}", d), display);
    }
    ctx.clear_exception();

    let s = atom.to_utf8(ctx.rt()).to_string();
    let q = quote_string(&s, opts.max_string_length, '\'');
    let display = colorize(&q, C_KEY, opts);
    (format!("1:{}", s), display)
}

// ---- custom inspect hook ----

fn try_custom_inspect(
    ctx: &mut ScopedContext,
    val: JSValue,
    obj: &mut Object,
    opts: &InspectOptions,
    level: usize,
) -> Option<String> {
    let syms = obj
        .get_own_property_names(ctx, JS_GPN_SYMBOL_MASK)
        .ok()?
        .to_vec();

    for pk in syms {
        let desc = obj.get_own_property(ctx, pk.atom).ok().flatten()?;
        if desc.flags & JS_PROP_GETSET != 0 {
            continue;
        }
        if !desc.value.is_callable() {
            continue;
        }
        let d = pk
            .atom
            .to_value(ctx)
            .ok()
            .and_then(|kv| kv.as_symbol(false))
            .map(|x| x.encode_utf8(false).into_owned())
            .unwrap_or_default();
        if d == "nodejs.util.inspect.custom" {
            let depth_arg = JSValue::from(opts.depth.map(|d| d as i32).unwrap_or(-1));
            match desc.value.call(
                ctx,
                val,
                &[depth_arg, JSValue::UNDEFINED, JSValue::UNDEFINED],
            ) {
                Ok(r) => {
                    if r.is_string() {
                        return Some(
                            r.as_string(false)
                                .map(|s| s.encode_utf8(false).into_owned())
                                .unwrap_or_default(),
                        );
                    }
                    // custom method returned a non-string: re-format with default rules (fresh stack to avoid false circular detection)
                    let mut fresh = Vec::new();
                    return Some(inspect(ctx, r, opts, level, &mut fresh));
                }
                Err(_) => {
                    ctx.clear_exception();
                    return None;
                }
            }
        }
    }
    None
}

// ---- accessor evaluation ----

fn invoke_getter(
    ctx: &mut ScopedContext,
    getter: JSValue,
    this: JSValue,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    match getter.call(ctx, this, &[]) {
        Ok(r) => inspect(ctx, r, opts, level + 1, stack),
        Err(_) => {
            ctx.clear_exception();
            colorize("[Getter]", C_SPECIAL, opts)
        }
    }
}

fn read_prop_value(
    ctx: &mut ScopedContext,
    this: JSValue,
    obj: &mut Object,
    atom: Atom,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    match obj.get_own_property(ctx, atom) {
        Ok(Some(desc)) if desc.flags & JS_PROP_GETSET != 0 => {
            let has_g = !desc.getter.is_undefined();
            let has_s = !desc.setter.is_undefined();
            match opts.getters {
                GettersOpt::Off => {
                    let label = if has_g && has_s {
                        "[Getter/Setter]"
                    } else if has_g {
                        "[Getter]"
                    } else {
                        "[Setter]"
                    };
                    colorize(label, C_SPECIAL, opts)
                }
                GettersOpt::Get => {
                    if has_g {
                        invoke_getter(ctx, desc.getter, this, opts, level, stack)
                    } else {
                        colorize("[Setter]", C_SPECIAL, opts)
                    }
                }
                GettersOpt::Set => {
                    if has_s {
                        colorize("[Getter]", C_SPECIAL, opts)
                    } else {
                        invoke_getter(ctx, desc.getter, this, opts, level, stack)
                    }
                }
                GettersOpt::All => {
                    if has_g {
                        invoke_getter(ctx, desc.getter, this, opts, level, stack)
                    } else {
                        colorize("[Setter]", C_SPECIAL, opts)
                    }
                }
            }
        }
        Ok(Some(desc)) => inspect(ctx, desc.value, opts, level + 1, stack),
        _ => {
            ctx.clear_exception();
            "?".to_string()
        }
    }
}

// ---- container formatting ----

fn format_object(
    ctx: &mut ScopedContext,
    val: JSValue,
    obj: &mut Object,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    let flags = if opts.show_hidden {
        JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK
    } else {
        JS_GPN_ENUM_ONLY | JS_GPN_STR_OR_INT_MASK | JS_GPN_SYMBOL_MASK
    };
    let keys = match obj.get_own_property_names(ctx, flags) {
        Ok(k) => k.to_vec(),
        Err(_) => {
            ctx.clear_exception();
            Vec::new()
        }
    };
    let mut entries: Vec<(String, String)> = Vec::new();
    stack.push(*obj);
    for pk in keys {
        let (sort_key, display_key) = key_rendering(ctx, &pk.atom, opts);
        let value = read_prop_value(ctx, val, obj, pk.atom, opts, level, stack);
        entries.push((sort_key, format!("{}: {}", display_key, value)));
    }
    stack.pop();

    if opts.sorted {
        entries.sort_by(|a, b| a.0.cmp(&b.0));
    }

    let displays: Vec<String> = entries.into_iter().map(|e| e.1).collect();
    format_block("{ ", &displays, " }", opts, level)
}

fn format_array(
    ctx: &mut ScopedContext,
    obj: &mut Object,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    let len = obj.get_length_prop(ctx).unwrap_or(0).max(0) as usize;
    let max = opts.max_array_length;

    let mut entries: Vec<String> = Vec::new();
    let mut empty_run: usize = 0;
    let mut i = 0usize;
    let mut truncated = false;

    stack.push(*obj);
    while i < len {
        if entries.len() >= max {
            truncated = true;
            break;
        }
        let atom = Atom::from_small_int(i as u32);
        let has = obj.has_property(ctx, atom).unwrap_or(false);
        if has {
            if empty_run > 0 {
                entries.push(format!("<{} empty item{}>", empty_run, plural(empty_run)));
                empty_run = 0;
            }
            let v = obj.get_property(ctx, atom).unwrap_or(JSValue::UNDEFINED);
            entries.push(inspect(ctx, v, opts, level + 1, stack));
        } else {
            empty_run += 1;
        }
        i += 1;
    }
    if empty_run > 0 {
        entries.push(format!("<{} empty item{}>", empty_run, plural(empty_run)));
    }
    if truncated {
        let more = len - i;
        if more > 0 {
            entries.push(format!("... {} more item{}", more, plural(more)));
        }
    }
    stack.pop();

    format_block("[", &entries, "]", opts, level)
}

fn format_typed_array(
    ctx: &mut ScopedContext,
    obj: &mut Object,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    let count = obj.payload_typed_array().map_or(0, |ta| ta.count());
    let max = opts.max_array_length;
    let mut entries: Vec<String> = Vec::new();
    let mut empty_run: usize = 0;
    let mut i = 0usize;
    let mut truncated = false;

    stack.push(*obj);

    while i < count {
        if entries.len() >= max {
            truncated = true;
            break;
        }
        let atom = Atom::from_small_int(i as u32);
        let has = obj.has_property(ctx, atom).unwrap_or(false);
        if has {
            if empty_run > 0 {
                entries.push(format!("<{} empty item{}>", empty_run, plural(empty_run)));
                empty_run = 0;
            }
            let v = obj.get_property(ctx, atom).unwrap_or(JSValue::UNDEFINED);
            entries.push(inspect(ctx, v, opts, level + 1, stack));
        } else {
            empty_run += 1;
        }
        i += 1;
    }
    if empty_run > 0 {
        entries.push(format!("<{} empty item{}>", empty_run, plural(empty_run)));
    }
    if truncated {
        let more = count - i;
        if more > 0 {
            entries.push(format!("... {} more item{}", more, plural(more)));
        }
    }
    stack.pop();

    let block = format_block("[", &entries, "]", opts, level);
    format!("{}({})", obj.class_name_str(), block)
}

fn format_mapset(
    ctx: &mut ScopedContext,
    obj: &mut Object,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    let cn = obj.class_name_str();
    let ms = match obj.payload_mapset() {
        Some(ms) => ms,
        None => return format!("{} {{}}", cn),
    };
    if ms.is_weak() {
        return format!("{} {{ <items unknown> }}", cn);
    }

    let is_set = cn == "Set";
    let mut entries: Vec<String> = Vec::new();
    stack.push(*obj);
    for e in ms.iter() {
        if is_set {
            // Set elements are stored in key() (value() is always undefined)
            entries.push(inspect(ctx, e.key(), opts, level + 1, stack));
        } else {
            let k = inspect(ctx, e.key(), opts, level + 1, stack);
            let v = inspect(ctx, e.value(), opts, level + 1, stack);
            entries.push(format!("{} => {}", k, v));
        }
    }
    stack.pop();

    let mut head = String::new();
    head.push_str(cn);
    head.push('(');
    head.push_str(&ms.len().to_string());
    head.push_str(") {");

    format_block(&head, &entries, "}", opts, level)
}

fn format_error(ctx: &mut ScopedContext, obj: &mut Object, opts: &InspectOptions) -> String {
    // Node: inspect(Error) prints err.stack (of the form "Name: message\n    at ...").
    let name = get_str_opt(ctx, obj, "name")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| obj.class_name_str().to_string());
    let msg = get_str_opt(ctx, obj, "message").unwrap_or_default();
    let mut s = if msg.is_empty() {
        name
    } else {
        format!("{}: {}", name, msg)
    };
    if let Some(stack) = get_str_opt(ctx, obj, "stack") {
        let stack = stack.trim_end();
        if !stack.is_empty() {
            s.push('\n');
            s.push_str(stack);
        }
    }
    colorize(&s, C_SPECIAL, opts)
}

fn collapse_tag(obj: &Object, opts: &InspectOptions) -> String {
    let cn = obj.class_name_str();
    let s = if cn == "Object" {
        "[Object]".to_string()
    } else if cn == "Array" {
        "[Array]".to_string()
    } else {
        format!("[{}]", cn)
    };
    colorize(&s, C_SPECIAL, opts)
}

fn inspect(
    ctx: &mut ScopedContext,
    val: JSValue,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    if val.is_undefined() {
        return colorize("undefined", C_SPECIAL, opts);
    }
    if val.is_null() {
        return colorize("null", C_SPECIAL, opts);
    }
    if val.is_bool() {
        return colorize(&val.as_bool().unwrap().to_string(), C_NUMBER, opts);
    }
    if val.is_int() {
        return colorize(
            &format_int(val.as_int().unwrap() as i64, opts),
            C_NUMBER,
            opts,
        );
    }
    if val.is_float64() {
        return colorize(&format_float(val.as_float().unwrap()), C_NUMBER, opts);
    }
    if val.is_string() {
        let s = val
            .as_string(false)
            .map(|x| x.encode_utf8(false).into_owned())
            .unwrap_or_default();
        return colorize(
            &quote_string(&s, opts.max_string_length, '\''),
            C_STRING,
            opts,
        );
    }
    if val.is_symbol() {
        let d = val
            .as_symbol(false)
            .map(|x| x.encode_utf8(false).into_owned())
            .unwrap_or_default();
        return colorize(&format!("Symbol({})", d), C_SYMBOL, opts);
    }
    if val.is_callable() {
        let name = val
            .as_bytecode()
            .map(|bc| bc.func_name(ctx.rt()))
            .or_else(|| val.as_object().and_then(|o| get_str_opt(ctx, &o, "name")));
        let label = match name {
            Some(n) if !n.is_empty() => format!("[Function: {}]", n),
            _ => "[Function (anonymous)]".to_string(),
        };
        return colorize(&label, C_FUNC, opts);
    }
    if val.as_module().is_some() {
        return colorize("[Module]", C_SPECIAL, opts);
    }

    let Some(mut obj) = val.as_object() else {
        return "[unknown]".to_string();
    };

    // circular reference
    if stack.contains(&obj) {
        return colorize("[Circular]", C_SPECIAL, opts);
    }
    // depth collapse
    if let Some(d) = opts.depth
        && level > d
    {
        return collapse_tag(&obj, opts);
    }
    // custom inspect hook
    if opts.custom_inspect
        && let Some(s) = try_custom_inspect(ctx, val, &mut obj, opts, level)
    {
        return s;
    }

    let cn = obj.class_name_str();

    // Proxy
    if cn == "Proxy" && !opts.show_proxy {
        return colorize("[Proxy]", C_SPECIAL, opts);
    }
    // engine does not directly expose target/handler: fall back to plain object traversal
    // TypedArray
    if obj.payload_typed_array().is_some() {
        return format_typed_array(ctx, &mut obj, opts, level, stack);
    }
    // Array
    if obj.payload_array().is_some() {
        return format_array(ctx, &mut obj, opts, level, stack);
    }
    // RegExp
    if cn == "RegExp"
        && let Ok(s) = val.to_string(ctx)
    {
        return colorize(&s.encode_utf8(false), C_REGEXP, opts);
    }
    // Date
    if cn == "Date"
        && let Ok(s) = val.to_string(ctx)
    {
        return colorize(&s.encode_utf8(false), C_DATE, opts);
    }
    // Error
    if cn.ends_with("Error") || cn == "Error" {
        return format_error(ctx, &mut obj, opts);
    }
    // Map / Set
    if obj.payload_mapset().is_some() {
        return format_mapset(ctx, &mut obj, opts, level, stack);
    }

    // plain object
    format_object(ctx, val, &mut obj, opts, level, stack)
}

/// Format an arbitrary JS value per Node `util.inspect` semantics.
pub fn js_inspect_value(ctx: &mut ScopedContext, val: JSValue, opts: &InspectOptions) -> String {
    let mut stack = Vec::new();
    inspect(ctx, val, opts, 0, &mut stack)
}
