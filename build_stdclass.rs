use std::{collections::HashMap, path::Path};

use once_cell::sync::Lazy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
enum PayloadKey {
    None,
    Array,
    Value,
    Datetime,
    NativeFunc,
    NativeFuncWithData,
    BcFunction,
    BoundFunction,
    RegExp,
    ForInIterator,
    ArrayBuffer,
    TypedArray,
    DataView,
    MapSet,
    MapSetIterator,
    ArrayIterator,
    RegExpStringIterator,
    Generator,
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
            PayloadKey::Value => "value",
            PayloadKey::Datetime => "datetime",
            PayloadKey::NativeFunc => "native_func",
            PayloadKey::NativeFuncWithData => "native_func_with_data",
            PayloadKey::BcFunction => "bc_function",
            PayloadKey::BoundFunction => "bound_function",
            PayloadKey::RegExp => "regexp",
            PayloadKey::ForInIterator => "for_in_iterator",
            PayloadKey::ArrayBuffer => "array_buffer",
            PayloadKey::TypedArray => "typed_array",
            PayloadKey::DataView => "data_view",
            PayloadKey::MapSet => "mapset",
            PayloadKey::MapSetIterator => "mapset_iterator",
            PayloadKey::ArrayIterator => "array_iterator",
            PayloadKey::RegExpStringIterator => "regexp_string_iterator",
            PayloadKey::Generator => "generator",
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

    m.insert(PayloadKey::Array, "crate::array::ArrayObjPayload");
    m.insert(PayloadKey::Value, "crate::value::JSValue");
    m.insert(
        PayloadKey::Datetime,
        "std::cell::Cell<crate::date::Datetime>",
    );
    m.insert(PayloadKey::NativeFunc, "crate::function::NFuncPayload");
    m.insert(
        PayloadKey::NativeFuncWithData,
        "Box<crate::callable::NativeFuncData>",
    );
    m.insert(
        PayloadKey::BcFunction,
        "Box<crate::function::BCFuncPayload>",
    );
    m.insert(
        PayloadKey::BoundFunction,
        "Box<crate::function::BoundFuncObjPayload>",
    );
    m.insert(PayloadKey::RegExp, "Box<crate::regexp::RegExpData>");
    m.insert(
        PayloadKey::ForInIterator,
        "std::cell::RefCell<Box<crate::iterator::ForInIterPayload>>",
    );
    m.insert(
        PayloadKey::ArrayBuffer,
        "crate::array_buffer::ArrayBufferPayload",
    );
    m.insert(
        PayloadKey::TypedArray,
        "Box<crate::typed_array::TypedArrayPayload>",
    );
    m.insert(
        PayloadKey::DataView,
        "Box<crate::typed_array::DataViewPayload>",
    );
    m.insert(
        PayloadKey::MapSet,
        "std::cell::RefCell<Box<crate::mapset::JSMapState>>",
    );
    m.insert(
        PayloadKey::MapSetIterator,
        "std::cell::RefCell<Box<crate::mapset::JSMapIteratorData>>",
    );
    m.insert(
        PayloadKey::ArrayIterator,
        "Box<crate::iterator::ArrayIterPayload>",
    );
    m.insert(
        PayloadKey::RegExpStringIterator,
        "Box<crate::regexp::RegExpStringIterPayload>",
    );
    m.insert(
        PayloadKey::Generator,
        "Box<crate::generator::GeneratorPayload>",
    );
    m.insert(PayloadKey::Proxy, "Box<crate::proxy::ProxyPayload>");
    m.insert(PayloadKey::Promise, "Box<crate::promise::PromisePayload>");
    m.insert(
        PayloadKey::PromiseResolvingFunction,
        "Box<crate::promise::PromiseResolvePayload>",
    );
    m.insert(
        PayloadKey::AsyncFunctionResolving,
        "crate::function::AsyncFuncState",
    );
    m.insert(
        PayloadKey::AsyncFromSyncIterator,
        "Box<crate::iterator::JSAsyncFromSyncIteratorData>",
    );
    m.insert(
        PayloadKey::AsyncGenerator,
        "Box<crate::generator::AGenPayload>",
    );

