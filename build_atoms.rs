// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::path::Path;

static ATOM_DESCR_DEFS: &[&str] = &[
    //
    // KEYWORD BEGIN - DON'T CHANGE ORDER (same order as TOKENS)
    //
    "null",
    "false",
    "true",
    "if",
    "else",
    "return",
    "var",
    "this",
    "delete",
    "void",
    "typeof",
    "new",
    "in",
    "instanceof",
    "do",
    "while",
    "for",
    "break",
    "continue",
    "switch",
    "case",
    "default",
    "throw",
    "try",
    "catch",
    "finally",
    "function",
    "debugger",
    "with",
    "class",
    "const",
    "enum",
    "export",
    "extends",
    "import",
    "super",
    "implements",
    "interface",
    "let",
    "package",
    "private",
    "protected",
    "public",
    "static",
    "yield",
    "await",
    "empty_string",
    "length",
    "fileName",
    "lineNumber",
    "message",
    "cause",
    "errors",
    "stack",
    "name",
    "toString",
    "toLocaleString",
    "valueOf",
    "eval",
    "prototype",
    "constructor",
    "configurable",
    "writable",
    "enumerable",
    "value",
    "get",
    "set",
    "of",
    // KEYWORDS END
    "__proto__",
    "undefined",
    "number",
    "boolean",
    "string",
    "object",
    "symbol",
    "integer",
    "unknown",
    "arguments",
    "callee",
    "caller",
    "<eval>",
    "<ret>",
    "<var>",
    "<arg_var>",
    "<with>",
    "lastIndex",
    "target",
    "index",
    "input",
    "defineProperties",
    "apply",
    "join",
    "concat",
    "split",
    "construct",
    "getPrototypeOf",
    "setPrototypeOf",
    "isExtensible",
    "preventExtensions",
    "has",
    "deleteProperty",
    "defineProperty",
    "getOwnPropertyDescriptor",
    "ownKeys",
    "add",
    "done",
    "next",
    "values",
    "source",
    "flags",
    "global",
    "unicode",
    "raw",
    "new.target",
    "this.active_func",
    "<home_object>",
    "<computed_field>",
    "<static_computed_field>",
    "<class_fields_init>",
    "<brand>",
    "#constructor",
    "as",
    "from",
    "meta",
    "*default*",
    "*",
    "Module",
    "then",
    "resolve",
    "reject",
    "promise",
    "proxy",
    "revoke",
    "async",
    "exec",
    "groups",
    "indices",
    "status",
    "reason",
    "globalThis",
    "bigint",
    //
    // atomics
    //
    // #[cfg(feature = "atomics")]
    // "not_equal",
    // #[cfg(feature = "atomics")]
    // "timed_out",
    // #[cfg(feature = "atomics")]
    // "ok",
    //
    #[cfg(feature = "builtin_json")]
    "toJSON",
    //
    // class names
    //
    "Object",
    "Array",
    "Error",
    "Number",
    "String",
    "Boolean",
    "Symbol",
    "Arguments",
    #[cfg(feature = "builtin_math")]
    "Math",
    #[cfg(feature = "builtin_json")]
    "JSON",
    #[cfg(feature = "builtin_json")]
    "rawJSON",
    #[cfg(feature = "builtin_json")]
    "RawJSON",
    #[cfg(feature = "builtin_date")]
    "Date",
    "Function",
    "GeneratorFunction",
    "ForInIterPayload",
    #[cfg(feature = "builtin_regexp")]
    "RegExp",
    "ArrayBuffer",
    "SharedArrayBuffer",
    //
    // typed array
    // must keep same order as class IDs for typed arrays
    //
    "Uint8ClampedArray",
    "Int8Array",
    "Uint8Array",
    "Int16Array",
    "Uint16Array",
    "Int32Array",
    "Uint32Array",
    "BigInt64Array",
    "BigUint64Array",
    "Float16Array",
    "Float32Array",
    "Float64Array",
    "DataView",
    "BigInt",
    //
    #[cfg(feature = "builtin_mapset")]
    "Map",
    #[cfg(feature = "builtin_mapset")]
    "Set", // Map+1
    #[cfg(feature = "builtin_mapset")]
    "WeakMap", // Map+2
    #[cfg(feature = "builtin_mapset")]
    "WeakSet", // Map+3
    #[cfg(feature = "builtin_mapset")]
    "Map_Iterator",
    #[cfg(feature = "builtin_mapset")]
    "Set_Iterator",
    "Array_Iterator",
    "String_Iterator",
    #[cfg(feature = "builtin_regexp")]
    "RegExp_String_Iterator",
    "Generator",
    "Iterator",
    #[cfg(feature = "builtin_proxy")]
    "Proxy",
    "Promise",
    "PromiseResolveFunction",
    "PromiseRejectFunction",
    "AsyncFunction",
    "AsyncFunctionResolve",
    "AsyncFunctionReject",
    "AsyncGeneratorFunction",
    "AsyncGenerator",
    "AsyncIterableIterator",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "InternalError",
    "AggregateError",
    #[cfg(feature = "builtin_weakref")]
    "WeakRef",
    #[cfg(feature = "builtin_weakref")]
    "FinalizationRegistry",
    //
    // intl402
    //
    #[cfg(feature = "intl402")]
    "Intl",
    #[cfg(feature = "intl402")]
    "Collator",
    #[cfg(feature = "intl402")]
    "DateTimeFormat",
    #[cfg(feature = "intl402")]
    "DurationFormat",
    #[cfg(feature = "intl402")]
    "ListFormat",
    #[cfg(feature = "intl402")]
    "Locale",
    #[cfg(feature = "intl402")]
    "NumberFormat",
    #[cfg(feature = "intl402")]
    "PluralRules",
    #[cfg(feature = "intl402")]
    "RelativeTimeFormat",
    #[cfg(feature = "intl402")]
    "Segmenter",
    #[cfg(feature = "intl402")]
    "DisplayNames",
    #[cfg(feature = "intl402")]
    "getCanonicalLocales",
    #[cfg(feature = "intl402")]
    "supportedLocalesOf",
    #[cfg(feature = "intl402")]
    "resolvedOptions",
    #[cfg(feature = "intl402")]
    "format",
    #[cfg(feature = "intl402")]
    "formatToParts",
    #[cfg(feature = "intl402")]
    "select",
    #[cfg(feature = "intl402")]
    "compare",
    // privates
    "Private_brand",
    //
    // symbols
    //
    "Symbol_toPrimitive",
    "Symbol_iterator",
    "Symbol_match",
    "Symbol_matchAll",
    "Symbol_replace",
    "Symbol_search",
    "Symbol_split",
    "Symbol_toStringTag",
    "Symbol_isConcatSpreadable",
    "Symbol_hasInstance",
    "Symbol_species",
    "Symbol_unscopables",
    "Symbol_asyncIterator",
];

