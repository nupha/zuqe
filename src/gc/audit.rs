// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Static audit of GC payload fields — the L2b layer of `memo/gc_bind_guard.md`.
//!
//! The module is a source-scanning test/tool utility, never called at runtime.
//!
//! The type system cannot stop someone from declaring `foo: JSValue` instead of
//! `WriteBarrier<JSValue>`. This module walks a source tree's GC payload root
//! set (`gc_type_register!` entries, every `def_native_object!` payload type and
//! every `PayloadPtr::new` payload type), expands local struct/enum field types
//! (and tuple field types, element-wise) recursively, and reports every GC-typed
//! field that is neither wrapped in `WriteBarrier`/`WriteBarrierVec`/
//! `WriteBarrierMap` nor allowlisted — with `file:line`.
//!
//! Three rules, all heuristic (deliberately no `syn`/`regex` dependency):
//!
//! 1. a GC leaf field of a payload type must be wrapped, unless allowlisted;
//! 2. a GC leaf field of a *container element* type (reached through `Vec`/
//!    `Box`/`[…]`) must be covered by an `impl GcBind for ElementType`, or be
//!    allowlisted — elements are bound by their container's `push`;
//! 3. `WriteBarrier::new` / `WriteBarrierVec::from_vec` /
//!    `WriteBarrierMap::new` may only construct (struct-literal field init,
//!    `let`); on the right of a field assignment or inside
//!    `mem::replace`/`mem::swap` it is a whole-field replacement that bypasses
//!    the barrier.
//!
//! # Using it from an embedding crate
//!
//! Everything ships in the library: your crate's `tests/gc_audit.rs` is one
//! macro call — no `Path`, no assert, no cfg — pointed at *your* source tree.
//! Nothing runs from zuqe's checkout, so it works with zuqe as a
//! registry/git/path dependency.
//!
//! ```ignore
//! // tests/gc_audit.rs — the whole file
//! zuqe::gc_audit_test!(30);
//! ```
//!
//! Exemptions live in `gc_audit_exemption.txt` next to your `Cargo.toml` — the
//! standard's single source of truth, read by both the macro and
//! `zuqe-gc-audit` ([`EXEMPTION_FILE`], [`read_exemptions`]). The two-argument
//! form `zuqe::gc_audit_test!(LIST, 30)` takes an inline slice as an escape
//! hatch, not as the standard layout.
//!
//! The standard `gc_audit_test!` macro expands to a `gc_audit_payload_fields`
//! test around [`test_failures`] (rules 1–3 plus the root-set floor); call the
//! individual functions directly if you need a different shape.
//!
//! To *discover* what needs auditing (from a tool or a test), [`gc_leaf_types`]
//! lists every struct/enum that directly declares a GC-leaf field with
//! `file:line` and its wrap/container status; [`field_verdicts`] adds the
//! per-field audit verdict; [`payload_root_types`] is the audit's root set.
//!
//! Unknown field types are skipped rather than guessed, and a payload type
//! unreachable from the root set is invisible here — this is a review-speed
//! bump, not a compiler.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// GC leaf bases: the value types plus gc-lite's strong/weak handles. A bare
/// `GcRef<T>`/`GcWeak<T>` payload field is a real (strong / weak) edge and must
/// be wrapped or allowlisted, never silently skipped. The object wrappers come
/// from [`leaf_table`] (zuqe's own wrapper list) plus whatever
/// `object_cast!`/`gcref_wrapper!`/`def_native_object!` sites exist in the
/// audited directory.
const GC_LEAF_BASES: &[&str] = &["JSValue", "Atom", "PropertyKey", "GcRef", "GcWeak"];

/// Violation messages shared by the root-set walk and the type report, so the
/// test output and the tool output always say the same thing.
const BARE_VIOLATION: &str = "bare GC field, not wrapped and not allowlisted";
const CONTAINER_VIOLATION: &str =
    "container element type has no `impl GcBind` and is not allowlisted";

/// Wrapper targets that no generated file lists: the hand-written
/// `gcref_wrapper!`/`object_cast!` sites of zuqe itself. A host payload may
/// hold any of them, and the host's sources never mention them, so they have
/// to travel with the library. `the_leaf_table_covers_every_wrapper_macro_site`
/// fails if a new macro site is added without extending this table.
const HAND_WRITTEN_WRAPPERS: &[&str] = &[
    "Object",
    "ContextPtr",
    "ModuleDef",
    "VarRef",
    "AsyncState",
    "Bytecode",
    "BCWrapper",
    "List",
    // Hand-written tuple structs that hold a GC handle but are not macro
    // wrappers. They are leaves for the audit, so a payload field of one of
    // these types is reported (and must be wrapped or allowlisted).
    "Shape",
    "PropValues",
    "StringRef",
    "BigIntRef",
    // Holds `Option<Bytecode>` callees through `FeedbackTable`; the single
    // write path (`record_call` → `note_call` bind) is allowlisted on the
    // `BytecodeImpl.feedback` field.
    "FeedbackSlot",
];

struct Field {
    name: String,
    ty: String,
    skip: bool,
    /// `Some(Variant)` for a field declared inside an enum variant. The audit
    /// key becomes `Type::Variant.field` (tuple variants: `Type::Variant.N`),
    /// so two variants may declare the same field name without sharing an
    /// exemption.
    variant: Option<String>,
}

struct Def {
    file: String,
    line: usize,
    fields: Vec<Field>,
}

/// All `.rs` files under `dir`, skipping the `foo.rs.N` editor backups.
fn source_files(dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(sub) = stack.pop() {
        for entry in std::fs::read_dir(&sub).expect("read source dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !name.ends_with(".rs") || name.contains(".rs.") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read source");
            out.push((rel(&path, dir), strip_cfg_test_modules(&text)));
        }
    }
    out.sort();
    out
}

fn rel(path: &Path, dir: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// Remove `#[cfg(test)] mod … { … }` blocks (and
/// `#[cfg(all(test, …))] mod …`) from the scanned text. Their payloads are
/// test fixtures declared inside a module, so they would otherwise show up as
/// "payload type not parsed" root misses. Production items are untouched.
fn strip_cfg_test_modules(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while let Some(pos) = text[i..].find("#[cfg(") {
        let start = i + pos;
        let line_start = text[..start].rfind('\n').map(|n| n + 1).unwrap_or(0);
        if text[line_start..start].contains("//") {
            out.push_str(&text[i..start + 6]);
            i = start + 6;
            continue;
        }
        let Some(close) = text[start..].find(']') else {
            break;
        };
        let attr_end = start + close + 1;
        if !text[start..attr_end].contains("test") {
            out.push_str(&text[i..attr_end]);
            i = attr_end;
            continue;
        }
        // `mod name { … }` (skip whitespace between the attribute and `mod`).
        let rest = &text[attr_end..];
        let lead = rest.len() - rest.trim_start().len();
        let after_ws = attr_end + lead;
        let body = &text[after_ws..];
        let is_mod = body.starts_with("mod ")
            || body.starts_with("pub mod ")
            || body.starts_with("pub(crate) mod ")
            || body.starts_with("pub(super) mod ");
        if !is_mod {
            out.push_str(&text[i..attr_end]);
            i = attr_end;
            continue;
        }
        let Some(brace_rel) = body.find('{') else {
            out.push_str(&text[i..attr_end]);
            i = attr_end;
            continue;
        };
        let brace = after_ws + brace_rel;
        let mut depth = 0i32;
        let mut in_str = false;
        let mut escaped = false;
        let mut end = None;
        for (k, ch) in text[brace..].char_indices() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_str = false;
                }
                continue;
            }
            match ch {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(brace + k);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else {
            break;
        };
        out.push_str(&text[i..start]);
        i = end + 1;
    }
    out.push_str(&text[i..]);
    out
}