    m
});

#[derive(Debug)]
struct ClassInfo {
    class_name: &'static str,
    const_name: &'static str,
    key: PayloadKey,
    initialize: Option<&'static str>,
    finalize: Option<&'static str>,
    gc_mark: Option<&'static str>,
}

impl ClassInfo {
    const fn class_def(
        class_name: &'static str,
        const_name: &'static str,
        key: PayloadKey,
        initialize: Option<&'static str>,
        finalize: Option<&'static str>,
        gc_mark: Option<&'static str>,
    ) -> Self {
        ClassInfo {
            class_name,
            const_name,
            key,
            initialize,
            finalize,
            gc_mark,
        }
    }
}

static CLASS_DEF_LIST: Lazy<Vec<ClassInfo>> = Lazy::new(|| {
    vec![
        // CORE CLASSES START
        ClassInfo::class_def("Object", "OBJECT", PayloadKey::None, None, None, None),
        ClassInfo::class_def(
            "Array",
            "ARRAY",
            PayloadKey::Array,
            None,
            Some("crate::array::js_array_finalizer"),
            Some("crate::array::js_array_mark"),
        ),
        ClassInfo::class_def("Error", "ERROR", PayloadKey::None, None, None, None),
        ClassInfo::class_def(
            "Number",
            "NUMBER",
            PayloadKey::Value,
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark"),
        ),
        ClassInfo::class_def(
            "String",
            "STRING",
            PayloadKey::Value,
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark"),
        ),
        ClassInfo::class_def(
            "Boolean",
            "BOOLEAN",
            PayloadKey::Value,
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark"),
        ),
        ClassInfo::class_def(
            "Symbol",
            "SYMBOL",
            PayloadKey::Value,
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark"),
        ),
        ClassInfo::class_def(
            "Arguments",
            "ARGUMENTS",
            PayloadKey::Array,
            None,
            Some("crate::array::js_array_finalizer"),
            Some("crate::array::js_array_mark"),
        ),
        ClassInfo::class_def(
            "Arguments",
            "MAPPED_ARGUMENTS",
            PayloadKey::None,
            None,
            None,
            None,
        ),
        ClassInfo::class_def(
            "Date",
            "DATE",
            PayloadKey::Datetime,
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark"),
        ),
        // CORE CLASSES END
        ClassInfo::class_def("Object", "MODULE_NS", PayloadKey::None, None, None, None),
        ClassInfo::class_def(
            "Function",
            "C_FUNCTION",
            PayloadKey::NativeFunc,
            None,
            Some("crate::function::NFuncPayload::class_finalizer"),
            Some("crate::function::NFuncPayload::class_mark"),
        ),
        ClassInfo::class_def(
            "Function",
            "BYTECODE_FUNCTION",
            PayloadKey::BcFunction,
            None,
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark"),
        ),
        ClassInfo::class_def(
            "Function",
            "BOUND_FUNCTION",
            PayloadKey::BoundFunction,
            None,
            Some("crate::function::BoundFunction::class_finalizer"),
            Some("crate::function::BoundFunction::class_mark"),
        ),
        ClassInfo::class_def(
            "Function",
            "C_FUNCTION_DATA",
            PayloadKey::NativeFuncWithData,
            None,
            Some("crate::callable::NativeFunctionWithData::class_finalizer"),
            Some("crate::callable::NativeFunctionWithData::class_mark"),
        ),
        ClassInfo::class_def(
            "GeneratorFunction",
            "GENERATOR_FUNCTION",
            PayloadKey::BcFunction,
            None,
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark"),
        ),
        ClassInfo::class_def(
            "ForInIterPayload",
            "FOR_IN_ITERATOR",
            PayloadKey::ForInIterator,
            None,
            Some("crate::iterator::js_for_in_iterator_finalizer"),
            Some("crate::iterator::js_for_in_iterator_mark"),
        ),
        ClassInfo::class_def(
            "RegExp",
            "REGEXP",
            PayloadKey::RegExp,
            None,
            Some("crate::regexp::RegExp::class_finalizer"),
            None,
        ),
        ClassInfo::class_def(
            "ArrayBuffer",
            "ARRAY_BUFFER",
            PayloadKey::ArrayBuffer,
            Some("crate::array_buffer::ArrayBuffer::class_initialize"),
            Some("crate::array_buffer::js_array_buffer_finalizer"),
            None,
        ),
        ClassInfo::class_def(
            "SharedArrayBuffer",
            "SHARED_ARRAY_BUFFER",
            PayloadKey::ArrayBuffer,
            Some("crate::array_buffer::ArrayBuffer::class_initialize"),
            Some("crate::array_buffer::js_array_buffer_finalizer"),
            None,
        ),
        //
        // start of typed arrays
        //
        ClassInfo::class_def(
            "Uint8ClampedArray",
            "UINT8C_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Int8Array",
            "INT8_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Uint8Array",
            "UINT8_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Int16Array",
            "INT16_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Uint16Array",
            "UINT16_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Int32Array",
            "INT32_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Uint32Array",
            "UINT32_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "BigInt64Array",
            "BIG_INT64_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "BigUint64Array",
            "BIG_UINT64_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Float32Array",
            "FLOAT32_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        ClassInfo::class_def(
            "Float64Array",
            "FLOAT64_ARRAY",
            PayloadKey::TypedArray,
            Some("crate::typed_array::TypedArray::class_initialize"),
            Some("crate::typed_array::TypedArray::class_finalize"),
            Some("crate::typed_array::js_typed_array_mark"),
        ),
        // end of typed arrays
        ClassInfo::class_def(
            "DataView",
            "DATAVIEW",
            PayloadKey::DataView,
            Some("crate::typed_array::DataView::class_initialize"),
            None,
            None,
        ),
        ClassInfo::class_def(
            "BigInt",
            "BIG_INT",
            PayloadKey::None,
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark"),
        ),
        //
        // map set and iterators
        //
        ClassInfo::class_def(
            "Map",
            "MAP",
            PayloadKey::MapSet,
            None,
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark"),
        ),
        ClassInfo::class_def(
            "Set",
            "SET",
            PayloadKey::MapSet,
            None,
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark"),
        ),
        ClassInfo::class_def(
            "WeakMap",
            "WEAKMAP",
            PayloadKey::MapSet,
            None,
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark"),
        ),
        ClassInfo::class_def(
            "WeakSet",
            "WEAKSET",
            PayloadKey::MapSet,
            None,
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark"),
        ),
        ClassInfo::class_def(
            "Map_Iterator",
            "MAP_ITERATOR",
            PayloadKey::MapSetIterator,
            None,
            Some("crate::mapset::js_map_iterator_finalizer"),
            Some("crate::mapset::js_map_iterator_mark"),
        ),
        ClassInfo::class_def(
            "Set_Iterator",
            "SET_ITERATOR",
            PayloadKey::MapSetIterator,
            None,
            Some("crate::mapset::js_map_iterator_finalizer"),
            Some("crate::mapset::js_map_iterator_mark"),
        ),
        ClassInfo::class_def(
            "Array_Iterator",
            "ARRAY_ITERATOR",
            PayloadKey::ArrayIterator,
            None,
            Some("crate::iterator::ArrayIterPayload::class_finalizer"),
            Some("crate::iterator::ArrayIterPayload::class_mark"),
        ),
        ClassInfo::class_def(
            "String_Iterator",
            "STRING_ITERATOR",
            PayloadKey::ArrayIterator,
            None,
            Some("crate::iterator::ArrayIterPayload::class_finalizer"),
            Some("crate::iterator::ArrayIterPayload::class_mark"),
        ),
        ClassInfo::class_def(
            "RegExp_String_Iterator",
            "REGEXP_STRING_ITERATOR",
            PayloadKey::RegExpStringIterator,
            None,
            Some("crate::regexp::js_regexp_iterator_finalizer"),
            Some("crate::regexp::js_regexp_iterator_mark"),
        ),
        //
        //
        //
        ClassInfo::class_def(
            "Generator",
            "GENERATOR",
            PayloadKey::Generator,
            None,
            Some("crate::generator::js_generator_finalizer"),
            Some("crate::generator::js_generator_mark"),
        ),
        //
        // Proxy
        //
        ClassInfo::class_def(
            "Object", // Proxy class name is Object
            "PROXY",
            PayloadKey::Proxy,
            Some("crate::proxy::Proxy::class_initialize"),
            Some("crate::proxy::Proxy::class_finalizer"),
            Some("crate::proxy::Proxy::class_mark"),
        ),
        //
        // Async stuff
        //
        ClassInfo::class_def(
            "Promise",
            "PROMISE",
            PayloadKey::Promise,
            None,
            Some("crate::promise::Promise::class_finalizer"),
            Some("crate::promise::Promise::class_mark"),
        ),
        ClassInfo::class_def(
            "PromiseResolveFunction",
            "PROMISE_RESOLVE_FUNCTION",
            PayloadKey::PromiseResolvingFunction,
            None,
            Some("crate::promise::PromiseResolveFunc::class_finalizer"),
            Some("crate::promise::PromiseResolveFunc::class_mark"),
        ),
        ClassInfo::class_def(
            "PromiseRejectFunction",
            "PROMISE_REJECT_FUNCTION",
            PayloadKey::PromiseResolvingFunction,
            None,
            Some("crate::promise::PromiseResolveFunc::class_finalizer"),
            Some("crate::promise::PromiseResolveFunc::class_mark"),
        ),
        ClassInfo::class_def(
            "AsyncFunction",
            "ASYNC_FUNCTION",
            PayloadKey::BcFunction,
            None,
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark"),
        ),
        ClassInfo::class_def(
            "AsyncFunctionResolve",
            "ASYNC_FUNCTION_RESOLVE",
            PayloadKey::AsyncFunctionResolving,
            None,
            Some("crate::function::AsyncFuncState::class_finalizer"),
            Some("crate::function::AsyncFuncState::class_mark"),
        ),
        ClassInfo::class_def(
            "AsyncFunctionReject",
            "ASYNC_FUNCTION_REJECT",
            PayloadKey::AsyncFunctionResolving,
            None,
            Some("crate::function::AsyncFuncState::class_finalizer"),
            Some("crate::function::AsyncFuncState::class_mark"),
        ),
        ClassInfo::class_def(
            "empty_string", // JS_ATOM_empty_string
            "ASYNC_FROM_SYNC_ITERATOR",
            PayloadKey::AsyncFromSyncIterator,
            None,
            Some("crate::iterator::js_async_from_sync_iterator_finalizer"),
            Some("crate::iterator::js_async_from_sync_iterator_mark"),
        ),
        ClassInfo::class_def(
            "AsyncGeneratorFunction",
            "ASYNC_GENERATOR_FUNCTION",
            PayloadKey::BcFunction,
            None,
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark"),
        ),
        ClassInfo::class_def(
            "AsyncGenerator",
            "ASYNC_GENERATOR",
            PayloadKey::AsyncGenerator,
            None,
            Some("crate::generator::AsyncGeneratorFunction::class_finalizer"),
            Some("crate::generator::AsyncGeneratorFunction::class_mark"),
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
            "// {i}: {}\nStdClassDef{{class_name:JS_ATOM_{name}, initialize:{}, finalize:{}, gc_mark:{} }},",
            cdef.const_name,
            cdef.initialize
                .map_or("None".to_string(), |s| format!("Some({s})")),
            cdef.finalize
                .map_or("None".to_string(), |s| format!("Some({s})")),
            cdef.gc_mark
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
        "use crate::{atom::consts::*, class::{ClassID, StdClassDef}};\n".to_string(),
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
            format!("pub fn {k}(&self)->Option<&{v}> {{ if let {} = self {{ Some(x) }} else {{ None }} }}",
            arms.join("|")
        ));
        }
    }

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
            "pub fn class_id(&self)->ClassID {{ match self {{ {} }} }}",
            getid_arms.join(",\n"),
        ),
        "\n".into(),
        // get payload methods
        get_payload_methods.join("\n\n"),
        "}".to_string(),
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
