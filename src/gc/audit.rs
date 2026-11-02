// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Static audit of GC payload fields — the L2b layer of `memo/gc_bind_guard.md`.
//!
//! The type system cannot stop someone from declaring `foo: JSValue` instead of
//! `WriteBarrier<JSValue>`. This module walks a source tree's GC payload root
//! set (`gc_type_register!` entries, every `def_native_object!` payload type and
//! every `PayloadPtr::new` payload type), expands local struct/enum field types
//! recursively, and reports every GC-typed field that is neither wrapped in
//! `WriteBarrier`/`WriteBarrierVec` nor allowlisted — with `file:line`.
//!
//! Three rules, all heuristic (deliberately no `syn`/`regex` dependency):
//!
//! 1. a GC leaf field of a payload type must be wrapped, unless allowlisted;
//! 2. a GC leaf field of a *container element* type (reached through `Vec`/
//!    `Box`/`[…]`) must be covered by an `impl GcBind for ElementType`, or be
//!    allowlisted — elements are bound by their container's `push`;
//! 3. `WriteBarrier::new` / `WriteBarrierVec::from_vec` may only construct
//!    (struct-literal field init, `let`); on the right of a field assignment or
//!    inside `mem::replace`/`mem::swap` it is a whole-field replacement that
//!    bypasses the barrier.
//!
//! # Using it from an embedding crate
//!
//! Add one test to your own crate (allowlist entries are `Type.field — reason`):
//!
//! ```ignore
//! use std::path::Path;
//!
//! const ALLOWLIST: &[&str] = &["MyPayload.cache — construction only"];
//!
//! #[test]
//! fn gc_payload_fields() {
//!     let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
//!     let v = zuqe::gc_audit::gc_field_violations(&src, ALLOWLIST);
//!     assert!(v.is_empty(), "{}", v.join("\n"));
//!
//!     let v = zuqe::gc_audit::guard_construction_violations(&src);
//!     assert!(v.is_empty(), "{}", v.join("\n"));
//! }
//! ```
//!
//! Unknown field types are skipped rather than guessed, and a payload type
//! unreachable from the root set is invisible here — this is a review-speed
//! bump, not a compiler.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// GC leaf bases. The object wrappers come from [`leaf_table`] (zuqe's own
/// wrapper list) plus whatever `object_cast!`/`gcref_wrapper!`/`def_native_object!`
/// sites exist in the audited directory.
const GC_LEAF_BASES: &[&str] = &["JSValue", "Atom", "PropertyKey"];

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
];

struct Field {
    name: String,
    ty: String,
    skip: bool,
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
            out.push((rel(&path, dir), text));
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

/// Split on top-level commas, tracking (), [], {} and generic `<>` depth.
fn split_top(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' | '[' | '{' | '<' => {
                depth += 1;
                cur.push(ch);
            }
            ')' | ']' | '}' | '>' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
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
                .split(|c: char| c.is_whitespace() || c == '(' || c == '{')
                .next()
                .unwrap_or("")
                .to_string();

            // Tuple/unit struct: `pub struct X(...);` — no named fields.
            if line.contains(';') && !line.contains('{') {
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
                        fields: parse_fields(&body),
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
fn parse_fields(body: &str) -> Vec<Field> {
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

        // `name: Type` — a struct field.
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
                });
                continue;
            }
        }

        // `Variant { ... }` — an enum struct-variant: recurse into its fields.
        if let Some(open) = rest.find('{') {
            let head = rest[..open].trim();
            if is_plain_ident(head) {
                let close = rest.rfind('}').unwrap_or(rest.len());
                out.extend(parse_fields(&rest[open + 1..close]));
            }
            continue;
        }

        // `Variant(T, ...)` — an enum tuple-variant: each element is a field.
        if let Some(open) = rest.find('(') {
            let head = rest[..open].trim();
            if is_plain_ident(head) {
                let close = rest.rfind(')').unwrap_or(rest.len());
                for (n, part) in split_top(&rest[open + 1..close]).into_iter().enumerate() {
                    if part.is_empty() {
                        continue;
                    }
                    out.push(Field {
                        name: format!("{head}.{n}"),
                        ty: part,
                        skip,
                    });
                }
            }
        }
    }
    out
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