/// Build-time replica of `Atom::calc_atom_str_hash`.
/// kind_num matches the runtime `AtomKind` repr: String=1, Symbol=3.
/// Produces the entry `text_hash` (content hash) that seeds the hash lookup.
fn calc_atom_str_hash(str: &str, kind_num: u32) -> u32 {
    let mut h = kind_num;
    if !str.is_empty() {
        for c in str.chars() {
            h = h.wrapping_mul(263).wrapping_add(c as u32);
        }
    }
    h
}

/// AtomKind repr number for a const atom name (String=1, Symbol=3).
fn kind_num_of(name: &str) -> u32 {
    if name.starts_with("Symbol_") || name.eq("Private_brand") {
        3
    } else {
        1
    }
}

pub fn gen_atoms(outfile: &Path) {
    let text = ATOM_DESCR_DEFS
        .iter()
        .map(|t| match *t {
            "empty_string" => "".to_string(),
            "Private_brand" => "<brand>".to_string(),
            s if s.starts_with("Symbol_") => {
                format!("Symbol.{}", &s[7..])
            }
            s => s.to_string(),
        })
        .collect::<Vec<_>>();

    let mut output = Vec::<String>::with_capacity(1024);
    let mut consts = Vec::<String>::with_capacity(1024);

    //output.push("use crate::atom::Atom;\n\n".to_string());

    for (i, txt) in ATOM_DESCR_DEFS.iter().enumerate() {
        let label = if txt.len() > 2
            && ((txt.starts_with("<") && txt.ends_with(">"))
                || (txt.starts_with("*") && txt.ends_with("*")))
        {
            // <XXX> or *XXX*
            format!("_{}_", &txt[1..txt.len() - 1])
        } else if txt.starts_with("#") {
            // #XXX
            format!("hash_{}", txt.strip_prefix('#').unwrap())
        } else if txt.eq(&"*") {
            "_star_".to_string()
        } else if txt.contains('.') {
            // aaa.bbb to aaa_bbb
            txt.replace('.', "_")
        } else {
            txt.to_string()
        };

        consts.push(format!("pub const CONST_{label}: Self = Self({});", i + 1));
    }

    assert_eq!(text.len(), ATOM_DESCR_DEFS.len());

    output.push("".into());

    output.push(format!(
        "pub const ATOM_CONST_COUNT: u32 = {};",
        ATOM_DESCR_DEFS.len()
    ));

    // const atom content table, indexed by (atom_id - 1).
    // Fields:
    // - .0 text:      const atom content ("" / "<brand>" / "Symbol.XXX").
    // - .1 kind:      const atom kind. `Symbol_*` (well-known symbols) and
    //                 `Private_brand` (the shared brand storage key) get
    //                 `AtomKind::Symbol`; everything else is `AtomKind::String`.
    // - .2 text_hash: build-time content hash (`calc_atom_str_hash`), used to
    //                 seed the runtime hash_lut so content lookup lands on the
    //                 const atom. NOT the node `atom_hash`: symbol descriptor
    //                 nodes carry the `JS_ATOM_HASH_SYMBOL`/`JS_ATOM_HASH_PRIVATE`
    //                 marker instead (see `RuntimeImpl::const_atom_value`).
    let const_defs = ATOM_DESCR_DEFS
        .iter()
        .zip(&text)
        .map(|(name, txt)| {
            let (kind_str, kind_num) = if name.starts_with("Symbol_") || name.eq(&"Private_brand") {
                ("AtomKind::Symbol", 3u32)
            } else {
                ("AtomKind::String", 1u32)
            };
            let text_hash = calc_atom_str_hash(txt, kind_num);
            format!(
                "(\"{}\", {kind_str}, {text_hash}),",
                txt.replace('\\', "\\\\").replace('"', "\\\""),
            )
        })
        .collect::<String>();

    output.push(
            "// ATOM_CONST_DEFS, indexed by (atom_id - 1). Fields:\n\
             //   .0 text:      const atom content (\"\" / \"<brand>\" / \"Symbol.XXX\").\n\
             //   .1 kind:      AtomKind::Symbol for Symbol_* & Private_brand, else String.\n\
             //   .2 text_hash: content hash (`calc_atom_str_hash`), building the\n\
             //                 ATOM_CONST_HASH_TABLE lookup index below.\n\
             //                 Symbol node atom_hash is JS_ATOM_HASH_SYMBOL / JS_ATOM_HASH_PRIVATE\n\
             //                 instead (see RuntimeImpl::const_atom_value)."
                .to_string(),
        );

    output.push(format!(
        "pub const ATOM_CONST_DEFS: &[(&str, AtomKind, u32)] = &[{const_defs}];"
    ));

    // const atom content hash index: (text_hash, atom_id) sorted by text_hash.
    // `Atom::lookup_const` binary-searches it so content lookup resolves a
    // const atom with no runtime registration (no hash_lut seeding, no
    // descriptor nodes).
    let mut hash_index = ATOM_DESCR_DEFS
        .iter()
        .zip(&text)
        .enumerate()
        .map(|(idx, (name, txt))| (calc_atom_str_hash(txt, kind_num_of(name)), idx as u32 + 1))
        .collect::<Vec<_>>();
    hash_index.sort_unstable_by_key(|t| t.0);
    let hash_table = hash_index
        .iter()
        .map(|(h, id)| format!("({h}, {id}),"))
        .collect::<String>();
    output.push(format!(
        "pub const ATOM_CONST_HASH_TABLE: &[(u32, u32)] = &[{hash_table}];"
    ));

    // atom consts
    output.push(format!("\n\nimpl Atom {{ {} }}\n\n", consts.join("\n")));

    // Symbol_XXX id ranges
    output.push(format!(
        "pub const ATOM_CONST_SYMBOL_RANGE: core::ops::RangeInclusive<u32> = Atom::CONST_{}.id()..=Atom::CONST_{}.id();",
        ATOM_DESCR_DEFS
            .iter()
            .find(|x| x.starts_with("Symbol_"))
            .unwrap()
             ,
        ATOM_DESCR_DEFS
            .iter()
            .rev()
            .find(|  x | x.starts_with("Symbol_"))
            .unwrap()
    ));

    // keyword id ranges
    output.push(
        "pub const ATOM_CONST_STRICT_KEYWORD_RANGE: core::ops::RangeInclusive<u32> = Atom::CONST_implements.id()..=Atom::CONST_yield.id();".to_string()
    );
    output.push(
        "pub const ATOM_CONST_KEYWORD_RANGE: core::ops::RangeInclusive<u32> = 1..=Atom::CONST_super.id();".to_string()
    );

    std::fs::write(outfile, output.join("\n")).unwrap();

    let o = std::process::Command::new("rustfmt")
        .arg("--")
        .arg(outfile)
        .output()
        .expect("Failed to execute rustfmt");
    if !o.status.success() {
        eprintln!("rustfmt failed: {}", String::from_utf8_lossy(&o.stderr));
        std::process::exit(1);
    }
}