/// Split on top-level commas, tracking (), [], {} and generic `<>` depth.
fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut prev = '\0';
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' | '<' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' => {
                depth -= 1;
                cur.push(ch);
            }
            '>' => {
                // `->` is a fn-return arrow, not a generic close: without this,
                // `Vec<fn() -> bool>` desyncs the depth and swallows every
                // field that follows it.
                if prev != '-' {
                    depth -= 1;
                }
                cur.push(ch);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
        prev = ch;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

fn strip_line_comments(s: &str) -> String {
    s.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_camel(s: &str) -> bool {
    s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn last_segment(s: &str) -> &str {
    s.rsplit("::").next().unwrap_or(s).trim()
}

/// Identifier tokens of an expression, in order.
fn idents(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(pos) = rest.find(|c: char| c.is_ascii_alphabetic() || c == '_') {
        rest = &rest[pos..];
        let end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

/// First identifier of `expr` that names a known local type: `crate::x::Foo::new`
/// → `Foo`, `JSValue::ZERO` → `JSValue`, `payload` → none (no type annotation
/// is resolved here, those cases are covered by the other two root sources).
fn type_from_expr(expr: &str, defs: &BTreeMap<String, Def>) -> Option<String> {
    idents(expr)
        .into_iter()
        .find(|id| is_camel(id) && defs.contains_key(*id))
        .map(|s| s.to_string())
}

/// Every `struct`/`enum` declared at column 0, with its field list.
fn parse_defs(files: &[(String, String)]) -> BTreeMap<String, Def> {
    let mut defs = BTreeMap::new();
    for (file, text) in files {
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            let rest = line
                .trim_start_matches("pub ")
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub(super) ");
            let kind = if rest.starts_with("struct ") {
                "struct"
            } else if rest.starts_with("enum ") {
                "enum"
            } else {
                i += 1;
                continue;
            };
            let name = rest[kind.len() + 1..]
                .split(|c: char| c.is_whitespace() || c == '(' || c == '{' || c == ';')
                .next()
                .unwrap_or("")
                .to_string();

            // Tuple/unit struct: `pub struct X(...);` — no named fields, but
            // it is a declared type: record it so a macro payload reference
            // resolves instead of showing up as a "payload type not parsed".
            if line.contains(';') && !line.contains('{') {
                if !name.is_empty() && !defs.contains_key(&name) {
                    defs.insert(
                        name,
                        Def {
                            file: file.clone(),
                            line: i + 1,
                            fields: Vec::new(),
                        },
                    );
                }
                i += 1;
                continue;
            }
            // Body: until the column-0 `}` that closes this item.
            let mut body = String::new();
            let mut header_open = line.contains('{');
            let mut j = i + 1;
            while j < lines.len() && j - i < 800 {
                let l = lines[j];
                if l.starts_with('}') {
                    break;
                }
                if !header_open {
                    if l.contains(';') && !l.contains('{') {
                        body.clear(); // multi-line tuple struct header
                        break;
                    }
                    if l.contains('{') {
                        header_open = true;
                    }
                }
                body.push_str(l);
                body.push('\n');
                j += 1;
            }
            if !name.is_empty() && !defs.contains_key(&name) {
                defs.insert(
                    name,
                    Def {
                        file: file.clone(),
                        line: i + 1,
                        fields: parse_fields(&body, kind == "enum"),
                    },
                );
            }
            i = j + 1;
        }
    }
    defs
}

/// Field list of a struct body, and of the enum variants inside it (both
/// struct-variants `V { f: T }` and tuple-variants `V(T)`).
///
/// Enum fields carry their variant, so the audit key is
/// `Type::Variant.field` (tuple variants: `Type::Variant.N`) instead of a bare
/// name every variant shares. A single-variant exemption can never silently
/// cover a same-named field of another variant.
fn parse_fields(body: &str, is_enum: bool) -> Vec<Field> {
    parse_fields_in(body, is_enum, None)
}

fn parse_fields_in(body: &str, is_enum: bool, variant: Option<&str>) -> Vec<Field> {
    let body = strip_line_comments(body);
    let mut out = Vec::new();
    for seg in split_top(&body) {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        // Drop leading attributes, remembering `#[gc(skip)]`.
        let mut skip = false;
        let mut rest = seg;
        loop {
            let t = rest.trim_start();
            if let Some(after) = t.strip_prefix("#[") {
                let end = after.find(']').map(|i| i + 1).unwrap_or(after.len());
                if after[..end].contains("gc(skip)") {
                    skip = true;
                }
                rest = &after[end..];
            } else {
                rest = t;
                break;
            }
        }
        let rest = rest
            .strip_prefix("pub(crate)")
            .or_else(|| rest.strip_prefix("pub(super)"))
            .or_else(|| rest.strip_prefix("pub"))
            .unwrap_or(rest)
            .trim_start();

        // `name: Type` — a struct field (or a field inside an enum
        // struct-variant; `variant` is already set in that case).
        if let Some(colon) = rest.find(':') {
            let head = rest[..colon].trim();
            if is_plain_ident(head) {
                out.push(Field {
                    name: head.to_string(),
                    ty: rest[colon + 1..]
                        .trim()
                        .trim_end_matches(',')
                        .trim()
                        .to_string(),
                    skip,
                    variant: variant.map(str::to_string),
                });
                continue;
            }
        }

        // Everything below is enum-variant syntax; a struct body has none.
        if !is_enum {
            continue;
        }

        // `Variant { ... }` — an enum struct-variant: recurse into its fields,
        // tagging each one with the variant name.
        if let Some(open) = rest.find('{') {
            let head = rest[..open].trim();
            if is_plain_ident(head) {
                let close = rest.rfind('}').unwrap_or(rest.len());
                out.extend(parse_fields_in(&rest[open + 1..close], false, Some(head)));
            }
            continue;
        }

        // `Variant(T, ...)` — an enum tuple-variant: each element is a field
        // named by its ordinal, so the audit key is `Type::Variant.N`.
        if let Some(open) = rest.find('(') {
            let head = rest[..open].trim();
            if is_plain_ident(head) {
                let close = rest.rfind(')').unwrap_or(rest.len());
                for (n, part) in split_top(&rest[open + 1..close]).into_iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    out.push(Field {
                        name: n.to_string(),
                        ty: part,
                        skip,
                        variant: Some(head.to_string()),
                    });
                }
            }
        }
    }
    out
}

/// Audit key of a field: `Type.field` for a struct field,
/// `Type::Variant.field` for an enum variant field.
fn field_key(type_name: &str, f: &Field) -> String {
    match &f.variant {
        Some(v) => format!("{type_name}::{v}.{}", f.name),
        None => format!("{type_name}.{}", f.name),
    }
}

fn is_plain_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && s != "dyn"
        && s != "impl"
}

/// Any GC leaf type name present as an identifier token inside `ty`.
/// Safety net: catches GC nodes hidden in wrappers `peel`/`base_name` do not
/// resolve (arbitrary nesting, unknown generics, shared wrappers we missed).
fn find_gc_leaf_in(ty: &str, leaves: &BTreeSet<String>) -> Option<String> {
    idents(ty)
        .into_iter()
        .find(|id| leaves.contains(*id))
        .map(|s| s.to_string())
}

/// Offsets of real call sites: the marker must be followed by optional
/// whitespace and the opening delimiter, so prose/comments mentioning the
/// macro are not parsed as code.
fn call_sites(text: &str, marker: &str, open: char) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(idx) = text[from..].find(marker) {
        let idx = from + idx;
        let line_start = text[..idx].rfind('\n').map(|n| n + 1).unwrap_or(0);
        if text[line_start..idx].contains("//") {
            from = idx + marker.len();
            continue;
        }
        let after = &text[idx + marker.len()..];
        if after.trim_start().starts_with(open) {
            out.push(idx);
        }
        from = idx + marker.len();
    }
    out
}

/// Root set plus the payload names the macros mention but `parse_defs` could
/// not resolve (nested/indented declaration, a type from another crate, or a
/// payload type the scanner has no rule for). A miss is a *failure*: silently
/// dropping the root would leave the whole payload unaudited.
struct RootScan {
    types: BTreeSet<String>,
    misses: BTreeSet<(String, &'static str)>,
}

/// First CamelCase identifier of `expr`, used to name a root miss when
/// `type_from_expr` cannot resolve a known local type.
fn first_camel_ident(expr: &str) -> Option<String> {
    idents(expr)
        .into_iter()
        .find(|id| is_camel(id))
        .map(str::to_string)
}

/// Root set: `gc_type_register!` entries, `def_native_object!` payload types
/// and `PayloadPtr::new` payload types. The last source is what catches enum
/// payloads (`ObjectPayload::LIST(...)`) that no macro declares.
fn root_scan(files: &[(String, String)], defs: &BTreeMap<String, Def>) -> RootScan {
    let mut scan = RootScan {
        types: BTreeSet::new(),
        misses: BTreeSet::new(),
    };
    let leaves = leaf_table();

    for (_, text) in files {
        for start in call_sites(text, "gc_type_register!", '{') {
            let open = text[start..].find('{').expect("checked by call_sites");
            let from = start + open + 1;
            let close = text[from..]
                .find('}')
                .map(|i| from + i)
                .unwrap_or(text.len());
            for entry in text[from..close].split(';') {
                let name = last_segment(entry.split(',').next().unwrap_or(""));
                if is_camel(name) && defs.contains_key(name) {
                    scan.types.insert(name.to_string());
                }
            }
        }

        for marker in ["def_native_object!", "PayloadPtr::new"] {
            for idx in call_sites(text, marker, '(') {
                let open = idx + marker.len() + 1;
                let mut depth = 1usize;
                let mut end = text.len();
                for (k, ch) in text[open..].char_indices() {
                    match ch {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = open + k;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                let body = strip_line_comments(&text[open..end]);
                let args = split_top(&body);
                let cand = if marker == "PayloadPtr::new" {
                    args.get(1).map(String::as_str)
                } else if args.first().map(String::as_str) == Some("builtin") {
                    // builtin forms carry `class_id:` before the payload
                    args.get(4).map(String::as_str)
                } else {
                    // native forms put the payload third; `()` = no payload
                    args.get(2).map(String::as_str)
                };
                let Some(cand) = cand else { continue };
                if cand.starts_with('$') || cand.starts_with('@') {
                    continue;
                }
                if let Some(t) = type_from_expr(cand, defs) {
                    scan.types.insert(t);
                } else if let Some(name) = first_camel_ident(cand) {
                    // A known GC wrapper (`JSValue`, `Object`, …) is not a
                    // payload declaration, so it is not a miss.
                    if !leaves.contains(name.as_str()) {
                        scan.misses.insert((name, marker));
                    }
                }
            }
        }
    }
    scan
}

fn root_set(files: &[(String, String)], defs: &BTreeMap<String, Def>) -> BTreeSet<String> {
    root_scan(files, defs).types
}

/// Wrapper type names declared by `object_cast!`/`gcref_wrapper!` sites inside
/// the audited directory (zuqe's own sources; a host has none of these).
fn macro_wrapper_types(files: &[(String, String)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, text) in files {
        for marker in ["object_cast!", "gcref_wrapper!"] {
            for (idx, _) in text.match_indices(marker) {
                let rest = text[idx + marker.len()..].trim_start();
                let rest = rest.strip_prefix('(').unwrap_or(rest).trim_start();
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if is_camel(&name) {
                    out.insert(name);
                }
            }
        }
    }
    out
}

/// zuqe's own wrapper table: the generated narrow types plus the hand-written
/// macro targets. Host audits need it because their sources never mention
/// zuqe's wrappers, yet their payloads use `Object`/`Promise`/… every day.
fn leaf_table() -> BTreeSet<&'static str> {
    GC_LEAF_BASES
        .iter()
        .copied()
        .chain(crate::object::NARROW_WRAPPER_TYPES.iter().copied())
        .chain(HAND_WRITTEN_WRAPPERS.iter().copied())
        .collect()
}

/// Native class wrappers declared by `def_native_object!` in the audited
/// directory: a host's own `pub struct Foo(Object)` instances are GC leaves when
/// another payload stores one.
fn native_class_types(files: &[(String, String)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, text) in files {
        for idx in call_sites(text, "def_native_object!", '(') {
            let open = idx + "def_native_object!".len() + 1;
            let mut depth = 1usize;
            let mut end = text.len();
            for (k, ch) in text[open..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let args = split_top(&strip_line_comments(&text[open..end]));
            // native: (name, ty, payload, …) — builtin: (builtin, name, ty, …)
            let slot = if args.first().map(String::as_str) == Some("builtin") {
                2
            } else {
                1
            };
            let Some(ty) = args.get(slot) else { continue };
            if ty.starts_with('$') || ty.starts_with('@') {
                continue;
            }
            let name = last_segment(ty);
            if is_camel(name) {
                out.insert(name.to_string());
            }
        }
    }
    out
}

/// Every type the audit treats as a GC leaf in `files`' directory.
fn leaf_types(files: &[(String, String)]) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = leaf_table().into_iter().map(String::from).collect();
    out.extend(macro_wrapper_types(files));
    out.extend(native_class_types(files));
    out
}

/// Types with a hand-written `impl GcBind for X`: element types whose fields
/// their container's `push` binds.
fn gc_bind_impls(files: &[(String, String)]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for (_, text) in files {
        for (idx, _) in text.match_indices("impl GcBind for ") {
            let rest = &text[idx + "impl GcBind for ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if is_camel(&name) {
                out.insert(name);
            }
        }
    }
    out
}

/// Peel guard/container layers off a field type, including arrays:
/// `[Vec<T>; 2]` → `T`. Returns `(inner, wrapped_by_guard, went_through_container,
/// shared_wrapper_seen)`. `shared_wrapper_seen` is set when a shared-ownership
/// wrapper (`Rc`/`Arc`/`Weak`) was peeled through — such wrappers let a GC ref
/// escape the traced/barriered slot, so the field stays suspicious even if it
/// would otherwise look guarded.
fn peel(mut ty: &str) -> (&str, bool, bool, bool) {
    let mut wrapped = false;
    let mut in_container = false;
    let mut shared = false;
    loop {
        let t = ty.trim();
        if let Some(inner) = strip_layer(t, &["WriteBarrierVec", "WriteBarrierMap", "WriteBarrier"])
        {
            wrapped = true;
            // a guarded Vec/Map is still a container: its element type's fields
            // are covered by the element's `GcBind` impl, not by the guard
            in_container |= t.starts_with("WriteBarrierVec") || t.starts_with("WriteBarrierMap");
            ty = inner;
            continue;
        }
        let mut moved = false;
        for layer in [
            "Option",
            "RefCell",
            "Cell",
            "UnsafeCell",
            // A raw pointer to a GC value (e.g. `VarRefImpl::Bind::pvalue`)
            // still names an edge: peel it so the pointee type is audited.
            "NonNull",
            "Box",
            "Vec",
            "VecDeque",
            "SmallVec",
            "HashList",
            "HashMap",
            "BTreeMap",
            "IndexMap",
            // Shared/sync wrappers: not GC-aware; peel through so a GC leaf hidden
            // inside (e.g. `Rc<RefCell<JSValue>>`, `Arc<JSValue>`) is still seen.
            "Rc",
            "Arc",
            "Weak",
            "Mutex",
            "RwLock",
            "Pin",
            "ManuallyDrop",
            "MaybeUninit",
            "Cow",
        ] {
            if let Some(inner) = strip_layer(t, &[layer]) {
                in_container |= matches!(
                    layer,
                    "Box"
                        | "Vec"
                        | "VecDeque"
                        | "SmallVec"
                        | "HashList"
                        | "HashMap"
                        | "BTreeMap"
                        | "IndexMap"
                );
                shared |= matches!(layer, "Rc" | "Arc" | "Weak");
                ty = inner;
                moved = true;
                break;
            }
        }
        if moved {
            continue;
        }
        if let Some(elem) = array_elem(t) {
            in_container = true;
            ty = elem;
            continue;
        }
        break;
    }
    (ty.trim(), wrapped, in_container, shared)
}

fn strip_layer<'a>(ty: &'a str, names: &[&str]) -> Option<&'a str> {
    for name in names {
        if let Some(rest) = ty.strip_prefix(name) {
            let rest = rest.trim_start();
            if let Some(rest) = rest.strip_prefix('<') {
                let close = match_depth_close(rest)?;
                if rest[close..].trim_start().starts_with('>') {
                    return Some(rest[..close].trim());
                }
            }
        }
    }
    None
}

/// Index of the `>` that closes the `<` at the start of `s`.
fn match_depth_close(s: &str) -> Option<usize> {
    let mut depth = 1i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `[T; N]` / `[T]` / `&[T]` → `T`; not an array → `None` (keep as is).
fn array_elem(ty: &str) -> Option<&str> {
    let t = ty
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();
    let inner = t.strip_prefix('[')?;
    if let Some(semi) = inner.rfind(';') {
        let end = inner[..semi].rfind(']').unwrap_or(semi);
        Some(inner[..end].trim())
    } else {
        inner.strip_suffix(']').map(|s| s.trim())
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    type_name: &str,
    def: &Def,
    through_container: bool,
    defs: &BTreeMap<String, Def>,
    leaves: &BTreeSet<String>,
    gc_bind: &BTreeSet<String>,
    allow: &BTreeMap<&str, &str>,
    allow_used: &mut BTreeSet<String>,
    violations: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    verdicts: &mut BTreeMap<String, FieldVerdict>,
    warnings: &mut BTreeSet<String>,
) {
    if !seen.insert(type_name.to_string()) {
        return;
    }
    for f in &def.fields {
        if f.skip {
            continue;
        }
        let (inner, wrapped, container, shared) = peel(&f.ty);
        let through_container = through_container || container;
        // A tuple field type is checked element-wise — `(JSValue, u32)` must
        // not hide its GC elements. The allowlist key stays `Type.field`, so
        // one exemption covers the whole tuple.
        for ty in expand_tuple(inner) {
            let Some(base) = base_name(&ty) else {
                continue;
            };
            let is_leaf = leaves.contains(&base);

            if is_leaf {
                let key = field_key(type_name, f);
                let is_exempt = allow.contains_key(key.as_str());
                // Natural verdict (without exemption): shared wrappers (Rc/Arc/Weak)
                // let the GC ref escape — always suspicious, 一切皆可疑.
                if shared {
                    if is_exempt {
                        allow_used.insert(key.clone());
                        // An exemption is a global fact about the key: a later
                        // (or earlier) naturally-safe visit through a container
                        // must not hide it and make the entry look stale.
                        verdicts.insert(key, FieldVerdict::Allowlisted);
                    } else {
                        verdicts.insert(
                            key,
                            FieldVerdict::Violation(
                                "GC leaf type behind Rc/Arc/Weak shared wrapper; can escape traced/barriered slot",
                            ),
                        );
                        violations.push(format!(
                            "  {}:{}: {type_name}.{}: `{}` — GC leaf behind Rc/Arc/Weak shared wrapper; can escape traced/barriered slot (not GC-safe)",
                            def.file, def.line, f.name, f.ty,
                        ));
                    }
                    continue;
                }
                if wrapped {
                    // Naturally safe: do not consume an exemption so stale entries
                    // (wrapped but still listed) are detected.
                    verdicts.entry(key).or_insert(FieldVerdict::Pass);
                    continue;
                }
                if through_container && gc_bind.contains(&base) {
                    verdicts.entry(key).or_insert(FieldVerdict::Pass);
                    continue;
                }
                if is_exempt {
                    allow_used.insert(key.clone());
                    verdicts.insert(key, FieldVerdict::Allowlisted);
                } else {
                    let reason = if through_container {
                        CONTAINER_VIOLATION
                    } else {
                        BARE_VIOLATION
                    };
                    verdicts.insert(key.clone(), FieldVerdict::Violation(reason));
                    violations.push(format!(
                        "  {}:{}: {key}: `{}` — {reason}",
                        def.file, def.line, f.ty,
                    ));
                }
                continue;
            }

            // Not a leaf: a local struct/enum expands, anything else is skipped.
            if is_camel(&base) && defs.contains_key(&base) {
                walk(
                    &base,
                    defs.get(&base).expect("just checked"),
                    through_container,
                    defs,
                    leaves,
                    gc_bind,
                    allow,
                    allow_used,
                    violations,
                    seen,
                    verdicts,
                    warnings,
                );
            } else if let Some(found) = find_gc_leaf_in(&ty, leaves) {
                // Safety net: a GC leaf type is present but wrapped/structured in a
                // way the normal path cannot resolve (unknown wrapper, custom generic,
                // multi-arg position, shared wrapper that escaped normalization, etc.).
                // Treat as unsafe unless explicitly allowlisted — "anything is possible".
                let key = field_key(type_name, f);
                if allow.contains_key(key.as_str()) {
                    allow_used.insert(key.clone());
                    verdicts.insert(key, FieldVerdict::Allowlisted);
                } else {
                    let reason = if shared {
                        "GC leaf type behind Rc/Arc/Weak shared wrapper; can escape traced/barriered slot"
                    } else {
                        "GC leaf type hidden in unresolvable wrapper; not verified safe"
                    };
                    verdicts.insert(key.clone(), FieldVerdict::Violation(reason));
                    violations.push(format!(
                        "  {}:{}: {key}: `{}` — contains GC leaf `{found}` inside unresolvable wrapper; not verified safe (wrap with WriteBarrier*/allowlist with justification)",
                        def.file, def.line, f.ty,
                    ));
                }
            } else if warns_unknown(&ty, &base) {
                // Cross-crate wrapper, alias, or a GC node we have no rule for.
                // Non-failing: String/u32/Instant noise must not break a build,
                // but a silent skip must not hide a real edge either.
                warnings.insert(format!(
                    "  ⚠ {}:{}: {type_name}.{}: unknown type `{}` — not in local defs or leaf table; add it to the leaf table if it is a GC node, otherwise this diagnostic is informational",
                    def.file, def.line, f.name, base,
                ));
            }
        }
    }
}

/// Standard-library / container names that are never a GC node on their own:
/// warning about them would bury the real cross-crate wrapper diagnostics.
const NON_GC_TYPE_NAMES: &[&str] = &[
    "String",
    "Path",
    "PathBuf",
    "Instant",
    "Duration",
    "SystemTime",
    "OsString",
    "OsStr",
    "CString",
    "CStr",
    "Vec",
    "Option",
    "Result",
    "Box",
    "Rc",
    "Arc",
    "Weak",
    "RefCell",
    "Cell",
    "Mutex",
    "RwLock",
    "PhantomData",
    "HashMap",
    "BTreeMap",
    "HashSet",
    "BTreeSet",
    "VecDeque",
    "SmallVec",
    "Pin",
    "ManuallyDrop",
    "MaybeUninit",
    "Cow",
    "NonNull",
    "Range",
    "Bound",
];

/// Whether an unresolved field type deserves the non-failing `⚠ unknown type`
/// diagnostic: CamelCase (a type, not a primitive/generic `T`), not a
/// multi-arg peel remainder (`Atom, JSValue`), not a known non-GC std type.
fn warns_unknown(ty: &str, base: &str) -> bool {
    is_camel(base) && base.len() > 1 && !ty.contains(',') && !NON_GC_TYPE_NAMES.contains(&base)
}

/// Flatten a tuple field type into its elements: `(JSValue, u32)` →
/// `["JSValue", "u32"]`, nested tuples recursively; anything else (including
/// `fn(..) -> T`, whose parens are not a tuple type) returns as-is.
fn expand_tuple(ty: &str) -> Vec<String> {
    let t = ty.trim();
    if t.starts_with('(') && t.ends_with(')') {
        split_top(&t[1..t.len() - 1])
            .into_iter()
            .flat_map(|e| expand_tuple(&e))
            .collect()
    } else {
        vec![t.to_string()]
    }
}

/// Outermost type name of a field type, path-qualified or not:
/// `Option<Vec<T>>` → `Option`, `zuqe::Object` → `Object`,
/// `crate::x::Foo<Bar>` → `Foo`.
fn base_name(ty: &str) -> Option<String> {
    let mut t = ty.trim();
    loop {
        let next = t
            .trim_start_matches('&')
            .trim_start_matches('*')
            .trim_start_matches("mut ")
            .trim_start_matches("const ")
            .trim();
        if next.len() == t.len() {
            break;
        }
        t = next;
    }
    if t.is_empty() || t.starts_with("dyn ") || t.starts_with("fn(") || t.starts_with('(') {
        return None;
    }
    let head = t.split('<').next().unwrap_or(t);
    let head = head.rsplit("::").next().unwrap_or(head);
    let end = head
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(head.len());
    let head = &head[..end];
    if head.is_empty()
        || !head
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
    {
        return None;
    }
    Some(head.to_string())
}

/// The GC payload root set of `dir`, sorted. Exposed so a caller can assert
/// that its own payload types were actually found — a scan that matched nothing
/// would otherwise pass vacuously.
pub fn payload_root_types(dir: &Path) -> Vec<String> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    root_set(&files, &defs).into_iter().collect()
}

/// The standard exemption file name: kept next to `Cargo.toml` (a sibling of
/// `src/`), read by both `zuqe::gc_audit_test!` and `zuqe-gc-audit`.
pub const EXEMPTION_FILE: &str = "gc_audit_exemption.txt";

/// Read an exemption file: one `Type.field — reason` entry per line, with
/// blank lines and `#` comments skipped. A missing file yields an empty list
/// (a crate with no exemptions needs no file).
pub fn read_exemptions(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// A struct/enum definition in the scanned tree that directly declares at
/// least one GC-leaf field — the candidate set an audit has to cover.
pub struct GcLeafType {
    pub name: String,
    pub file: String,
    pub line: usize,
    /// Only the GC-leaf fields; plain fields of the same definition are not
    /// listed.
    pub fields: Vec<GcLeafField>,
}

/// One GC-leaf field of a [`GcLeafType`].
pub struct GcLeafField {
    /// Field name; enum tuple variants use the ordinal (`0`, `1`, …).
    pub name: String,
    /// `Some(Variant)` for a field declared inside an enum variant.
    pub variant: Option<String>,
    /// The declared type, as written in the source.
    pub ty: String,
    /// The field sits behind `WriteBarrier`/`WriteBarrierVec`/`WriteBarrierMap`
    /// (rule 1 holds).
    pub wrapped: bool,
    /// The leaf was reached through a container (`Vec`/`Box`/…): rule 2
    /// applies instead — the element type needs an `impl GcBind`.
    pub container: bool,
}

impl GcLeafField {
    /// The audit key: `Type.field` for a struct field,
    /// `Type::Variant.field` for an enum variant field (tuple:
    /// `Type::Variant.N`). This is the key an exemption must use.
    pub fn key(&self, type_name: &str) -> String {
        match &self.variant {
            Some(v) => format!("{type_name}::{v}.{}", self.name),
            None => format!("{type_name}.{}", self.name),
        }
    }
}

/// Scan `dir` for struct/enum definitions that directly declare a GC-leaf
/// field, sorted by type name. The leaf set is the same one
/// [`gc_field_violations`] uses, so a candidate found here and reported clean
/// there refers to the same field.
///
/// Only *direct* fields are matched: a definition whose GC-ness comes only
/// through a nested local type is not listed — that is what the root-set walk
/// in [`gc_field_violations`] covers. This is the discovery half; use
/// [`field_verdicts`] for the audit verdict per field.
pub fn gc_leaf_types(dir: &Path) -> Vec<GcLeafType> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    let leaves = leaf_types(&files);

    let mut out = Vec::new();
    for (name, def) in &defs {
        let mut fields = Vec::new();
        for f in &def.fields {
            if f.skip {
                continue;
            }
            let (inner, wrapped, container, _shared) = peel(&f.ty);
            let hit = expand_tuple(inner)
                .iter()
                .any(|ty| base_name(ty).is_some_and(|b| leaves.contains(&b)));
            if hit {
                fields.push(GcLeafField {
                    name: f.name.clone(),
                    variant: f.variant.clone(),
                    ty: f.ty.clone(),
                    wrapped,
                    container,
                });
            }
        }
        if !fields.is_empty() {
            out.push(GcLeafType {
                name: name.clone(),
                file: def.file.clone(),
                line: def.line,
                fields,
            });
        }
    }
    out
}

/// Audit verdict for one GC-leaf field, as the root-set walk decides it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldVerdict {
    /// Wrapped, or a container element type covered by `impl GcBind`.
    Pass,
    /// Bare, but its `Type.field` key is in the allowlist.
    Allowlisted,
    /// Fails the audit; the payload is the rule that failed.
    Violation(&'static str),
}

/// Per-field verdicts for the whole payload root set, keyed by `Type.field`
/// (fields of definitions the audit does not reach are absent). This is the
/// report form of [`gc_field_violations`] — same walk, same rules — so a
/// field reported clean there is `Pass`/`Allowlisted` here.
pub fn field_verdicts(dir: &Path, allowlist: &[&str]) -> BTreeMap<String, FieldVerdict> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    let roots = root_set(&files, &defs);
    let leaves = leaf_types(&files);
    let mut gc_bind = gc_bind_impls(&files);
    gc_bind.extend(macro_wrapper_types(&files));
    let allow: BTreeMap<&str, &str> = allowlist.iter().map(|e| (allowlist_key(e), *e)).collect();

    let mut out = BTreeMap::new();
    for root in &roots {
        if let Some(def) = defs.get(root) {
            walk(
                root,
                def,
                false,
                &defs,
                &leaves,
                &gc_bind,
                &allow,
                &mut BTreeSet::new(),
                &mut Vec::new(),
                &mut BTreeSet::new(),
                &mut out,
                &mut BTreeSet::new(),
            );
        }
    }
    out
}

/// Rule 1 + rule 2 violations, plus stale allowlist entries (an entry whose
/// field disappeared, was renamed or got wrapped — keeps the list honest).
///
/// Allowlist entries are `Type.field — reason`; the part before `—` is the key,
/// the whole entry is echoed back in the message.
pub fn gc_field_violations(dir: &Path, allowlist: &[&str]) -> Vec<String> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    let roots = root_set(&files, &defs);
    let leaves = leaf_types(&files);
    let mut gc_bind = gc_bind_impls(&files);
    // `gcref_wrapper!` / `object_cast!` expand a `GcBind` impl at every call
    // site, so the source text only has the macro invocation, not the impl.
    // Without this, `Vec<VarRef>` and friends are false positives.
    gc_bind.extend(macro_wrapper_types(&files));

    let allow: BTreeMap<&str, &str> = allowlist.iter().map(|e| (allowlist_key(e), *e)).collect();
    let mut allow_used = BTreeSet::new();
    let mut out = Vec::new();

    for root in &roots {
        let Some(def) = defs.get(root) else {
            continue;
        };
        walk(
            root,
            def,
            false,
            &defs,
            &leaves,
            &gc_bind,
            &allow,
            &mut allow_used,
            &mut out,
            &mut BTreeSet::new(),
            &mut BTreeMap::new(),
            &mut BTreeSet::new(),
        );
    }

    let stale: Vec<&str> = allowlist
        .iter()
        .copied()
        .filter(|e| !allow_used.contains(allowlist_key(e)))
        .collect();
    if !stale.is_empty() {
        out.push(format!(
            "stale allowlist entries (field gone, renamed or now wrapped):\n  {}",
            stale.join("\n  ")
        ));
    }
    out
}

