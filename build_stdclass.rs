// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{collections::HashMap, path::Path};

use once_cell::sync::Lazy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
enum PayloadKey {
    None,
    Array,
    String,
    Bool,
    Value,
    #[cfg(feature = "builtin_date")]
    Datetime,
    NativeFunc,
    BcFunction,
    BoundFunction,
    #[cfg(feature = "builtin_regexp")]
    RegExp,
    ForInIterator,
    ArrayBuffer,
    TypedArray,
    DataView,
    #[cfg(feature = "builtin_mapset")]
    MapSet,
    #[cfg(feature = "builtin_mapset")]
    MapSetIterator,
    ArrayIterator,
    #[cfg(feature = "builtin_regexp")]
    RegExpStringIterator,
    Generator,
    #[cfg(feature = "builtin_proxy")]
    Proxy,
    Promise,
    PromiseResolvingFunction,
    AsyncFunctionResolving,
    AsyncFromSyncIterator,
    AsyncGenerator,
}

impl PayloadKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            PayloadKey::None => "",
            PayloadKey::Array => "array",
            PayloadKey::String => "string",
            PayloadKey::Bool => "bool",
            PayloadKey::Value => "value",
            #[cfg(feature = "builtin_date")]
            PayloadKey::Datetime => "datetime",
            PayloadKey::NativeFunc => "native_func",
            PayloadKey::BcFunction => "bc_function",
            PayloadKey::BoundFunction => "bound_function",
            #[cfg(feature = "builtin_regexp")]
            PayloadKey::RegExp => "regexp",
            PayloadKey::ForInIterator => "for_in_iterator",
            PayloadKey::ArrayBuffer => "array_buffer",
            PayloadKey::TypedArray => "typed_array",
            PayloadKey::DataView => "data_view",
            #[cfg(feature = "builtin_mapset")]
            PayloadKey::MapSet => "mapset",
            #[cfg(feature = "builtin_mapset")]
            PayloadKey::MapSetIterator => "mapset_iterator",
            PayloadKey::ArrayIterator => "array_iterator",
            #[cfg(feature = "builtin_regexp")]
            PayloadKey::RegExpStringIterator => "regexp_string_iterator",
            PayloadKey::Generator => "generator",
            #[cfg(feature = "builtin_proxy")]
            PayloadKey::Proxy => "proxy",
            PayloadKey::Promise => "promise",
            PayloadKey::PromiseResolvingFunction => "promise_resolving_function",
            PayloadKey::AsyncFunctionResolving => "async_function_resolving",
            PayloadKey::AsyncFromSyncIterator => "async_from_sync_iterator",
            PayloadKey::AsyncGenerator => "async_generator",
        }
    }
}

impl std::fmt::Display for PayloadKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

static CLASS_PAYLOAD_TYPE: Lazy<HashMap<PayloadKey, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::with_capacity(128);

    m.insert(PayloadKey::Array, "crate::array::ArrayPayload");
    m.insert(PayloadKey::Bool, "bool");
    m.insert(PayloadKey::String, "crate::string::StringRef");
    m.insert(PayloadKey::Value, "crate::value::JSValue");
    m.insert(PayloadKey::NativeFunc, "Box<crate::function::NFuncPayload>");
    m.insert(PayloadKey::BcFunction, "Box<crate::function::BFuncPayload>");
    m.insert(
        PayloadKey::BoundFunction,
        "Box<crate::function::BoundFuncObjPayload>",
    );
    #[cfg(feature = "builtin_regexp")]
    m.insert(
        PayloadKey::RegExp,
        "Box<crate::builtins::regexp::RegExpData>",
    );
    m.insert(
        PayloadKey::ForInIterator,
        "Box<crate::builtins::iterator::ForInIterPayload>",
    );
    m.insert(
        PayloadKey::ArrayBuffer,
        "Box<crate::builtins::array_buffer::ArrayBufferPayload>",
    );
    m.insert(
        PayloadKey::TypedArray,
        "Box<crate::builtins::typed_array::TypedArrayPayload>",
    );
    m.insert(
        PayloadKey::DataView,
        "Box<crate::builtins::typed_array::DataViewPayload>",
    );
    #[cfg(feature = "builtin_mapset")]
    m.insert(
        PayloadKey::MapSet,
        "Box<crate::builtins::mapset::JSMapState>",
    );
    #[cfg(feature = "builtin_mapset")]
    m.insert(
        PayloadKey::MapSetIterator,
        "Box<crate::builtins::mapset::JSMapIteratorData>",
    );
    m.insert(
        PayloadKey::ArrayIterator,
        "Box<crate::builtins::iterator::ArrayIterPayload>",
    );
    #[cfg(feature = "builtin_regexp")]
    m.insert(
        PayloadKey::RegExpStringIterator,
        "Box<crate::builtins::regexp::RegExpStringIterPayload>",
    );
    m.insert(
        PayloadKey::Generator,
        "Box<crate::builtins::generator::GeneratorPayload>",
    );
    #[cfg(feature = "builtin_proxy")]
    m.insert(
        PayloadKey::Proxy,
        "Box<crate::builtins::proxy::ProxyPayload>",
    );
    m.insert(
        PayloadKey::Promise,
        "Box<crate::builtins::promise::PromiseData>",
    );
    m.insert(
        PayloadKey::PromiseResolvingFunction,
        "Box<crate::builtins::promise::PromiseResolvePayload>",
    );
    m.insert(
        PayloadKey::AsyncFunctionResolving,
        "crate::function::AsyncState",
    );
    m.insert(
        PayloadKey::AsyncFromSyncIterator,
        "Box<crate::builtins::iterator::JSAsyncFromSyncIteratorData>",
    );
    m.insert(
        PayloadKey::AsyncGenerator,
        "Box<crate::builtins::generator::AGenPayload>",
    );

    #[cfg(feature = "builtin_date")]
    m.insert(PayloadKey::Datetime, "crate::builtins::date::Datetime");

    m
});