/// Offsets of real call sites: the marker must be followed by optional
/// whitespace and the opening delimiter, so prose/comments mentioning the
/// macro are not parsed as code.
fn call_sites(text: &str, marker: &str, open: char) -> Vec<usize> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(idx) = text[from..].find(marker) {
        let idx = from + idx;
        let after = &text[idx + marker.len()..];
        if after.trim_start().starts_with(open) {
            out.push(idx);
        }
        from = idx + marker.len();
    }
    out
}

/// Root set: `gc_type_register!` entries, `def_native_object!` payload types
/// and `PayloadPtr::new` payload types. The last source is what catches enum
/// payloads (`ObjectPayload::LIST(...)`) that no macro declares.
fn root_set(files: &[(String, String)], defs: &BTreeMap<String, Def>) -> BTreeSet<String> {
    let mut roots = BTreeSet::new();

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
                    roots.insert(name.to_string());
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
                    roots.insert(t);
                }
            }
        }
    }
    roots
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
/// `[Vec<T>; 2]` → `T`. Returns `(inner, wrapped_by_guard,
/// went_through_container)`.
fn peel(mut ty: &str) -> (&str, bool, bool) {
    let mut wrapped = false;
    let mut in_container = false;
    loop {
        let t = ty.trim();
        if let Some(inner) = strip_layer(t, &["WriteBarrierVec", "WriteBarrier"]) {
            wrapped = true;
            // a guarded Vec is still a container: its element type's fields are
            // covered by the element's `GcBind` impl, not by the guard
            in_container |= t.starts_with("WriteBarrierVec");
            ty = inner;
            continue;
        }
        let mut moved = false;
        for layer in [
            "Option", "RefCell", "Cell", "Box", "Vec", "HashMap", "BTreeMap", "IndexMap",
        ] {
            if let Some(inner) = strip_layer(t, &[layer]) {
                in_container |=
                    matches!(layer, "Box" | "Vec" | "HashMap" | "BTreeMap" | "IndexMap");
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
    (ty.trim(), wrapped, in_container)
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
) {
    if !seen.insert(type_name.to_string()) {
        return;
    }
    for f in &def.fields {
        if f.skip {
            continue;
        }
        let (inner, wrapped, container) = peel(&f.ty);
        let through_container = through_container || container;

        let Some(base) = base_name(inner) else {
            continue;
        };
        let is_leaf = leaves.contains(&base);

        if is_leaf {
            if wrapped {
                continue;
            }
            let key = format!("{type_name}.{}", f.name);
            if allow.contains_key(key.as_str()) {
                allow_used.insert(key);
                continue;
            }
            if through_container && gc_bind.contains(type_name) {
                continue;
            }
            violations.push(format!(
                "  {}:{}: {type_name}.{}: `{}` — {}",
                def.file,
                def.line,
                f.name,
                f.ty,
                if through_container {
                    "container element type has no `impl GcBind` and is not allowlisted"
                } else {
                    "bare GC field, not wrapped and not allowlisted"
                }
            ));
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
            );
        }
    }
}

/// Outermost type name of a field type, path-qualified or not:
/// `Option<Vec<T>>` → `Option`, `zuqe::Object` → `Object`,
/// `crate::x::Foo<Bar>` → `Foo`.
fn base_name(ty: &str) -> Option<String> {
    let t = ty
        .trim()
        .trim_start_matches('&')
        .trim_start_matches('*')
        .trim_start_matches("const ")
        .trim();
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
    let gc_bind = gc_bind_impls(&files);

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

/// Rule 3: a guard may only be constructed, never assigned over a live field.
pub fn guard_construction_violations(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (file, text) in source_files(dir) {
        for (no, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for marker in ["WriteBarrier::new(", "WriteBarrierVec::from_vec("] {
                let mut from = 0;
                while let Some(idx) = line[from..].find(marker) {
                    let idx = from + idx;
                    let before = line[..idx].trim_end();
                    // `x.field = WriteBarrier::new(..)` — a field assignment,
                    // not a `let`/struct-literal construction.
                    let field_assign = before.ends_with('=')
                        && before
                            .split([';', '{', '}'])
                            .next_back()
                            .is_some_and(|lhs| lhs.contains('.'));
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
}
