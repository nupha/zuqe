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
    NativeFuncWithData,
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
            PayloadKey::NativeFuncWithData => "native_func_with_data",
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

    m.insert(PayloadKey::Array, "crate::builtins::array::ArrayPayload");
    m.insert(PayloadKey::Bool, "bool");
    m.insert(PayloadKey::String, "crate::string::StringRef");
    m.insert(PayloadKey::Value, "crate::value::JSValue");
    m.insert(PayloadKey::NativeFunc, "crate::function::NFuncPayload");
    m.insert(
        PayloadKey::NativeFuncWithData,
        "Box<crate::function::NativeFuncData>",
    );
    m.insert(
        PayloadKey::BcFunction,
        "Box<crate::function::BCFuncPayload>",
    );
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
    key: PayloadKey,
    initialize: Option<&'static str>,
    finalize: Option<&'static str>,
}

impl ClassInfo {
    const fn class_def(
        class_name: &'static str,
        const_name: &'static str,
        key: PayloadKey,
        initialize: Option<&'static str>,
        finalize: Option<&'static str>,
    ) -> Self {
        ClassInfo {
            class_name,
            const_name,
            key,
            initialize,
            finalize,
        }
    }
}

static CLASS_DEF_LIST: Lazy<Vec<ClassInfo>> = Lazy::new(|| {
    vec![
        // CORE CLASSES START
        ClassInfo::class_def("Object", "OBJECT", PayloadKey::None, None, None),
        ClassInfo::class_def("Array", "ARRAY", PayloadKey::Array, None, None),
        ClassInfo::class_def("Error", "ERROR", PayloadKey::None, None, None),
        ClassInfo::class_def("Number", "NUMBER", PayloadKey::Value, None, None),
        ClassInfo::class_def("Boolean", "BOOLEAN", PayloadKey::Bool, None, None),
        ClassInfo::class_def("String", "STRING", PayloadKey::String, None, None),
        #[cfg(feature = "builtin_symbol")]
        ClassInfo::class_def("Symbol", "SYMBOL", PayloadKey::String, None, None),
        ClassInfo::class_def("Arguments", "ARGUMENTS", PayloadKey::Array, None, None),
        ClassInfo::class_def(
            "Arguments",
            "MAPPED_ARGUMENTS",
            PayloadKey::None,
            None,
            None,
        ),
        #[cfg(feature = "builtin_date")]
        ClassInfo::class_def(
            "Date",
            "DATE",
            PayloadKey::Datetime,
            Some("crate::builtins::date::Datetime::class_initialize"),
            None,
        ),
        // CORE CLASSES END
        ClassInfo::class_def("Object", "MODULE_NS", PayloadKey::None, None, None),
        ClassInfo::class_def("Function", "C_FUNCTION", PayloadKey::NativeFunc, None, None),
        ClassInfo::class_def(
            "Function",
            "BYTECODE_FUNCTION",
            PayloadKey::BcFunction,
            None,
            None,
        ),
        ClassInfo::class_def(
            "Function",
            "BOUND_FUNCTION",
            PayloadKey::BoundFunction,
            None,
            None,
        ),
        ClassInfo::class_def(
            "Function",
            "C_FUNCTION_DATA",
            PayloadKey::NativeFuncWithData,
            None,
            None,
        ),
        ClassInfo::class_def(
            "GeneratorFunction",
            "GENERATOR_FUNCTION",
            PayloadKey::BcFunction,
            None,
            None,
        ),
        ClassInfo::class_def(
            "ForInIterPayload",
            "FOR_IN_ITERATOR",
            PayloadKey::ForInIterator,
            None,
            None,
        ),
        #[cfg(feature = "builtin_regexp")]
        ClassInfo::class_def(
            "RegExp",
            "REGEXP",
            PayloadKey::RegExp,
            Some("crate::builtins::regexp::RegExp::class_initialize"),
            None,
        ),
        ClassInfo::class_def(
            "ArrayBuffer",
            "ARRAY_BUFFER",
            PayloadKey::ArrayBuffer,
            Some("crate::builtins::array_buffer::ArrayBuffer::class_initialize"),
            Some("crate::builtins::array_buffer::ArrayBuffer::class_finalize"),
        ),
        ClassInfo::class_def(
            "SharedArrayBuffer",
            "SHARED_ARRAY_BUFFER",
            PayloadKey::ArrayBuffer,
            Some("crate::builtins::array_buffer::ArrayBuffer::class_initialize"),
            Some("crate::builtins::array_buffer::ArrayBuffer::class_finalize"),
        ),
        //
        // start of typed arrays
        //
        ClassInfo::class_def(
            "Uint8ClampedArray",
            "UINT8C_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Int8Array",
            "INT8_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Uint8Array",
            "UINT8_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Int16Array",
            "INT16_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Uint16Array",
            "UINT16_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Int32Array",
            "INT32_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Uint32Array",
            "UINT32_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "BigInt64Array",
            "BIG_INT64_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "BigUint64Array",
            "BIG_UINT64_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Float32Array",
            "FLOAT32_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        ClassInfo::class_def(
            "Float64Array",
            "FLOAT64_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::builtins::typed_array::TypedArray::class_initialize"),
            Some("crate::builtins::typed_array::TypedArray::class_finalize"),
        ),
        // end of typed arrays
        ClassInfo::class_def(
            "DataView",
            "DATAVIEW",
            PayloadKey::DataView,
            Some("crate::builtins::typed_array::DataView::class_initialize"),
            None,
        ),
        ClassInfo::class_def("BigInt", "BIG_INT", PayloadKey::None, None, None),
        //
        // map set and iterators
        //
        #[cfg(feature = "builtin_mapset")]
        ClassInfo::class_def(
            "Map",
            "MAP",
            PayloadKey::MapSet,
            Some("crate::builtins::mapset::MapSet::class_initialize"),
            None,
        ),
        #[cfg(feature = "builtin_mapset")]
        ClassInfo::class_def(
            "Set",
            "SET",
            PayloadKey::MapSet,
            Some("crate::builtins::mapset::MapSet::class_initialize"),
            None,
        ),
        #[cfg(feature = "builtin_mapset")]
        ClassInfo::class_def(
            "WeakMap",
            "WEAKMAP",
            PayloadKey::MapSet,
            Some("crate::builtins::mapset::MapSet::class_initialize"),
            None,
        ),
        #[cfg(feature = "builtin_mapset")]
        ClassInfo::class_def(
            "WeakSet",
            "WEAKSET",
            PayloadKey::MapSet,
            Some("crate::builtins::mapset::MapSet::class_initialize"),
            None,
        ),
        #[cfg(feature = "builtin_mapset")]
        ClassInfo::class_def(
            "Map_Iterator",
            "MAP_ITERATOR",
            PayloadKey::MapSetIterator,
            Some("crate::builtins::mapset::MapSet::class_initialize"),
            None,
        ),
        #[cfg(feature = "builtin_mapset")]
        ClassInfo::class_def(
            "Set_Iterator",
            "SET_ITERATOR",
            PayloadKey::MapSetIterator,
            Some("crate::builtins::mapset::MapSet::class_initialize"),
            None,
        ),
        ClassInfo::class_def(
            "Array_Iterator",
            "ARRAY_ITERATOR",
            PayloadKey::ArrayIterator,
            None,
            None,
        ),
        ClassInfo::class_def(
            "String_Iterator",
            "STRING_ITERATOR",
            PayloadKey::ArrayIterator,
            None,
            None,
        ),
        #[cfg(feature = "builtin_regexp")]
        ClassInfo::class_def(
            "RegExp_String_Iterator",
            "REGEXP_STRING_ITERATOR",
            PayloadKey::RegExpStringIterator,
            Some("crate::builtins::regexp::RegExp::class_initialize"),
            None,
        ),
        //
        //
        //
        ClassInfo::class_def("Generator", "GENERATOR", PayloadKey::Generator, None, None),
        //
        // Proxy
        //
        #[cfg(feature = "builtin_proxy")]
        ClassInfo::class_def(
            "Object", // Proxy class name is Object
            "PROXY",
            PayloadKey::Proxy,
            Some("crate::builtins::proxy::Proxy::class_initialize"),
            None,
        ),
        //
        // Async stuff
        //
        ClassInfo::class_def(
            "Promise",
            "PROMISE",
            PayloadKey::Promise,
            Some("crate::builtins::promise::Promise::class_initialize"),
            None,
        ),
        ClassInfo::class_def(
            "PromiseResolveFunction",
            "PROMISE_RESOLVE_FUNCTION",
            PayloadKey::PromiseResolvingFunction,
            None,
            None,
        ),
        ClassInfo::class_def(
            "PromiseRejectFunction",
            "PROMISE_REJECT_FUNCTION",
            PayloadKey::PromiseResolvingFunction,
            None,
            None,
        ),
        ClassInfo::class_def(
            "AsyncFunction",
            "ASYNC_FUNCTION",
            PayloadKey::BcFunction,
            Some("crate::function::AsyncFunction::class_initialize"),
            None,
        ),
        ClassInfo::class_def(
            "AsyncFunctionResolve",
            "ASYNC_FUNCTION_RESOLVE",
            PayloadKey::AsyncFunctionResolving,
            None,
            None,
        ),
        ClassInfo::class_def(
            "AsyncFunctionReject",
            "ASYNC_FUNCTION_REJECT",
            PayloadKey::AsyncFunctionResolving,
            None,
            None,
        ),
        ClassInfo::class_def(
            "empty_string", // JS_ATOM_empty_string
            "ASYNC_FROM_SYNC_ITERATOR",
            PayloadKey::AsyncFromSyncIterator,
            Some("crate::builtins::iterator::AsyncFromSyncIterator::class_initialize"),
            None,
        ),
        ClassInfo::class_def(
            "AsyncGeneratorFunction",
            "ASYNC_GENERATOR_FUNCTION",
            PayloadKey::BcFunction,
            Some("crate::builtins::generator::AsyncGeneratorFunction::class_initialize"),
            None,
        ),
        ClassInfo::class_def(
            "AsyncGenerator",
            "ASYNC_GENERATOR",
            PayloadKey::AsyncGenerator,
            Some("crate::builtins::generator::AsyncGeneratorFunction::class_initialize"),
            None,
        ),
    ]
});