#[derive(Debug)]
struct ClassInfo {
    class_name: &'static str,
    const_name: &'static str,
    class_def: &'static str,
    key: PayloadKey,
}

impl ClassInfo {
    const fn class_def(
        class_name: &'static str,
        const_name: &'static str,
        class_def: &'static str,
        key: PayloadKey,
    ) -> Self {
        ClassInfo {
            class_name,
            const_name,
            class_def,
            key,
        }
    }
}

const CLASS_DEF_LIST: &[ClassInfo] = &[
    // CORE CLASSES START
    ClassInfo::class_def(
        "Object",
        "OBJECT",
        "crate::builtins::object::CLASS_DEF_OBJECT",
        PayloadKey::None,
    ),
    ClassInfo::class_def(
        "Array",
        "ARRAY",
        "crate::builtins::array::CLASS_DEF_ARRAY",
        PayloadKey::Array,
    ),
    ClassInfo::class_def(
        "Error",
        "ERROR",
        "crate::builtins::error::CLASS_DEF_ERROR",
        PayloadKey::None,
    ),
    ClassInfo::class_def(
        "Number",
        "NUMBER",
        "crate::builtins::number::CLASS_DEF_NUMBER",
        PayloadKey::Value,
    ),
    ClassInfo::class_def(
        "Boolean",
        "BOOLEAN",
        "crate::builtins::boolean::CLASS_DEF_BOOLEAN",
        PayloadKey::Bool,
    ),
    ClassInfo::class_def(
        "String",
        "STRING",
        "crate::builtins::string::CLASS_DEF_STRING",
        PayloadKey::String,
    ),
    #[cfg(feature = "builtin_symbol")]
    ClassInfo::class_def(
        "Symbol",
        "SYMBOL",
        "crate::builtins::symbol::CLASS_DEF_SYMBOL",
        PayloadKey::String,
    ),
    ClassInfo::class_def(
        "Arguments",
        "ARGUMENTS",
        "crate::builtins::arguments::CLASS_DEF_ARGUMENTS",
        PayloadKey::Array,
    ),
    ClassInfo::class_def(
        "Arguments",
        "MAPPED_ARGUMENTS",
        "crate::builtins::arguments::CLASS_DEF_MAPPED_ARGUMENTS",
        PayloadKey::None,
    ),
    #[cfg(feature = "builtin_date")]
    ClassInfo::class_def(
        "Date",
        "DATE",
        "crate::builtins::date::CLASS_DEF_DATE",
        PayloadKey::Datetime,
    ),
    // CORE CLASSES END
    ClassInfo::class_def(
        "Object",
        "MODULE_NS",
        "crate::builtins::CLASS_DEF_MODULE_NS",
        PayloadKey::None,
    ),
    ClassInfo::class_def(
        "Function",
        "C_FUNCTION",
        "crate::builtins::function::CLASS_DEF_C_FUNCTION",
        PayloadKey::NativeFunc,
    ),
    ClassInfo::class_def(
        "Function",
        "BYTECODE_FUNCTION",
        "crate::builtins::function::CLASS_DEF_BYTECODE_FUNCTION",
        PayloadKey::BcFunction,
    ),
    ClassInfo::class_def(
        "Function",
        "BOUND_FUNCTION",
        "crate::builtins::function::CLASS_DEF_BOUND_FUNCTION",
        PayloadKey::BoundFunction,
    ),
    ClassInfo::class_def(
        "Function",
        "C_FUNCTION_DATA",
        "crate::builtins::function::CLASS_DEF_C_FUNCTION_DATA",
        PayloadKey::NativeFunc,
    ),
    ClassInfo::class_def(
        "ForInIterPayload",
        "FOR_IN_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_FOR_IN_ITERATOR",
        PayloadKey::ForInIterator,
    ),
    #[cfg(feature = "builtin_regexp")]
    ClassInfo::class_def(
        "RegExp",
        "REGEXP",
        "crate::builtins::regexp::CLASS_DEF_REGEXP",
        PayloadKey::RegExp,
    ),
    ClassInfo::class_def(
        "ArrayBuffer",
        "ARRAY_BUFFER",
        "crate::builtins::array_buffer::CLASS_DEF_ARRAY_BUFFER",
        PayloadKey::ArrayBuffer,
    ),
    ClassInfo::class_def(
        "SharedArrayBuffer",
        "SHARED_ARRAY_BUFFER",
        "crate::builtins::array_buffer::CLASS_DEF_SHARED_ARRAY_BUFFER",
        PayloadKey::ArrayBuffer,
    ),
    //
    // start of typed arrays
    //
    ClassInfo::class_def(
        "Uint8ClampedArray",
        "UINT8C_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT8C_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Int8Array",
        "INT8_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT8_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Uint8Array",
        "UINT8_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT8_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Int16Array",
        "INT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT16_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Uint16Array",
        "UINT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT16_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Int32Array",
        "INT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT32_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Uint32Array",
        "UINT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT32_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "BigInt64Array",
        "BIG_INT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_BIG_INT64_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "BigUint64Array",
        "BIG_UINT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_BIG_UINT64_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Float32Array",
        "FLOAT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT32_ARRAY",
        PayloadKey::TypedArray,
    ),
    ClassInfo::class_def(
        "Float64Array",
        "FLOAT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT64_ARRAY",
        PayloadKey::TypedArray,
    ),
    // end of typed arrays
    ClassInfo::class_def(
        "DataView",
        "DATAVIEW",
        "crate::builtins::typed_array::CLASS_DEF_DATAVIEW",
        PayloadKey::DataView,
    ),
    ClassInfo::class_def(
        "BigInt",
        "BIG_INT",
        "crate::builtins::CLASS_DEF_BIG_INT",
        PayloadKey::None,
    ),
    //
    // map set and iterators
    //
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "Map",
        "MAP",
        "crate::builtins::mapset::CLASS_DEF_MAP",
        PayloadKey::MapSet,
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "Set",
        "SET",
        "crate::builtins::mapset::CLASS_DEF_SET",
        PayloadKey::MapSet,
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "WeakMap",
        "WEAKMAP",
        "crate::builtins::mapset::CLASS_DEF_WEAK_MAP",
        PayloadKey::MapSet,
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "WeakSet",
        "WEAKSET",
        "crate::builtins::mapset::CLASS_DEF_WEAK_SET",
        PayloadKey::MapSet,
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "Map_Iterator",
        "MAP_ITERATOR",
        "crate::builtins::mapset::CLASS_DEF_MAP_ITERATOR",
        PayloadKey::MapSetIterator,
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "Set_Iterator",
        "SET_ITERATOR",
        "crate::builtins::mapset::CLASS_DEF_SET_ITERATOR",
        PayloadKey::MapSetIterator,
    ),
    ClassInfo::class_def(
        "Array_Iterator",
        "ARRAY_ITERATOR",
        "crate::builtins::array::CLASS_DEF_ARRAY_ITERATOR",
        PayloadKey::ArrayIterator,
    ),
    ClassInfo::class_def(
        "String_Iterator",
        "STRING_ITERATOR",
        "crate::builtins::string::CLASS_DEF_STRING_ITERATOR",
        PayloadKey::ArrayIterator,
    ),
    #[cfg(feature = "builtin_regexp")]
    ClassInfo::class_def(
        "RegExp_String_Iterator",
        "REGEXP_STRING_ITERATOR",
        "crate::builtins::regexp::CLASS_DEF_REGEXP_STRING_ITERATOR",
        PayloadKey::RegExpStringIterator,
    ),
    //
    //
    //
    ClassInfo::class_def(
        "Generator",
        "GENERATOR",
        "crate::builtins::generator::CLASS_DEF_GENERATOR",
        PayloadKey::Generator,
    ),
    ClassInfo::class_def(
        "GeneratorFunction",
        "GENERATOR_FUNCTION",
        "crate::builtins::generator::CLASS_DEF_GENERATOR_FUNCTION",
        PayloadKey::BcFunction,
    ),
    //
    // Proxy
    //
    #[cfg(feature = "builtin_proxy")]
    ClassInfo::class_def(
        "Object", // Proxy class name is Object
        "PROXY",
        "crate::builtins::proxy::CLASS_DEF_PROXY",
        PayloadKey::Proxy,
    ),
    //
    // Async stuff
    //
    ClassInfo::class_def(
        "Promise",
        "PROMISE",
        "crate::builtins::promise::CLASS_DEF_PROMISE",
        PayloadKey::Promise,
    ),
    ClassInfo::class_def(
        "PromiseResolveFunction",
        "PROMISE_RESOLVE_FUNCTION",
        "crate::builtins::promise::CLASS_DEF_PROMISE_RESOLVE_FUNCTION",
        PayloadKey::PromiseResolvingFunction,
    ),
    ClassInfo::class_def(
        "PromiseRejectFunction",
        "PROMISE_REJECT_FUNCTION",
        "crate::builtins::promise::CLASS_DEF_PROMISE_REJECT_FUNCTION",
        PayloadKey::PromiseResolvingFunction,
    ),
    ClassInfo::class_def(
        "AsyncFunction",
        "ASYNC_FUNCTION",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION",
        PayloadKey::BcFunction,
    ),
    ClassInfo::class_def(
        "AsyncFunctionResolve",
        "ASYNC_FUNCTION_RESOLVE",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION_RESOLVE",
        PayloadKey::AsyncFunctionResolving,
    ),
    ClassInfo::class_def(
        "AsyncFunctionReject",
        "ASYNC_FUNCTION_REJECT",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION_REJECT",
        PayloadKey::AsyncFunctionResolving,
    ),
    ClassInfo::class_def(
        "empty_string", // JS_ATOM_empty_string
        "ASYNC_FROM_SYNC_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR",
        PayloadKey::AsyncFromSyncIterator,
    ),
    ClassInfo::class_def(
        "AsyncGenerator",
        "ASYNC_GENERATOR",
        "crate::builtins::generator::CLASS_DEF_ASYNC_GENERATOR",
        PayloadKey::AsyncGenerator,
    ),
    ClassInfo::class_def(
        "AsyncGeneratorFunction",
        "ASYNC_GENERATOR_FUNCTION", // depends on ASYNC_GENERATOR
        "crate::builtins::generator::CLASS_DEF_ASYNC_GENERATOR_FUNCTION",
        PayloadKey::BcFunction,
    ),
];

