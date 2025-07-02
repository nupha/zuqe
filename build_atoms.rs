use std::path::Path;

static ATOM_DEFS: &[&str] = &[
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
    "computed_field",
    "static_computed_field",
    "class_fields_init",
    "brand",
    "hash_constructor",
    "as",
    "from",
    "meta",
    "<default>",
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
    #[cfg(feature = "atomics")]
    "not_equal",
    #[cfg(feature = "atomics")]
    "timed_out",
    #[cfg(feature = "atomics")]
    "ok",
    //
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
    "Math",
    "JSON",
    "Date",
    "Function",
    "GeneratorFunction",
    "ForInIterator",
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
    "Float32Array",
    "Float64Array",
    "DataView",
    "BigInt",
    //
    "Map",
    "Set",     // Map+1
    "WeakMap", // Map+2
    "WeakSet", // Map+3
    "Map_Iterator",
    "Set_Iterator",
    "Array_Iterator",
    "String_Iterator",
    "RegExp_String_Iterator",
    "Generator",
    "Proxy",
    "Promise",
    "PromiseResolveFunction",
    "PromiseRejectFunction",
    "AsyncFunction",
    "AsyncFunctionResolve",
    "AsyncFunctionReject",
    "AsyncGeneratorFunction",
    "AsyncGeneratorValue",
    "EvalError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "TypeError",
    "URIError",
    "InternalError",
    //
    // private symbols
    //
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

pub fn gen_atoms(outfile: &Path) {
    let text = ATOM_DEFS
        .iter()
        .map(|t| match *t {
            "empty_string" => "".to_string(),
            s if s.starts_with("Symbol_") => {
                format!("Symbol.{}", &s[7..])
            }
            s => s.to_string(),
        })
        .collect::<Vec<_>>();

    let full_cstr_buf = text.iter().map(|t| format!("{t}\\0")).collect::<String>();

    let mut output = Vec::<String>::with_capacity(1024);

    output.push(format!(
        "pub const JS_DEFAULT_ATOMS_TEXT: &[u8] = b\"{full_cstr_buf}\";"
    ));

    // lines.push(format!("pub static js_atom_init: &[&[u8]] = &["));
    // for (i, txt) in verbose.iter().enumerate() {
    //     lines.push(format!("b\"{txt}\\0\".as_slice(),"))
    // }
    // lines.push("];".into());

    for (i, txt) in ATOM_DEFS.iter().enumerate() {
        let label = if txt.len() > 2 && txt.starts_with("<") && txt.ends_with(">") {
            format!("_{}_", &txt[1..txt.len() - 1])
        } else if txt.eq(&"*") {
            "_star_".to_string()
        } else if txt.contains('.') {
            txt.replace('.', "_")
        } else {
            txt.to_string()
        };
        output.push(format!(
            "pub const JS_ATOM_{label}: JSAtom = JSAtom({});",
            i + 1
        ))
    }

    output.push("".into());
    output.push(format!(
        "pub const JS_ATOM_INIT_COUNT:u32={};",
        ATOM_DEFS.len() + 1
    ));
    output.push("pub const JS_ATOM_FIRST: JSAtom = JS_ATOM_null;".into());
    output.push("pub const JS_ATOM_LAST_KEYWORD: JSAtom = JS_ATOM_super;".into());
    output.push("pub const JS_ATOM_FIRST_STRICT_KEYWORD: JSAtom = JS_ATOM_implements;".into());
    output.push("pub const JS_ATOM_LAST_STRICT_KEYWORD: JSAtom = JS_ATOM_yield;".into());

    std::fs::write(outfile, output.join("\n")).unwrap();
}
