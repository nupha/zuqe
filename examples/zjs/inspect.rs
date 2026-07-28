// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>
//
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

/// 何时求值访问器（getter/setter）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GettersOpt {
    /// 不求值，显示 `[Getter]` / `[Setter]` / `[Getter/Setter]`。
    Off,
    /// 仅求值 getter，纯 setter 显示 `[Setter]`。
    Get,
    /// 仅求值 setter，纯 getter 显示 `[Getter]`（无法读取值，仅占位）。
    Set,
    /// 求值所有可访问的访问器。
    All,
}

impl GettersOpt {
    pub fn from_bool(b: bool) -> Self {
        if b { GettersOpt::All } else { GettersOpt::Off }
    }
    pub fn from_str(s: &str) -> Self {
        match s {
            "get" => GettersOpt::Get,
            "set" => GettersOpt::Set,
            _ => GettersOpt::Off,
        }
    }
}

/// `util.inspect` 的选项集合（Node 默认值为基准）。
#[derive(Clone, Debug)]
pub struct InspectOptions {
    /// 递归深度，`None` 表示无限（`Infinity`）。默认 `Some(2)`。
    pub depth: Option<usize>,
    /// 数组/Map/Set 最多展开条目。默认 `100`。
    pub max_array_length: usize,
    /// 字符串截断上限。默认 `10000`。
    pub max_string_length: usize,
    /// 单行超过该长度且非 compact 时折行。默认 `80`。
    pub break_length: usize,
    /// 单行（紧凑）还是多行缩进。Node 默认 `true`（近似）。
    pub compact: bool,
    /// 属性按 key 排序。默认 `false`。
    pub sorted: bool,
    /// ANSI 上色。默认 `false`。
    pub colors: bool,
    /// 调用对象的自定义 inspect 方法。默认 `true`。
    pub custom_inspect: bool,
    /// 访问器求值策略。默认 `Off`。
    pub getters: GettersOpt,
    /// 包含不可枚举属性。默认 `false`。
    pub show_hidden: bool,
    /// 数字千位加 `_`。默认 `false`。
    pub numeric_separator: bool,
    /// 展开 Proxy 的 target/handler。引擎若不支持则退化为标记。默认 `false`。
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

// ---- ANSI 颜色（近似 Node 默认调色板）----
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

// ---- 数字格式化 ----

fn format_int(n: i64, opts: &InspectOptions) -> String {
    if !opts.numeric_separator {
        return n.to_string();
    }
    let neg = n < 0;
    let s = n.unsigned_abs().to_string();
    let mut out = String::new();
    let mut cnt = 0;
    for &b in s.as_bytes().iter().rev() {
        if cnt > 0 && cnt % 3 == 0 {
            out.push('_');
        }
        out.push(b as char);
        cnt += 1;
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

// ---- 字符串转义 / 加引号（Node 风格）----

/// 选择引号：单引号默认，若字符串包含更多单引号则改用双引号（最小化转义）。
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
    let mut shown = 0;
    for ch in s.chars() {
        if shown >= limit {
            break;
        }
        escape_char(ch, q, &mut out);
        shown += 1;
    }
    if len > budget {
        out.push_str(&format!("... {} more characters", len - budget));
    }
    out.push(q);
    out
}

// ---- 通用辅助 ----

fn is_callable_val(v: JSValue) -> bool {
    v.as_object().map(|o| o.is_callable()).unwrap_or(false)
}

fn get_str_opt(ctx: &mut ScopedContext, obj: &Object, key: &str) -> Option<String> {
    obj.get_property_str(ctx, key)
        .ok()
        .and_then(|v| v.as_string(false))
        .map(|s| s.encode_utf8(false).into_owned())
}

fn class_name_str(ctx: &mut ScopedContext, obj: &Object) -> String {
    obj.class_name().to_utf8(ctx.rt()).to_string()
}

/// 把一组条目排布成 `open ... close`，根据 compact / breakLength 决定单行或多行。
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

// ---- 键渲染 ----

/// 返回 (排序键, 显示串)。
fn key_rendering(ctx: &mut ScopedContext, atom: &Atom, opts: &InspectOptions) -> (String, String) {
    if atom.is_small_int() {
        let s = atom.to_utf8(ctx.rt()).to_string();
        let n: i64 = s.parse().unwrap_or(0);
        let display = colorize(&s, C_KEY, opts);
        return (format!("0:{:020}", n), display);
    }
    if let Ok(kv) = atom.to_value(ctx) {
        if kv.is_symbol() {
            let d = kv
                .as_symbol(false)
                .map(|x| x.encode_utf8(false).into_owned())
                .unwrap_or_default();
            let raw = format!("Symbol({})", d);
            let display = colorize(&raw, C_KEY, opts);
            return (format!("2:{}", d), display);
        }
    }
    ctx.clear_exception();
    let s = atom.to_utf8(ctx.rt()).to_string();
    let q = quote_string(&s, opts.max_string_length, '\'');
    let display = colorize(&q, C_KEY, opts);
    (format!("1:{}", s), display)
}

// ---- 自定义 inspect 钩子 ----

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
        if !is_callable_val(desc.value) {
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
                    // 自定义方法返回非字符串：用默认规则再格式化（独立栈避免误判循环）
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

// ---- 访问器求值 ----

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

// ---- 容器格式化 ----

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
    stack.push(obj.clone());
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
    stack.push(obj.clone());
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
    let cn = class_name_str(ctx, obj);
    let count = match obj.payload().typed_array() {
        Some(ta) => ta.count() as usize,
        None => 0,
    };
    let max = opts.max_array_length;
    let mut entries: Vec<String> = Vec::new();
    let mut empty_run: usize = 0;
    let mut i = 0usize;
    let mut truncated = false;
    stack.push(obj.clone());
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
    format!("{}(", cn).to_string() + &block
}

fn format_mapset(
    ctx: &mut ScopedContext,
    obj: &mut Object,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    let cn = class_name_str(ctx, obj);
    let ms = match obj.payload().mapset() {
        Some(ms) => ms,
        None => return format!("{} {{}}", cn),
    };
    if ms.is_weak() {
        return format!("{} {{ <items unknown> }}", cn);
    }
    let is_set = cn == "Set";
    let mut entries: Vec<String> = Vec::new();
    stack.push(obj.clone());
    for e in ms.iter() {
        if is_set {
            // Set 的元素存于 key()（value() 恒为 undefined）
            entries.push(inspect(ctx, e.key(), opts, level + 1, stack));
        } else {
            let k = inspect(ctx, e.key(), opts, level + 1, stack);
            let v = inspect(ctx, e.value(), opts, level + 1, stack);
            entries.push(format!("{} => {}", k, v));
        }
    }
    stack.pop();
    let mut head = String::new();
    head.push_str(&cn);
    head.push('(');
    head.push_str(&ms.len().to_string());
    head.push_str(") {");
    format_block(&head, &entries, "}", opts, level)
}

fn format_error(ctx: &mut ScopedContext, obj: &mut Object, opts: &InspectOptions) -> String {
    // Node: inspect(Error) 输出 err.stack（形如 "Name: message\n    at ..."）。
    let name = get_str_opt(ctx, obj, "name")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| class_name_str(ctx, obj));
    let msg = get_str_opt(ctx, obj, "message").unwrap_or_default();
    let mut s = if msg.is_empty() { name } else { format!("{}: {}", name, msg) };
    if let Some(stack) = get_str_opt(ctx, obj, "stack") {
        let stack = stack.trim_end();
        if !stack.is_empty() {
            s.push('\n');
            s.push_str(stack);
        }
    }
    colorize(&s, C_SPECIAL, opts)
}

fn collapse_tag(ctx: &mut ScopedContext, obj: &Object, opts: &InspectOptions) -> String {
    let cn = class_name_str(ctx, obj);
    let s = if cn == "Object" {
        "[Object]".to_string()
    } else if cn == "Array" {
        "[Array]".to_string()
    } else {
        format!("[{}]", cn)
    };
    colorize(&s, C_SPECIAL, opts)
}

// ---- 主入口 ----

fn inspect(
    ctx: &mut ScopedContext,
    val: JSValue,
    opts: &InspectOptions,
    level: usize,
    stack: &mut Vec<Object>,
) -> String {
    // 基础类型
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
    if is_callable_val(val) {
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

    // 循环引用
    if stack.iter().any(|o| *o == obj) {
        return colorize("[Circular]", C_SPECIAL, opts);
    }
    // 深度折叠
    if let Some(d) = opts.depth {
        if level > d {
            return collapse_tag(ctx, &obj, opts);
        }
    }
    // 自定义 inspect 钩子
    if opts.custom_inspect {
        if let Some(s) = try_custom_inspect(ctx, val, &mut obj, opts, level) {
            return s;
        }
    }

    let cn = class_name_str(ctx, &obj);

    // Proxy
    if cn == "Proxy" {
        if !opts.show_proxy {
            return colorize("[Proxy]", C_SPECIAL, opts);
        }
        // 引擎未直接暴露 target/handler：退化为普通对象遍历
    }
    // TypedArray
    if obj.payload().typed_array().is_some() {
        return format_typed_array(ctx, &mut obj, opts, level, stack);
    }
    // Array
    if obj.payload().array().is_some() {
        return format_array(ctx, &mut obj, opts, level, stack);
    }
    // RegExp
    if cn == "RegExp" {
        if let Ok(s) = val.to_string(ctx) {
            return colorize(&s.encode_utf8(false).into_owned(), C_REGEXP, opts);
        }
    }
    // Date
    if cn == "Date" {
        if let Ok(s) = val.to_string(ctx) {
            return colorize(&s.encode_utf8(false).into_owned(), C_DATE, opts);
        }
    }
    // Error
    if cn.ends_with("Error") || cn == "Error" {
        return format_error(ctx, &mut obj, opts);
    }
    // Map / Set
    if obj.payload().mapset().is_some() {
        return format_mapset(ctx, &mut obj, opts, level, stack);
    }
    // 普通对象
    format_object(ctx, val, &mut obj, opts, level, stack)
}

/// 按 Node `util.inspect` 规范格式化任意 JS 值。
pub fn js_inspect_value(ctx: &mut ScopedContext, val: JSValue, opts: &InspectOptions) -> String {
    let mut stack = Vec::new();
    inspect(ctx, val, opts, 0, &mut stack)
}

/// 使用默认选项（`InspectOptions::default()`）格式化。
pub fn js_inspect(ctx: &mut ScopedContext, val: JSValue) -> String {
    js_inspect_value(ctx, val, &InspectOptions::default())
}