pub fn gen_std_class_defs(outfile: &Path) {
    let mut cdefs = Vec::<String>::with_capacity(128);
    let mut consts = Vec::<String>::with_capacity(128);

    let mut async_start: Option<usize> = None;
    let mut async_end: Option<usize> = None;

    // builtin class defs
    //
    cdefs.push("pub(crate) const BUILTIN_CLASS_DEFS: &[ClassDef] = &[".to_string());

    for (i, cdef) in CLASS_DEF_LIST.iter().enumerate() {
        let name = cdef.class_name;

        match cdef.const_name {
            "PROMISE" => {
                async_start = Some(i);
            }
            "ASYNC_GENERATOR" => {
                async_end = Some(i);
            }
            _ => {}
        }

        cdefs.push(format!(
            "// #{i}: id={} {}\n{},",
            i + 1,
            cdef.const_name,
            cdef.class_def
        ));

        consts.push(format!(
            "pub const JS_CLASS_{}:ClassID={};",
            cdef.const_name,
            i + 1
        ));
    }

    consts.push(format!(
        "pub const USER_CLASS_ID_START: ClassID={};",
        CLASS_DEF_LIST.len() + 1
    ));

    cdefs.push("];".to_string());

    let lines = [
        "use crate::{Atom, class::{ClassID, ClassDef}};\n".to_string(),
        consts.join("\n"),
        "".to_string(),
        cdefs.join("\n"),
        "// end of std class".to_string(),
        format!(
            "pub const STD_CLASS_DEFS_ASYNC_INDEX_RANGE: std::ops::RangeInclusive<usize> = {}..={};",
            async_start.unwrap(),
            async_end.unwrap()
        ),
    ];

    std::fs::write(outfile, lines.join("\n")).unwrap();

    let output = std::process::Command::new("rustfmt")
        .arg("--")
        .arg(outfile)
        .output()
        .expect("Failed to execute rustfmt");

    if !output.status.success() {
        eprintln!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }
}