pub fn gen_std_class_defs(outfile: &Path) {
    let class_defs = Lazy::force(&CLASS_DEF_LIST);

    let mut cdefs = Vec::<String>::with_capacity(128);
    let mut consts = Vec::<String>::with_capacity(128);

    let mut async_start: Option<usize> = None;
    let mut async_end: Option<usize> = None;

    cdefs.push(format!(
        "pub const STD_CLASS_DEFS: [StdClassDef;{}] = [",
        class_defs.len()
    ));

    for (i, cdef) in class_defs.iter().enumerate() {
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
            "// {i}: {}\nStdClassDef{{class_name:Atom::CONST_{name}, initialize:{}, finalize:{} }},",
            cdef.const_name,
            cdef.initialize
                .map_or("None".to_string(), |s| format!("Some({s})")),
            cdef.finalize
                .map_or("None".to_string(), |s| format!("Some({s})")),
        ));

        consts.push(format!(
            "pub const JS_CLASS_{}:ClassID={};",
            cdef.const_name,
            i + 1
        ));
    }

    consts.push(format!(
        "pub const USER_CLASS_ID_START: ClassID={};",
        class_defs.len() + 1
    ));

    cdefs.push("];".to_string());

    let lines = [
        "use crate::{Atom, class::{ClassID, StdClassDef}};\n".to_string(),
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
    let class_defs = Lazy::force(&CLASS_DEF_LIST);

    let mut items = Vec::<String>::with_capacity(class_defs.len() + 10);
    let mut getid_arms = Vec::<String>::with_capacity(class_defs.len() + 8);

    for cdef in class_defs {
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
    let mut get_payload_methods = Vec::<String>::with_capacity(class_defs.len() + 8);
    let mut gc_trace_arms = Vec::<String>::with_capacity(class_defs.len() + 8);

    let mut kvs = CLASS_PAYLOAD_TYPE.iter().collect::<Vec<_>>();
    kvs.sort();
    for (k, v) in kvs {
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
            get_payload_methods.push(
                format!("pub const fn {k}(&self)->Option<&{v}> {{ if let {} = self {{ Some(x) }} else {{ None }} }}",
                arms.join("|")
            ));
            get_payload_methods.push(
                format!("pub const fn {k}_mut(&mut self)->Option<&mut {v}> {{ if let {} = self {{ Some(x) }} else {{ None }} }}",
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
        "#[derive(Debug)]\npub enum ObjectPayload {\n".to_string(),
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
            r#"unsafe impl gc_lite::GcTracable for ObjectPayload {{
                fn trace(&self, tr: gc_lite::GcTraceOp) {{
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