/// Types annotated with `unsafe_empty_gc_trace!` assert they contain no GC
/// nodes. This check keeps that assertion honest: any reachable GC leaf in
/// their fields — wrapped, shared, or behind a container — fails the audit.
///
/// Unlike rules 1–2 there is no wrapping or allowlist that makes a GC field
/// safe here: the type is not traced at all, so even a `WriteBarrier` field
/// would never be visited. Unknown cross-crate field types are skipped, the
/// same way the rest of the audit treats them.
pub fn empty_trace_violations(dir: &Path) -> Vec<String> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    let leaves = leaf_types(&files);
    let mut out = Vec::new();

    for (_, text) in &files {
        for idx in call_sites(text, "unsafe_empty_gc_trace!", '(') {
            let open = idx + "unsafe_empty_gc_trace!".len() + 1;
            let mut depth = 1usize;
            let mut end = text.len();
            for (k, ch) in text[open..].char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = open + k;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            for arg in split_top(&text[open..end]) {
                let Some(base) = base_name(&arg) else {
                    continue;
                };
                let Some(def) = defs.get(&base) else {
                    continue;
                };
                let mut seen = BTreeSet::new();
                check_empty_trace(&base, def, &defs, &leaves, &mut seen, &mut out);
            }
        }
    }
    out
}