pub fn gen_class_payload(outfile: &Path) {
    let mut items = Vec::<String>::with_capacity(CLASS_DEF_LIST.len() + 10);
    let mut getid_arms = Vec::<String>::with_capacity(CLASS_DEF_LIST.len() + 8);

    for cdef in CLASS_DEF_LIST {
        items.push(if cdef.key != PayloadKey::None {
            format!("{}({})", cdef.const_name, CLASS_PAYLOAD_TYPE[&cdef.key])
        } else {
            cdef.const_name.to_string()
        });

        getid_arms.push(if cdef.key != PayloadKey::None {
            format!(
                "Self::{}(_)=>crate::JS_CLASS_{}",
                cdef.const_name, cdef.const_name
            )
        } else {
            format!(
                "Self::{}=>crate::JS_CLASS_{}",
                cdef.const_name, cdef.const_name
            )
        });
    }

    items.push("OTHER(Box<crate::class::OtherPayload>)".to_string());
    getid_arms.push("Self::OTHER(u)=>u.class_id".to_string());

    // get payload methods
    let mut get_payload_methods = Vec::<String>::with_capacity(CLASS_DEF_LIST.len() + 8);
    let mut gc_trace_arms = Vec::<String>::with_capacity(CLASS_DEF_LIST.len() + 8);

    let mut kvs = CLASS_PAYLOAD_TYPE.iter().collect::<Vec<_>>();
    kvs.sort();
    for (k, pty) in kvs {
        let arms = CLASS_DEF_LIST
            .iter()
            .filter_map(|c| {
                if c.key.eq(k) {
                    Some(format!("Self::{}(x)", c.const_name))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if !arms.is_empty() {
            let ty = if pty.starts_with("Box<") && pty.ends_with(">") {
                &pty[4..pty.len() - 1]
            } else {
                pty
            };

            get_payload_methods.push(
                format!("pub const fn {k}(&self)->Option<&{ty}> {{ if let {} = self {{ Some(x) }} else {{ None }} }}",
                arms.join("|")
            ));
            get_payload_methods.push(
                format!("pub const fn {k}_mut(&mut self)->Option<&mut {ty}> {{ if let {} = self {{ Some(x) }} else {{ None }} }}",
                arms.join("|")
            ));

            gc_trace_arms.push(format!("{} => {{ x.trace(tr); }}", arms.join("|")));
        }
    }

    get_payload_methods.push("pub const fn other(&self)->Option<&crate::class::OtherPayload> { if let Self::OTHER(o)=self {Some(o)} else{None} }".to_string());
    get_payload_methods.push("pub const fn other_mut(&mut self)->Option<&mut crate::class::OtherPayload> { if let Self::OTHER(o)=self {Some(o)} else{None} }".to_string());

    let lines: Vec<String> = vec![
        "use crate::{  class::{ClassID }};\n".to_string(),
        // ObjectPayload
        "#[allow(clippy::upper_case_acronyms)]\n#[derive(Debug)]\n#[repr(u8)]\npub enum ObjectPayload {\n".to_string(),
        items.join(",\n"),
        "}".to_string(),
        "".to_string(),
        // impl
        "impl ObjectPayload {".to_string(),
        // class_id()
        format!(
            "pub const fn class_id(&self)->ClassID {{ match self {{ {} }} }}",
            getid_arms.join(",\n"),
        ),
        "\n".into(),
        // get payload methods
        get_payload_methods.join("\n\n"),
        "}".to_string(),
        // impl collect trait
        "\n\n".to_string(),
        format!(
            r#"impl gc_lite::GcTrace for ObjectPayload {{
                fn trace(&self, tr: &mut gc_lite::GcTraceCtx) {{
                    match self {{
                        {},
                        _=>{{ }}
                    }}
                }}
            }}"#,
            gc_trace_arms.join(",\n")
        ),
    ];

    std::fs::write(outfile, lines.join("\n")).unwrap();

    let output = std::process::Command::new("rustfmt")
        .arg("--")
        .arg(outfile)
        .output()
        .expect("Failed to execute rustfmt");

    if !output.status.success() {
        eprintln!(
            "rustfmt failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::process::exit(1);
    }
}
