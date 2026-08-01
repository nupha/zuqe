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
    AsyncIterableIterator,
    IteratorHelper,
    IteratorWrap,
    IteratorConcat,
    IteratorZip,
    #[cfg(feature = "builtin_weakref")]
    WeakRef,
    #[cfg(feature = "builtin_weakref")]
    FinalizationRegistry,
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
            PayloadKey::AsyncIterableIterator => "async_iterator",
            PayloadKey::IteratorHelper => "iterator_helper",
            PayloadKey::IteratorWrap => "iterator_wrap",
            PayloadKey::IteratorConcat => "iterator_concat",
            PayloadKey::IteratorZip => "iterator_zip",
            #[cfg(feature = "builtin_weakref")]
            PayloadKey::WeakRef => "weak_ref",
            #[cfg(feature = "builtin_weakref")]
            PayloadKey::FinalizationRegistry => "finalization_registry",
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
        "Box<crate::function::BoundFunctionPayload>",
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
        "Box<crate::builtins::promise::PromisePayload>",
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
    m.insert(
        PayloadKey::AsyncIterableIterator,
        "Box<crate::builtins::iterator::AsyncIterPayload>",
    );

    m.insert(
        PayloadKey::IteratorHelper,
        "Box<crate::builtins::iterator::IteratorHelperData>",
    );
    m.insert(
        PayloadKey::IteratorWrap,
        "Box<crate::builtins::iterator::IteratorWrapData>",
    );
    m.insert(
        PayloadKey::IteratorConcat,
        "Box<crate::builtins::iterator::IteratorConcatData>",
    );
    m.insert(
        PayloadKey::IteratorZip,
        "Box<crate::builtins::iterator::IteratorZipData>",
    );

    #[cfg(feature = "builtin_date")]
    m.insert(PayloadKey::Datetime, "crate::builtins::date::Datetime");

    #[cfg(feature = "builtin_weakref")]
    m.insert(
        PayloadKey::WeakRef,
        "Box<crate::builtins::weakref::WeakRefData>",
    );
    #[cfg(feature = "builtin_weakref")]
    m.insert(
        PayloadKey::FinalizationRegistry,
        "Box<crate::builtins::weakref::FinalizationRegistryData>",
    );

    m
});

#[derive(Debug)]
struct ClassInfo {
    const_name: &'static str,
    class_def: &'static str,
    key: PayloadKey,
    /// narrow object wrapper type this class maps to (name-matched, at most
    /// one). `None` = this class does not participate in any narrow type.
    /// The narrow types are aggregated from `CLASS_DEF_LIST` by this field.
    narrow: Option<&'static str>,
}

impl ClassInfo {
    const fn class_def(const_name: &'static str, class_def: &'static str, key: PayloadKey) -> Self {
        ClassInfo {
            const_name,
            class_def,
            key,
            narrow: None,
        }
    }

    const fn narrow_type(
        const_name: &'static str,
        class_def: &'static str,
        key: PayloadKey,
        narrow: &'static str,
    ) -> Self {
        ClassInfo {
            const_name,
            class_def,
            key,
            narrow: Some(narrow),
        }
    }
}

