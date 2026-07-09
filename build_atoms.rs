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
    #[cfg(feature = "builtin_symbol")]
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
    // privates
    //
    #[cfg(feature = "builtin_symbol")]
    "Private_brand",
    //
    // symbols
    //
    #[cfg(feature = "builtin_symbol")]
    "Symbol_toPrimitive",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_iterator",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_match",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_matchAll",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_replace",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_search",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_split",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_toStringTag",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_isConcatSpreadable",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_hasInstance",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_species",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_unscopables",
    #[cfg(feature = "builtin_symbol")]
    "Symbol_asyncIterator",
];

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
        "pub const ATOM_CONST_COUNT:u32={};\npub const ATOM_USER_BASE_ID:u32={};",
        ATOM_DESCR_DEFS.len(),
        ATOM_DESCR_DEFS.len() + 1
    ));

    // output.push(format!(
    //     "/// the first builtin const atom\npub const JS_ATOM_FIRST: Atom = Atom::CONST_{};",
    //     ATOM_DESCR_DEFS.first().unwrap()
    // ));
    // output.push(format!(
    //     "/// the last builtin const atom\npub const JS_ATOM_LAST: Atom = Atom::CONST_{};",
    //     ATOM_DESCR_DEFS.last().unwrap()
    // ));

    // output.push("pub const JS_ATOM_LAST_KEYWORD: Atom = Atom::CONST_super;".into());
    // output.push("pub const JS_ATOM_FIRST_STRICT_KEYWORD: Atom = Atom::CONST_implements;".into());
    // output.push("pub const JS_ATOM_LAST_STRICT_KEYWORD: Atom = Atom::CONST_yield;".into());

    // Symbol_XXX
    // output.push(format!(
    //     "/// the first builtin const Symbol_XXX atom\npub const JS_ATOM_FIRST_SYMBOL: Atom = Atom::CONST_{};",
    //     ATOM_DESCR_DEFS.iter().find(|x|x.starts_with("Symbol_")).unwrap()
    // ));
    // output.push(format!(
    //     "/// the last builtin const Symbol_XXX atom\npub const JS_ATOM_LAST_SYMBOL: Atom = Atom::CONST_{};",
    //     ATOM_DESCR_DEFS.iter().rev().find(|x|x.starts_with("Symbol_")).unwrap()
    // ));

    // full atom text data
    let full_cstr_buf = text.iter().map(|t| format!("{t}\\0")).collect::<String>();
    output.push(format!(
        "\n\npub static ATOM_CONST_CAPTIONS: &[u8] = b\"{full_cstr_buf}\";"
    ));

    // atom consts
    output.push(format!("\n\nimpl Atom {{ {} }}\n\n", consts.join("\n")));

    // Symbol_XXX id ranges
    #[cfg(feature = "builtin_symbol")]
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