fn check_empty_trace(
    type_name: &str,
    def: &Def,
    defs: &BTreeMap<String, Def>,
    leaves: &BTreeSet<String>,
    seen: &mut BTreeSet<String>,
    out: &mut Vec<String>,
) {
    if !seen.insert(type_name.to_string()) {
        return;
    }
    for f in &def.fields {
        let ty = f.ty.trim();
        // `PhantomData<T>` and function pointers do not own `T`.
        if ty.starts_with("PhantomData") || ty.starts_with("fn(") {
            continue;
        }
        if let Some(leaf) = find_gc_leaf_in(ty, leaves) {
            out.push(format!(
                "  {}:{}: {type_name}.{}: `{}` — `unsafe_empty_gc_trace!` type contains GC node `{leaf}`; remove the macro or trace the field",
                def.file, def.line, f.name, f.ty,
            ));
            continue;
        }
        for id in idents(ty) {
            if is_camel(id)
                && let Some(nested) = defs.get(id)
            {
                check_empty_trace(id, nested, defs, leaves, seen, out);
            }
        }
    }
}

/// Non-failing diagnostics for fields the audit cannot classify: a CamelCase
/// type that is neither a local `struct`/`enum` nor in the leaf table, so the
/// audit skipped it instead of deciding whether it is a GC edge. Typical case:
/// a cross-crate hand-written wrapper. Add the type to the leaf table if it is
/// a GC node; the same field then becomes a normal violation/pass. Otherwise
/// the warning is informational and no exemption is needed (or possible: an
/// exemption key only resolves to a verdict for a type the audit can classify).
///
/// These never make the audit fail — `String`/`u32`/`Instant` noise must not
/// break a build — but the CLI prints them so a silent skip is visible.
pub fn unknown_field_type_warnings(dir: &Path) -> Vec<String> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    let roots = root_set(&files, &defs);
    let leaves = leaf_types(&files);
    let mut gc_bind = gc_bind_impls(&files);
    gc_bind.extend(macro_wrapper_types(&files));
    let allow: BTreeMap<&str, &str> = BTreeMap::new();
    let mut warnings = BTreeSet::new();

    for root in &roots {
        let Some(def) = defs.get(root) else {
            continue;
        };
        walk(
            root,
            def,
            false,
            &defs,
            &leaves,
            &gc_bind,
            &allow,
            &mut BTreeSet::new(),
            &mut Vec::new(),
            &mut BTreeSet::new(),
            &mut BTreeMap::new(),
            &mut warnings,
        );
    }
    warnings.into_iter().collect()
}