const CLASS_DEF_LIST: &[ClassInfo] = &[
    // CORE CLASSES START
    ClassInfo::class_def(
        "OBJECT",
        "crate::builtins::object::CLASS_DEF_OBJECT",
        PayloadKey::None,
    ),
    ClassInfo::narrow_type(
        "ARRAY",
        "crate::builtins::array::CLASS_DEF_ARRAY",
        PayloadKey::Array,
        "Array",
    ),
    ClassInfo::class_def(
        "ERROR",
        "crate::builtins::error::CLASS_DEF_ERROR",
        PayloadKey::None,
    ),
    ClassInfo::class_def(
        "NUMBER",
        "crate::builtins::number::CLASS_DEF_NUMBER",
        PayloadKey::Value,
    ),
    ClassInfo::class_def(
        "BOOLEAN",
        "crate::builtins::boolean::CLASS_DEF_BOOLEAN",
        PayloadKey::Bool,
    ),
    ClassInfo::class_def(
        "STRING",
        "crate::builtins::string::CLASS_DEF_STRING",
        PayloadKey::String,
    ),
    ClassInfo::class_def(
        "SYMBOL",
        "crate::builtins::symbol::CLASS_DEF_SYMBOL",
        PayloadKey::String,
    ),
    ClassInfo::narrow_type(
        "ARGUMENTS",
        "crate::builtins::arguments::CLASS_DEF_ARGUMENTS",
        PayloadKey::Array,
        "Array",
    ),
    ClassInfo::class_def(
        "MAPPED_ARGUMENTS",
        "crate::builtins::arguments::CLASS_DEF_MAPPED_ARGUMENTS",
        PayloadKey::None,
    ),
    #[cfg(feature = "builtin_date")]
    ClassInfo::class_def(
        "DATE",
        "crate::builtins::date::CLASS_DEF_DATE",
        PayloadKey::Datetime,
    ),
    // CORE CLASSES END
    ClassInfo::class_def(
        "MODULE_NS",
        "crate::builtins::CLASS_DEF_MODULE_NS",
        PayloadKey::None,
    ),
    ClassInfo::narrow_type(
        "C_FUNCTION",
        "crate::builtins::function::CLASS_DEF_C_FUNCTION",
        PayloadKey::NativeFunc,
        "NativeFunction",
    ),
    ClassInfo::narrow_type(
        "BYTECODE_FUNCTION",
        "crate::builtins::function::CLASS_DEF_BYTECODE_FUNCTION",
        PayloadKey::BcFunction,
        "BytecodeFunction",
    ),
    ClassInfo::narrow_type(
        "BOUND_FUNCTION",
        "crate::builtins::function::CLASS_DEF_BOUND_FUNCTION",
        PayloadKey::BoundFunction,
        "BoundFunction",
    ),
    ClassInfo::narrow_type(
        "C_FUNCTION_DATA",
        "crate::builtins::function::CLASS_DEF_C_FUNCTION_DATA",
        PayloadKey::NativeFunc,
        "NativeFunction",
    ),
    ClassInfo::class_def(
        "FOR_IN_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_FOR_IN_ITERATOR",
        PayloadKey::ForInIterator,
    ),
    #[cfg(feature = "builtin_regexp")]
    ClassInfo::narrow_type(
        "REGEXP",
        "crate::builtins::regexp::CLASS_DEF_REGEXP",
        PayloadKey::RegExp,
        "RegExp",
    ),
    ClassInfo::narrow_type(
        "ARRAY_BUFFER",
        "crate::builtins::array_buffer::CLASS_DEF_ARRAY_BUFFER",
        PayloadKey::ArrayBuffer,
        "ArrayBuffer",
    ),
    ClassInfo::narrow_type(
        "SHARED_ARRAY_BUFFER",
        "crate::builtins::array_buffer::CLASS_DEF_SHARED_ARRAY_BUFFER",
        PayloadKey::ArrayBuffer,
        "ArrayBuffer",
    ),
    //
    // start of typed arrays
    //
    ClassInfo::narrow_type(
        "UINT8C_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT8C_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "INT8_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT8_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "UINT8_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT8_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "INT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT16_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "UINT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT16_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "INT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT32_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "UINT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT32_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "BIG_INT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_BIG_INT64_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "BIG_UINT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_BIG_UINT64_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "FLOAT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT16_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "FLOAT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT32_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    ClassInfo::narrow_type(
        "FLOAT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT64_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    // end of typed arrays
    ClassInfo::narrow_type(
        "DATAVIEW",
        "crate::builtins::typed_array::CLASS_DEF_DATAVIEW",
        PayloadKey::DataView,
        "DataView",
    ),
    ClassInfo::class_def(
        "BIG_INT",
        "crate::builtins::CLASS_DEF_BIG_INT",
        PayloadKey::None,
    ),
    #[cfg(feature = "builtin_json")]
    ClassInfo::class_def(
        "RAWJSON",
        "crate::builtins::json::CLASS_DEF_RAWJSON",
        PayloadKey::None,
    ),
    //
    // map set and iterators
    //
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::narrow_type(
        "MAP",
        "crate::builtins::mapset::CLASS_DEF_MAP",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::narrow_type(
        "SET",
        "crate::builtins::mapset::CLASS_DEF_SET",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::narrow_type(
        "WEAKMAP",
        "crate::builtins::mapset::CLASS_DEF_WEAK_MAP",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::narrow_type(
        "WEAKSET",
        "crate::builtins::mapset::CLASS_DEF_WEAK_SET",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::narrow_type(
        "MAP_ITERATOR",
        "crate::builtins::mapset::CLASS_DEF_MAP_ITERATOR",
        PayloadKey::MapSetIterator,
        "MapIterator",
    ),
    #[cfg(feature = "builtin_mapset")]
    ClassInfo::class_def(
        "SET_ITERATOR",
        "crate::builtins::mapset::CLASS_DEF_SET_ITERATOR",
        PayloadKey::MapSetIterator,
    ),
    ClassInfo::class_def(
        "ARRAY_ITERATOR",
        "crate::builtins::array::CLASS_DEF_ARRAY_ITERATOR",
        PayloadKey::ArrayIterator,
    ),
    ClassInfo::class_def(
        "STRING_ITERATOR",
        "crate::builtins::string::CLASS_DEF_STRING_ITERATOR",
        PayloadKey::ArrayIterator,
    ),
    #[cfg(feature = "builtin_regexp")]
    ClassInfo::class_def(
        "REGEXP_STRING_ITERATOR",
        "crate::builtins::regexp::CLASS_DEF_REGEXP_STRING_ITERATOR",
        PayloadKey::RegExpStringIterator,
    ),
    //
    //
    //
    ClassInfo::class_def(
        "GENERATOR",
        "crate::builtins::generator::CLASS_DEF_GENERATOR",
        PayloadKey::Generator,
    ),
    ClassInfo::narrow_type(
        "GENERATOR_FUNCTION",
        "crate::builtins::generator::CLASS_DEF_GENERATOR_FUNCTION",
        PayloadKey::BcFunction,
        "GeneratorFunction",
    ),
    //
    // Proxy
    //
    #[cfg(feature = "builtin_proxy")]
    ClassInfo::narrow_type(
        "PROXY",
        "crate::builtins::proxy::CLASS_DEF_PROXY",
        PayloadKey::Proxy,
        "Proxy",
    ),
    //
    // Async stuff
    //
    ClassInfo::narrow_type(
        "PROMISE",
        "crate::builtins::promise::CLASS_DEF_PROMISE",
        PayloadKey::Promise,
        "Promise",
    ),
    ClassInfo::narrow_type(
        "PROMISE_RESOLVE_FUNCTION",
        "crate::builtins::promise::CLASS_DEF_PROMISE_RESOLVE_FUNCTION",
        PayloadKey::PromiseResolvingFunction,
        "PromiseResolveFunc",
    ),
    ClassInfo::narrow_type(
        "PROMISE_REJECT_FUNCTION",
        "crate::builtins::promise::CLASS_DEF_PROMISE_REJECT_FUNCTION",
        PayloadKey::PromiseResolvingFunction,
        "PromiseResolveFunc",
    ),
    ClassInfo::narrow_type(
        "ASYNC_FUNCTION",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION",
        PayloadKey::BcFunction,
        "AsyncFunction",
    ),
    ClassInfo::class_def(
        "ASYNC_FUNCTION_RESOLVE",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION_RESOLVE",
        PayloadKey::AsyncFunctionResolving,
    ),
    ClassInfo::class_def(
        "ASYNC_FUNCTION_REJECT",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION_REJECT",
        PayloadKey::AsyncFunctionResolving,
    ),
    ClassInfo::narrow_type(
        "ASYNC_FROM_SYNC_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR",
        PayloadKey::AsyncFromSyncIterator,
        "AsyncFromSyncIterator",
    ),
    ClassInfo::class_def(
        "ASYNC_GENERATOR",
        "crate::builtins::generator::CLASS_DEF_ASYNC_GENERATOR",
        PayloadKey::AsyncGenerator,
    ),
    ClassInfo::narrow_type(
        "ASYNC_GENERATOR_FUNCTION", // depends on ASYNC_GENERATOR
        "crate::builtins::generator::CLASS_DEF_ASYNC_GENERATOR_FUNCTION",
        PayloadKey::BcFunction,
        "AsyncGeneratorFunction",
    ),
    ClassInfo::narrow_type(
        "ASYNC_ITERABLE_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ASYNC_ITERABLE_ITERATOR",
        PayloadKey::AsyncIterableIterator,
        "AsyncIterableIterator",
    ),
    //
    // ES2024+ Iterator global object
    //
    ClassInfo::class_def(
        "ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR",
        PayloadKey::None,
    ),
    ClassInfo::class_def(
        "ITERATOR_HELPER",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_HELPER",
        PayloadKey::IteratorHelper,
    ),
    ClassInfo::class_def(
        "ITERATOR_WRAP",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_WRAP",
        PayloadKey::IteratorWrap,
    ),
    ClassInfo::class_def(
        "ITERATOR_CONCAT",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_CONCAT",
        PayloadKey::IteratorConcat,
    ),
    ClassInfo::class_def(
        "ITERATOR_ZIP",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_ZIP",
        PayloadKey::IteratorZip,
    ),
    #[cfg(feature = "builtin_weakref")]
    ClassInfo::class_def(
        "WEAKREF",
        "crate::builtins::weakref::CLASS_DEF_WEAKREF",
        PayloadKey::WeakRef,
    ),
    #[cfg(feature = "builtin_weakref")]
    ClassInfo::class_def(
        "FINALIZATION_REGISTRY",
        "crate::builtins::weakref::CLASS_DEF_FINALIZATION_REGISTRY",
        PayloadKey::FinalizationRegistry,
    ),
];

pub fn gen_std_class_defs(outfile: &Path) {
    let mut cdefs = Vec::<String>::with_capacity(128);
    let mut consts = Vec::<String>::with_capacity(128);

    let mut async_start: Option<usize> = None;
    let mut async_end: Option<usize> = None;

    // builtin class defs
    //
    cdefs.push("pub(crate) const BUILTIN_CLASS_DEFS: &[&ClassDef] = &[".to_string());

    for (i, cdef) in CLASS_DEF_LIST.iter().enumerate() {
        match cdef.const_name {
            "PROMISE" => {
                async_start = Some(i);
            }
            "ASYNC_ITERABLE_ITERATOR" => {
                async_end = Some(i);
            }
            _ => {}
        }

        cdefs.push(format!(
            "// #{i}: id={} {}\n&{},",
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
        "use crate::{class::{ClassID, ClassDef}};\n".to_string(),
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
    // PayloadType impls: map each payload data type back to its variant accessor,
    // enabling generic extraction via class_payload()/class_payload_mut().
    let mut payload_type_impls = Vec::<String>::with_capacity(CLASS_PAYLOAD_TYPE.len());

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

            payload_type_impls.push(format!(
                r#"impl crate::object::PayloadType for {ty} {{
                    #[inline(always)]
                    fn from_payload(p: &ObjectPayload) -> Option<&Self> {{ p.{k}() }}
                    #[inline(always)]
                    fn from_payload_mut(p: &mut ObjectPayload) -> Option<&mut Self> {{ p.{k}_mut() }}
                }}"#
            ));
        }
    }

    gc_trace_arms.push("Self::OTHER(x) => { x.trace(tr); }".to_string());

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
        // PayloadType impls
        "\n\n".to_string(),
        payload_type_impls.join("\n\n"),
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

/// Bulk-generate `object_cast!` boilerplate for the narrow object wrapper
/// types. The set of narrow types is aggregated from `CLASS_DEF_LIST`: every
/// class whose `ClassInfo.narrow` is `Some(name)` joins the named narrow
/// type. `BCWrapper` is the single exception: it groups four function classes
/// (BYTECODE_FUNCTION, GENERATOR_FUNCTION, ASYNC_FUNCTION,
/// ASYNC_GENERATOR_FUNCTION) that each already map to their own narrow type,
/// so it is hand-written in `src/object/casts.rs` instead of here.
///
/// Emits `$OUT_DIR/object_casts.rs` which is `include!`d from
/// `src/object/casts.rs` (inside `crate::object`, where `Object`, `ObjectImpl`,
/// `JSValue`, `GcRef` and the `object_cast!` macro are all in scope).
///
/// Feature handling is implicit: `CLASS_DEF_LIST` entries are gated with
/// `#[cfg(feature)]`, so when a feature is off its classes are not present in
/// the list and the corresponding narrow type is simply not generated. Each
/// generated struct is a transparent, `Copy` wrapper with a `pub(crate)` GcRef
/// field so inherent-method `impl` blocks in the original modules can still
/// access `.0`.
pub fn gen_object_casts(outfile: &Path) {
    let mut groups: std::collections::BTreeMap<&str, Vec<&ClassInfo>> =
        std::collections::BTreeMap::new();
    for cdef in CLASS_DEF_LIST {
        if let Some(narrow) = cdef.narrow {
            groups.entry(narrow).or_default().push(cdef);
        }
    }

    let mut items = Vec::<String>::with_capacity(groups.len());
    let mut used_consts: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for (ty, members) in &groups {
        let class_ids = members
            .iter()
            .map(|m| format!("JS_CLASS_{}", m.const_name))
            .collect::<Vec<_>>()
            .join(" | ");
        used_consts.extend(members.iter().map(|m| format!("JS_CLASS_{}", m.const_name)));

        items.push(format!(
            "/// Narrow wrapper around a GcRef<ObjectImpl>.\n\
             #[repr(transparent)]\n\
             #[derive(Debug, Clone, Copy)]\n\
             pub struct {ty}(pub(crate) GcRef<ObjectImpl>);\n\n\
             object_cast!({ty}, {class_ids});\n",
            ty = ty,
        ));
    }

    let mut lines = vec![
        "// @generated by build_stdclass.rs gen_object_casts. DO NOT EDIT.".to_string(),
        String::new(),
        "use crate::{".to_string(),
        used_consts.into_iter().collect::<Vec<_>>().join(", "),
        "};".to_string(),
        String::new(),
    ];
    lines.extend(items);

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