/// Payload types named in `def_native_object!`/`PayloadPtr::new` arguments but
/// missing from the parsed definitions. Unlike an unknown *field* type this is
/// a failure: the whole payload is absent from the audit, usually because it
/// is declared nested/indented, so it was never treated as a root.
pub fn payload_root_misses(dir: &Path) -> Vec<String> {
    let files = source_files(dir);
    let defs = parse_defs(&files);
    root_scan(&files, &defs)
        .misses
        .into_iter()
        .map(|(name, marker)| {
            format!(
                "  payload type not parsed: {name} ({marker}, nested or indented?) — payload types must be top-level struct/enum items"
            )
        })
        .collect()
}

/// Exemption entries that no longer match anything: the field was renamed,
/// wrapped, or removed from the payload set. Same check the audit applies
/// (derived from [`field_verdicts`], so it cannot disagree with the report).
pub fn stale_exemptions(dir: &Path, allowlist: &[&str]) -> Vec<String> {
    let verdicts = field_verdicts(dir, allowlist);
    allowlist
        .iter()
        .filter(|e| {
            !matches!(
                verdicts.get(allowlist_key(e)),
                Some(FieldVerdict::Allowlisted)
            )
        })
        .map(|e| e.to_string())
        .collect()
}

/// Rule 3: a guard may only be constructed, never assigned over a live field.
pub fn guard_construction_violations(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (file, text) in source_files(dir) {
        for (no, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for marker in [
                "WriteBarrier::new(",
                "WriteBarrierVec::from_vec(",
                "WriteBarrierMap::new(",
            ] {
                let mut from = 0;
                while let Some(idx) = line[from..].find(marker) {
                    let idx = from + idx;
                    let before = line[..idx].trim_end();
                    // `x.field = WriteBarrier::new(..)` — a field assignment,
                    // not a `let`/struct-literal construction. `*controller =
                    // …` (a match-arm binding deref) is the same whole-field
                    // replacement: the lhs has no `.`, but it still bypasses
                    // the barrier.
                    let lhs = before.split([';', '{', '}']).next_back().unwrap_or(before);
                    let field_assign = before.ends_with('=')
                        && (lhs.contains('.') || lhs.trim_start().starts_with('*'));
                    let mem_op = before.contains("mem::replace") || before.contains("mem::swap");
                    if field_assign || mem_op {
                        out.push(format!("  {file}:{}: {}", no + 1, line.trim()));
                    }
                    from = idx + marker.len();
                }
            }
        }
    }
    out
}

/// Run the whole L2b audit over `dir` — rules 1–3: field violations (with
/// stale allowlist entries) plus guard-construction violations. Empty result
/// means clean. This is the one call an embedding crate's test needs; the
/// allowlist is that crate's own (entries are `Type.field — reason`).
pub fn all_violations(dir: &Path, allowlist: &[&str]) -> Vec<String> {
    let mut out = empty_trace_violations(dir);
    out.extend(gc_field_violations(dir, allowlist));
    out.extend(guard_construction_violations(dir));
    out
}

/// The whole standard `tests/gc_audit.rs` body: rules 1–3 (via
/// [`all_violations`]) plus the anti-vacuous root-set check — a scan that
/// found fewer than `min_roots` payload types cannot pass. Returns the
/// failure lines (empty = clean).
///
/// The checks live here so every crate's test file is the same thin wrapper:
/// it only supplies its own source directory, allowlist and root-count floor.
pub fn test_failures(dir: &Path, allowlist: &[&str], min_roots: usize) -> Vec<String> {
    let mut out = all_violations(dir, allowlist);
    out.extend(payload_root_misses(dir));
    let roots = payload_root_types(dir);
    if roots.len() < min_roots {
        out.push(format!(
            "  root set suspiciously small: {} entries (< {min_roots}): {roots:?}",
            roots.len()
        ));
    }
    out
}

/// `Type.field — reason` → `Type.field`.
fn allowlist_key(entry: &str) -> &str {
    entry.split(" — ").next().unwrap_or(entry).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `object_cast!`/`gcref_wrapper!` target in zuqe's own sources must
    /// be in the leaf table, or the audit silently skips those fields — and a
    /// host auditing against the same table inherits the hole.
    #[test]
    fn the_leaf_table_covers_every_wrapper_macro_site() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let table = leaf_table();
        let missing: Vec<String> = macro_wrapper_types(&source_files(&dir))
            .into_iter()
            .filter(|t| !table.contains(t.as_str()))
            .collect();
        assert!(
            missing.is_empty(),
            "add these wrapper types to HAND_WRITTEN_WRAPPERS: {missing:?}"
        );
    }

    /// `split_top` must not treat the `>` of a fn-return arrow as a generic
    /// close: `Vec<fn() -> bool>` otherwise desyncs the depth and swallows
    /// every field that follows it (it hid `ContextImpl.realm_symbols`).
    #[test]
    fn split_top_tracks_fn_return_arrows() {
        let parts = split_top("a: Vec<fn(&mut C) -> bool>, b: u8");
        assert_eq!(parts.len(), 2, "{parts:?}");
        assert!(parts[0].contains("fn(&mut C) -> bool"), "{parts:?}");
        assert!(parts[1].starts_with("b:"), "{parts:?}");
    }

    /// Tuple field types are audited element-wise; nested tuples flatten and
    /// non-tuple types (incl. `fn(..) -> T`) pass through unchanged.
    #[test]
    fn expand_tuple_flattens_elements() {
        assert_eq!(expand_tuple("(JSValue, u32)"), ["JSValue", "u32"]);
        assert_eq!(
            expand_tuple("((JSValue, Atom), u32)"),
            ["JSValue", "Atom", "u32"]
        );
        assert_eq!(expand_tuple("JSValue"), ["JSValue"]);
        assert_eq!(expand_tuple("fn(&mut C) -> bool"), ["fn(&mut C) -> bool"]);
        assert_eq!(expand_tuple("()"), [] as [&str; 0]);
    }

    /// The discovery scanner lists every definition with a direct GC-leaf
    /// The discovery inventory lists every definition with a direct GC-leaf
    /// field, only its GC fields, in name order.
    #[test]
    fn gc_leaf_types_lists_definitions_with_gc_fields() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let types = gc_leaf_types(&dir);
        assert!(
            types.windows(2).all(|w| w[0].name < w[1].name),
            "not sorted by type name"
        );

        // bare handle field, and plain fields of the same struct excluded
        let rope = types
            .iter()
            .find(|t| t.name == "RopeImpl")
            .expect("RopeImpl");
        let left = rope.fields.iter().find(|f| f.name == "left").expect("left");
        assert_eq!(left.ty, "StringRef");
        assert!(!left.wrapped && !left.container);
        assert!(
            !rope.fields.iter().any(|f| f.name == "depth"),
            "non-GC field listed"
        );

        // guarded field
        let promise = types
            .iter()
            .find(|t| t.name == "PromisePayload")
            .expect("PromisePayload");
        assert!(
            promise
                .fields
                .iter()
                .any(|f| f.name == "result" && f.wrapped)
        );

        // container element field
        let abuf = types
            .iter()
            .find(|t| t.name == "ArrayBufferPayload")
            .expect("ArrayBufferPayload");
        assert!(
            abuf.fields
                .iter()
                .any(|f| f.name == "typed_array_list" && f.container)
        );
    }

    /// Verdicts come from the same walk as `gc_field_violations`: the three
    /// states must agree with the violation output.
    #[test]
    fn field_verdicts_match_the_violation_walk() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");

        let verdicts = field_verdicts(&dir, &[]);
        assert_eq!(
            verdicts.get("PromisePayload.result"),
            Some(&FieldVerdict::Pass),
            "wrapped field must pass"
        );
        assert!(
            matches!(
                verdicts.get("AGenPayload.async_state"),
                Some(FieldVerdict::Violation(_))
            ),
            "bare root field must violate"
        );

        // every violating verdict must appear in the textual walk output
        let lines = gc_field_violations(&dir, &[]);
        for (key, verdict) in &verdicts {
            if matches!(verdict, FieldVerdict::Violation(_)) {
                assert!(
                    lines.iter().any(|l| l.contains(&format!("{key}: "))),
                    "verdict for {key} missing from the violations: {lines:?}"
                );
            }
        }

        // allowlisted field switches to the allowlisted state
        let verdicts = field_verdicts(&dir, &["AGenPayload.async_state — test exemption"]);
        assert_eq!(
            verdicts.get("AGenPayload.async_state"),
            Some(&FieldVerdict::Allowlisted)
        );
    }

    /// Exemption files are line-oriented: blank lines and `#` comments are
    /// skipped, every other line is one `Type.field — reason` entry.
    #[test]
    fn read_exemptions_skips_comments_and_blank_lines() {
        let dir = std::env::temp_dir().join(format!("zuqe-gc-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("exempt.txt");
        std::fs::write(
            &path,
            "# header comment\n\nA.b — construction only\n   # indented comment\n\nC.d — weak\n",
        )
        .expect("write temp file");

        assert_eq!(
            read_exemptions(&path),
            ["A.b — construction only", "C.d — weak"]
        );
        assert!(
            read_exemptions(&dir.join("missing.txt")).is_empty(),
            "a missing file means no exemptions"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Stale detection equals "the allowlist key did not resolve to an
    /// allowlisted verdict": a bogus key, or a wrapped field (which passes
    /// without the entry), must be reported.
    #[test]
    fn stale_exemptions_detect_unmatched_entries() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(stale_exemptions(&dir, &[]).is_empty());

        let stale = stale_exemptions(
            &dir,
            &[
                "NoSuchType.field — test",
                "AGenPayload.async_state — live entry",
                "PromisePayload.result — wrapped, entry is stale",
            ],
        );
        assert_eq!(
            stale,
            [
                "NoSuchType.field — test",
                "PromisePayload.result — wrapped, entry is stale"
            ]
        );
    }

    /// The one-call entry point must run both halves of the audit — an
    /// embedding crate relying on it must not silently get half a check.
    #[test]
    fn all_violations_runs_field_and_guard_checks() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        // sanity: with no allowlist the field half must report something,
        // otherwise the equality below could pass vacuously.
        assert!(!gc_field_violations(&dir, &[]).is_empty());

        let mut expected = gc_field_violations(&dir, &[]);
        expected.extend(guard_construction_violations(&dir));
        assert_eq!(all_violations(&dir, &[]), expected);
    }

    /// `test_failures` is `all_violations` plus the root-set floor: floor 0
    /// must be exactly the violations, an impossible floor must trip the
    /// sanity line.
    #[test]
    fn test_failures_adds_the_root_set_floor() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(test_failures(&dir, &[], 0), all_violations(&dir, &[]));
        assert!(
            test_failures(&dir, &[], usize::MAX)
                .iter()
                .any(|l| l.contains("root set suspiciously small")),
            "impossible floor must report the root set"
        );
    }

    /// Hand-written tuple structs that hold a GC handle but are not macro
    /// wrappers must be leaves — otherwise a payload field of one of these
    /// types is silently skipped (`StringRef`/`BigIntRef` blind spot).
    #[test]
    fn the_leaf_table_covers_handle_tuple_structs() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let table = leaf_table();
        let mut missing = Vec::new();
        for (file, text) in source_files(&dir) {
            for (no, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                // `pub struct Name(<handle>…);` — tuple struct holding a GC
                // handle directly (multi-line declarations are not scanned).
                let Some(rest) = t
                    .strip_prefix("pub struct ")
                    .or_else(|| t.strip_prefix("pub(crate) struct "))
                    .or_else(|| t.strip_prefix("struct "))
                else {
                    continue;
                };
                if !(rest.contains("GcRef<") || rest.contains("NonNull<GcHead>")) {
                    continue;
                }
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect();
                if name.is_empty() || !table.contains(name.as_str()) {
                    missing.push(format!("{file}:{}: {name}", no + 1));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "add these GC-handle tuple structs to HAND_WRITTEN_WRAPPERS: {missing:?}"
        );
    }

    /// D.5.1/D.5.2: containers and tuple structs the first audit version
    /// silently skipped. `UnsafeCell` is a cell, not a container: its element
    /// must still be reported as a bare field.
    #[test]
    fn peel_understands_the_container_table() {
        assert_eq!(
            peel("SmallVec<[JSValue; 2]>"),
            ("JSValue", false, true, false)
        );
        assert_eq!(peel("VecDeque<JSValue>"), ("JSValue", false, true, false));
        assert_eq!(
            peel("HashList<ShapeProperty>"),
            ("ShapeProperty", false, true, false)
        );
        assert_eq!(
            peel("UnsafeCell<JSValue>"),
            ("JSValue", false, false, false)
        );
        // a guarded map is wrapped *and* still a container (element needs GcBind)
        assert_eq!(
            peel("WriteBarrierMap<Atom, JSValue>"),
            ("Atom, JSValue", true, true, false)
        );
        // longest prefix first: `WriteBarrier` must not swallow the others
        assert_eq!(
            peel("WriteBarrierVec<JSValue>"),
            ("JSValue", true, true, false)
        );
        assert_eq!(
            peel("WriteBarrier<JSValue>"),
            ("JSValue", true, false, false)
        );
    }

    /// Shared/sync ownership wrappers must be peeled through: a GC leaf hidden
    /// inside `Rc`/`Arc`/locks is still a GC leaf and must not escape detection.
    /// The 4th element flags that a shared wrapper (Rc/Arc/Weak) was seen.
    #[test]
    fn peel_sees_through_shared_wrappers() {
        assert_eq!(peel("Rc<JSValue>"), ("JSValue", false, false, true));
        assert_eq!(peel("Arc<JSValue>"), ("JSValue", false, false, true));
        assert_eq!(
            peel("Rc<RefCell<JSValue>>"),
            ("JSValue", false, false, true)
        );
        assert_eq!(
            peel("Arc<Mutex<RefCell<JSValue>>>"),
            ("JSValue", false, false, true)
        );
        assert_eq!(peel("Pin<Box<JSValue>>"), ("JSValue", false, true, false));
        // Path-qualified: prefix strip doesn't match, but the safety-net scan
        // in the audit still catches the GC leaf.
        assert_eq!(
            peel("std::rc::Rc<JSValue>"),
            ("std::rc::Rc<JSValue>", false, false, false)
        );
    }

    /// `base_name` must resolve raw pointers so `*mut JSValue`/`*const JSValue`
    /// are recognized as carrying a GC leaf, not skipped as `mut`/`const`.
    #[test]
    fn base_name_resolves_raw_pointers() {
        assert_eq!(base_name("JSValue").as_deref(), Some("JSValue"));
        assert_eq!(base_name("*mut JSValue").as_deref(), Some("JSValue"));
        assert_eq!(base_name("*const JSValue").as_deref(), Some("JSValue"));
        assert_eq!(base_name("NonNull<JSValue>").as_deref(), Some("NonNull"));
        assert_eq!(base_name("std::rc::Rc<JSValue>").as_deref(), Some("Rc"));
    }

    /// Safety net: GC leaf identifiers are found anywhere inside a type string,
    /// so an unrecognized wrapper cannot hide a GC node from the audit.
    #[test]
    fn find_gc_leaf_in_detects_hidden_leaves() {
        let leaves = leaf_table()
            .into_iter()
            .map(String::from)
            .collect::<BTreeSet<_>>();
        // `Rc`/`RefCell` are not GC leaves; the first GC leaf token is `JSValue`.
        assert_eq!(
            find_gc_leaf_in("Rc<RefCell<JSValue>>", &leaves).as_deref(),
            Some("JSValue")
        );
        assert_eq!(
            find_gc_leaf_in("MyBox<Arc<JSValue>>", &leaves).as_deref(),
            Some("JSValue")
        );
        assert_eq!(
            find_gc_leaf_in("HashMap<String, Rc<Object>>", &leaves).as_deref(),
            Some("Object")
        );
        assert_eq!(
            find_gc_leaf_in("Rc<RefCell<Atom>>", &leaves).as_deref(),
            Some("Atom")
        );
        assert_eq!(
            find_gc_leaf_in("std::rc::Rc<Promise>", &leaves).as_deref(),
            Some("Promise")
        );
        assert_eq!(find_gc_leaf_in("Vec<u8>", &leaves), None);
        assert_eq!(find_gc_leaf_in("String", &leaves), None);
    }

    /// End-to-end: the full audit (root walk + field verdicts) must flag GC
    /// leaves hidden in any compound wrapper, on a synthetic payload.    #[test]
    fn the_audit_catches_gc_leaves_in_compound_wrappers() {
        let dir = std::env::temp_dir().join(format!(
            "zuqe-gc-audit-compound-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("create temp src");
        // Build the fixture by parts so the text scanner (which reads source
        // files naively) does not see `pub struct` / `PayloadPtr::new` tokens in
        // this file and treat them as real definitions/roots.
        let fixture = [
            "use std::cell::RefCell;\n",
            "use std::rc::Rc;\n",
            "use std::sync::{Arc, Mutex, RwLock};\n\n",
            "pu",
            "b struct HiddenPayload {\n",
            "    pu",
            "b a: Rc<RefCell<JSValue>>,\n",
            "    pu",
            "b b: Rc<JSValue>,\n",
            "    pu",
            "b c: Arc<JSValue>,\n",
            "    pu",
            "b d: Arc<Mutex<JSValue>>,\n",
            "    pu",
            "b e: Rc<RefCell<Option<Object>>>,\n",
            "    pu",
            "b f: Arc<RefCell<Atom>>,\n",
            "    pu",
            "b g: *mut JSValue,\n",
            "    pu",
            "b h: HashMap<String, Rc<JSValue>>,\n",
            "    pu",
            "b i: CustomWrap<JSValue>,\n",
            "    pu",
            "b j: Option<Rc<JSValue>>,\n",
            "    pu",
            "b m: std::rc::Rc<JSValue>,\n",
            "    pu",
            "b n: Arc<RwLock<JSValue>>,\n",
            "    pu",
            "b o: ManuallyDrop<JSValue>,\n",
            "    pu",
            "b p: MaybeUninit<JSValue>,\n",
            "    pu",
            "b q: Pin<Box<JSValue>>,\n",
            "    pu",
            "b r: Rc<RefCell<Promise>>,\n",
            "    pu",
            "b s: *const Object,\n",
            "    pu",
            "b t: (Rc<JSValue>, u32),\n",
            "    pu",
            "b u: Vec<Rc<JSValue>>,\n",
            "    pu",
            "b v: WriteBarrier<Rc<JSValue>>,\n",
            "    pu",
            "b w: WriteBarrierVec<Rc<JSValue>>,\n",
            "    pu",
            "b k: WriteBarrier<JSValue>,\n",
            "    pu",
            "b l: WriteBarrierVec<JSValue>,\n",
            "    pu",
            "b plain: u32,\n",
            "}\n\n",
            "// Register the payload so the root-set walk reaches it.\n",
            "fn _root() {\n",
            "    let _ = PayloadPtr::ne",
            "w(0, HiddenPayload::ne",
            "w());\n",
            "}\n",
            // Provide GcBind impls so rule-2 container checks see them in this
            // synthetic dir (normally discovered from zuqe's own sources).
            "impl GcBind for JSValue { fn bind_node(&self,_:&(),_:()){} }\n",
            "impl GcBind for Object { fn bind_node(&self,_:&(),_:()){} }\n",
            "impl GcBind for Atom { fn bind_node(&self,_:&(),_:()){} }\n",
            "impl GcBind for Promise { fn bind_node(&self,_:&(),_:()){} }\n",
        ]
        .concat();
        std::fs::write(src.join("lib.rs"), fixture).expect("write temp lib.rs");

        let verdicts = field_verdicts(&src, &[]);
        // GC leaf hidden in shared/unknown wrappers, raw ptrs, or shared wrappers
        // behind guards/containers — all violations (一切皆可疑).
        for key in [
            "HiddenPayload.a",
            "HiddenPayload.b",
            "HiddenPayload.c",
            "HiddenPayload.d",
            "HiddenPayload.e",
            "HiddenPayload.f",
            "HiddenPayload.g",
            "HiddenPayload.h",
            "HiddenPayload.i",
            "HiddenPayload.j",
            "HiddenPayload.m",
            "HiddenPayload.n",
            "HiddenPayload.o",
            "HiddenPayload.p",
            "HiddenPayload.r",
            "HiddenPayload.s",
            "HiddenPayload.t",
            "HiddenPayload.u", // plain Vec + Rc(shared) => caught
            "HiddenPayload.v", // WriteBarrier + Rc(shared) => still caught
            "HiddenPayload.w", // WriteBarrierVec + Rc(shared) => still caught
        ] {
            assert!(
                matches!(verdicts.get(key), Some(FieldVerdict::Violation(_))),
                "{key} must be a violation, got {:?}",
                verdicts.get(key)
            );
        }
        // `Pin<Box<JSValue>>`: no Rc/Arc shared wrapper; Box is a container and
        // JSValue has GcBind, so rule 2 accepts it (same shape as Vec<JSValue>).
        // Not a shared-escape case the Rc/Arc rule targets.
        assert_eq!(
            verdicts.get("HiddenPayload.q"),
            Some(&FieldVerdict::Pass),
            "Pin<Box<JSValue>> passes via container+GcBind (no shared wrapper), got {:?}",
            verdicts.get("HiddenPayload.q")
        );
        // Properly guarded fields pass.
        assert_eq!(
            verdicts.get("HiddenPayload.k"),
            Some(&FieldVerdict::Pass),
            "WriteBarrier<JSValue> must pass"
        );
        assert_eq!(
            verdicts.get("HiddenPayload.l"),
            Some(&FieldVerdict::Pass),
            "WriteBarrierVec<JSValue> must pass"
        );
        // Non-GC field is absent from verdicts.
        assert!(verdicts.get("HiddenPayload.plain").is_none());

        // With exemptions the specific fields become allowlisted.
        let verdicts = field_verdicts(
            &src,
            &[
                "HiddenPayload.a — reviewed",
                "HiddenPayload.g — raw ptr reviewed",
            ],
        );
        assert_eq!(
            verdicts.get("HiddenPayload.a"),
            Some(&FieldVerdict::Allowlisted)
        );
        assert_eq!(
            verdicts.get("HiddenPayload.g"),
            Some(&FieldVerdict::Allowlisted)
        );
        // Others still violate.
        assert!(matches!(
            verdicts.get("HiddenPayload.b"),
            Some(FieldVerdict::Violation(_))
        ));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Create a throwaway `<tmp>/<name>/src/lib.rs` fixture and return the
    /// crate root (the directory the audit scans is `root/src`).
    fn write_fixture(name: &str, source: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zuqe-gc-audit-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).expect("create temp src");
        std::fs::write(src.join("lib.rs"), source).expect("write fixture");
        dir
    }

    /// A type declared with `unsafe_empty_gc_trace!` must contain no GC node:
    /// wrapping the field does not help because the type is never traced.
    #[test]
    fn empty_trace_type_with_gc_field_fails() {
        let dir = write_fixture(
            "empty-trace-gc",
            "unsafe_empty_gc_trace!(Bad);\n\
             struct Bad {\n\
                 value: JSValue,\n\
             }\n\
             struct Wrapped {\n\
                 value: WriteBarrier<JSValue>,\n\
             }\n\
             unsafe_empty_gc_trace!(Wrapped);\n",
        );
        let v = empty_trace_violations(&dir.join("src"));
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().any(|l| l.contains("Bad.value")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("Wrapped.value")), "{v:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Plain data, and GC nodes reached through a local struct, are checked
    /// recursively; a clean no-trace payload passes.
    #[test]
    fn empty_trace_type_checks_nested_local_types() {
        let dir = write_fixture(
            "empty-trace-nested",
            "unsafe_empty_gc_trace!(Clean, Wrapper);\n\
             struct Clean {\n\
                 bytes: Vec<u8>,\n\
                 name: String,\n\
             }\n\
             struct Nested {\n\
                 root: JSValue,\n\
             }\n\
             struct Wrapper {\n\
                 nested: Nested,\n\
             }\n",
        );
        let v = empty_trace_violations(&dir.join("src"));
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("Nested.root"), "{v:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-5: two enum variants may declare the same field name; the audit key
    /// must be `Type::Variant.field`, and an exemption must cover only the
    /// variant it names.
    #[test]
    fn enum_variant_field_keys_are_distinct_and_exemptions_are_scoped() {
        let dir = write_fixture(
            "enum-keys",
            "pub enum Kind {
    Default { pending_read: JSValue, controller: Option<JSValue> },
    Byte { pending_read: JSValue },
}

pub struct Payload {
    pub kind: Kind,
}

fn _root() {
    let _ = PayloadPtr::new(0, Payload::new());
}
",
        );
        let src = dir.join("src");

        let verdicts = field_verdicts(&src, &[]);
        for key in [
            "Kind::Default.pending_read",
            "Kind::Default.controller",
            "Kind::Byte.pending_read",
        ] {
            assert!(
                matches!(verdicts.get(key), Some(FieldVerdict::Violation(_))),
                "{key} must be a violation, got {:?}",
                verdicts.get(key)
            );
        }
        // The variant-blind bare key must be gone: a mistake that reintroduces
        // it would make one variant's exemption cover the other's field.
        assert!(verdicts.get("Kind.pending_read").is_none());

        // Exempting Byte.pending_read leaves Default.pending_read violating.
        let verdicts = field_verdicts(&src, &["Kind::Byte.pending_read — reviewed"]);
        assert_eq!(
            verdicts.get("Kind::Byte.pending_read"),
            Some(&FieldVerdict::Allowlisted)
        );
        assert!(
            matches!(
                verdicts.get("Kind::Default.pending_read"),
                Some(FieldVerdict::Violation(_))
            ),
            "Default.pending_read must stay violated: {:?}",
            verdicts.get("Kind::Default.pending_read")
        );

        // Discovery exposes the same variant-aware key.
        let types = gc_leaf_types(&src);
        let kind = types.iter().find(|t| t.name == "Kind").expect("Kind");
        let pending: Vec<String> = kind
            .fields
            .iter()
            .filter(|f| f.name == "pending_read")
            .map(|f| f.key(&kind.name))
            .collect();
        assert_eq!(
            pending,
            ["Kind::Default.pending_read", "Kind::Byte.pending_read"]
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// An exemption is a global fact about a `Type.field` key. When the same
    /// type is reached both through a container (a natural rule-2 pass) and
    /// directly as a root (bare), the allowlisted verdict must win; otherwise
    /// `field_verdicts` disagrees with `gc_field_violations` and the entry is
    /// reported stale even though it is still needed.
    #[test]
    fn allowlisted_verdict_wins_over_a_natural_pass_from_another_root_path() {
        let dir = write_fixture(
            "allowlisted-wins",
            "pub struct Inner {
    pub v: JSValue,
}

impl GcBind for JSValue {
    fn bind_node(&self, _: &(), _: ()) {}
}

pub struct Container {
    pub items: Vec<Inner>,
}

pub struct Direct {
    pub inner: Inner,
}

fn _root() {
    let _ = PayloadPtr::new(0, Container::new());
    let _ = PayloadPtr::new(1, Direct::new());
}
",
        );
        let src = dir.join("src");
        let allow = ["Inner.v — direct root needs the exemption"];

        assert_eq!(
            field_verdicts(&src, &allow).get("Inner.v"),
            Some(&FieldVerdict::Allowlisted),
            "the direct-root visit must upgrade the container-path Pass"
        );
        assert!(
            stale_exemptions(&src, &allow).is_empty(),
            "the exemption is still used by the direct-root path"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-4 + P3-7: a payload declared inside a module (indented) is not parsed
    /// as a top-level item, so the macro root must be reported instead of
    /// silently dropped.
    #[test]
    fn nested_payload_is_reported_as_a_root_miss() {
        let dir = write_fixture(
            "root-miss",
            "mod outer {
    pub(crate) struct NestedPayload {
        pub(crate) v: JSValue,
    }
}

fn _root() {
    let _ = PayloadPtr::new(0, outer::NestedPayload::new());
}
",
        );
        let src = dir.join("src");

        let misses = payload_root_misses(&src);
        assert!(
            misses.iter().any(|m| m.contains("NestedPayload")),
            "miss must name the payload: {misses:?}"
        );
        assert!(
            misses.iter().any(|m| m.contains("nested or indented")),
            "miss must explain the convention: {misses:?}"
        );
        // It is a failure, not a warning.
        assert!(
            test_failures(&src, &[], 0)
                .iter()
                .any(|l| l.contains("payload type not parsed: NestedPayload")),
            "root miss must fail the audit"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P2-6: an unresolvable CamelCase field type is a non-failing warning;
    /// once it is in the leaf table the same field becomes a normal violation.
    #[test]
    fn unknown_field_type_is_a_warning_and_the_leaf_table_changes_the_verdict() {
        let dir = write_fixture(
            "unknown-type",
            "pub struct FooPayload {
    pub handle: ForeignHandle,
}

fn _root() {
    let _ = PayloadPtr::new(0, FooPayload::new());
}
",
        );
        let src = dir.join("src");

        let warnings = unknown_field_type_warnings(&src);
        assert!(
            warnings.iter().any(|w| w.contains("ForeignHandle")),
            "warning must name the type: {warnings:?}"
        );
        assert!(
            all_violations(&src, &[]).is_empty(),
            "unknown types stay non-failing"
        );
        assert!(
            test_failures(&src, &[], 0).is_empty(),
            "warnings must not fail the audit"
        );

        // Teach the audit that ForeignHandle is a GC leaf (the fixture is not
        // compiled, so any macro shape that declares the wrapper works).
        std::fs::write(src.join("wrapper.rs"), "object_cast!(ForeignHandle, 1);\n")
            .expect("write wrapper fixture");
        let verdicts = field_verdicts(&src, &[]);
        assert!(
            matches!(
                verdicts.get("FooPayload.handle"),
                Some(FieldVerdict::Violation(_))
            ),
            "leaf-table entry must turn the warning into a violation: {:?}",
            verdicts.get("FooPayload.handle")
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P1-3: `*controller = WriteBarrier::new(..)` (a match-arm binding deref,
    /// lhs has no `.`) is the same whole-field replacement as `x.slot = …`.
    #[test]
    fn guard_construction_catches_deref_binding_replacement() {
        let dir = write_fixture(
            "guard-deref",
            "fn f() {
    *controller = WriteBarrier::new(Some(c));
    self.slot = WriteBarrier::new(v);
    let ok = WriteBarrier::new(v);
    mem::replace(&mut p.slot, WriteBarrier::new(v));
}
",
        );
        let src = dir.join("src");

        let found = guard_construction_violations(&src);
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(
            found.iter().any(|l| l.contains("*controller =")),
            "deref-binding replacement missing: {found:?}"
        );
        assert!(found.iter().any(|l| l.contains("self.slot =")));
        assert!(found.iter().any(|l| l.contains("mem::replace")));
        assert!(!found.iter().any(|l| l.contains("let ok")));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// P3-7 support: test-only modules are excluded from scanning, so their
    /// fixture payloads do not turn into false root misses.
    #[test]
    fn cfg_test_modules_are_not_scanned() {
        let text = "pub struct Real { pub v: JSValue }\n\
                    #[cfg(test)]\n\
                    mod tests {\n\
                        struct Fixture { v: JSValue }\n\
                        fn f() { let _ = PayloadPtr::new(0, Fixture::new()); }\n\
                    }\n\
                    fn _root() { let _ = PayloadPtr::new(0, Real::new()); }\n";
        let stripped = strip_cfg_test_modules(text);
        assert!(stripped.contains("struct Real"));
        assert!(!stripped.contains("Fixture"), "{stripped}");

        let files = vec![("lib.rs".to_string(), stripped)];
        let defs = parse_defs(&files);
        assert!(defs.contains_key("Real"));
        assert!(!defs.contains_key("Fixture"));
        assert!(payload_root_misses_from(&files, &defs).is_empty());
    }

    fn payload_root_misses_from(
        files: &[(String, String)],
        defs: &BTreeMap<String, Def>,
    ) -> Vec<String> {
        root_scan(files, defs)
            .misses
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }
}
