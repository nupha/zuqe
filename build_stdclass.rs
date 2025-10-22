use std::{collections::HashMap, path::Path};

use once_cell::sync::Lazy;

struct StdClassDef {
    class_name: &'static str,
    const_name: &'static str,
    key: Option<&'static str>,
    finalizer: Option<&'static str>,
    gc_mark: Option<&'static str>,
    payload: Option<&'static str>,
}

macro_rules! class_def {
    ($name:expr, $const_name:expr, $key:expr, $payload:expr, $fin:expr, $mark:expr) => {
        StdClassDef {
            class_name: $name,
            const_name: $const_name,
            key: if $key.is_empty() { None } else { Some($key) },
            payload: $payload,
            finalizer: $fin,
            gc_mark: $mark,
        }
    };
}

static CLASS_PAYLOAD_TYPE: Lazy<HashMap<&'static str, &'static str>> = Lazy::new(|| {
    let mut m = HashMap::with_capacity(128);

    m.insert("array", "crate::array::ArrayObjPayload");
    m.insert("value", "crate::value::JSValue");
    m.insert("datetime", "std::cell::Cell<crate::date::Datetime>");
    m.insert("native_func", "crate::function::ObjNativeFunc");
    m.insert(
        "native_func_with_data",
        "Box<crate::callable::NativeFuncData>",
    );
    m.insert("bc_function", "Box<crate::function::BCFuncPayload>");
    m.insert(
        "bound_function",
        "Box<crate::function::BoundFuncObjPayload>",
    );
    m.insert("regexp", "Box<crate::regexp::RegExpData>");
    m.insert(
        "for_in_iterator",
        "std::cell::RefCell<Box<crate::iterator::ForInIterPayload>>",
    );
    m.insert("array_buffer", "crate::array_buffer::ArrayBufferPayload");
    m.insert(
        "typed_array",
        "std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>",
    );
    m.insert(
        "mapset",
        "std::cell::RefCell<Box<crate::mapset::JSMapState>>",
    );
    m.insert(
        "mapset_iterator",
        "std::cell::RefCell<Box<crate::mapset::JSMapIteratorData>>",
    );
    m.insert("array_iterator", "Box<crate::iterator::ArrayIterPayload>");
    m.insert(
        "regexp_string_iterator",
        "Box<crate::regexp::RegExpStringIterPayload>",
    );
    m.insert("generator", "Box<crate::generator::GeneratorPayload>");
    m.insert("proxy", "Box<crate::proxy::ProxyPayload>");
    m.insert("promise", "Box<crate::promise::PromisePayload>");
    m.insert(
        "promise_resolving_function",
        "Box<crate::promise::PromiseResolvePayload>",
    );
    m.insert(
        "async_function_resolving",
        "crate::function::AsyncFuncState",
    );
    m.insert(
        "async_from_sync_iterator",
        "Box<crate::iterator::JSAsyncFromSyncIteratorData>",
    );
    m.insert("async_generator", "Box<crate::generator::AGenPayload>");

    m
});

static CLASS_DEF_LIST: Lazy<Vec<StdClassDef>> = Lazy::new(|| {
    vec![
        class_def!("Object", "OBJECT", "", None, None, None),
        class_def!(
            "Array",
            "ARRAY",
            "array",
            Some("crate::array::ArrayObjPayload"),
            Some("crate::array::js_array_finalizer"),
            Some("crate::array::js_array_mark")
        ),
        class_def!("Error", "ERROR", "", None, None, None),
        class_def!(
            "Number",
            "NUMBER",
            "value",
            Some("crate::value::JSValue"),
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "String",
            "STRING",
            "value",
            Some("crate::value::JSValue"),
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "Boolean",
            "BOOLEAN",
            "value",
            Some("crate::value::JSValue"),
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "Symbol",
            "SYMBOL",
            "value",
            Some("crate::value::JSValue"),
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "Arguments",
            "ARGUMENTS",
            "array",
            Some("crate::array::ArrayObjPayload"),
            Some("crate::array::js_array_finalizer"),
            Some("crate::array::js_array_mark")
        ),
        class_def!("Arguments", "MAPPED_ARGUMENTS", "", None, None, None),
        class_def!(
            "Date",
            "DATE",
            "datetime",
            Some("std::cell::Cell<crate::date::Datetime>"),
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!("Object", "MODULE_NS", "", None, None, None),
        class_def!(
            "Function",
            "C_FUNCTION",
            "native_func",
            Some("crate::function::ObjNativeFunc"),
            Some("crate::function::ObjNativeFunc::class_finalizer"),
            Some("crate::function::ObjNativeFunc::class_mark")
        ),
        class_def!(
            "Function",
            "BYTECODE_FUNCTION",
            "bc_function",
            Some("Box<crate::function::BCFuncPayload>"),
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "Function",
            "BOUND_FUNCTION",
            "bound_function",
            Some("Box<crate::function::BoundFuncObjPayload>"),
            Some("crate::function::BoundFunction::class_finalizer"),
            Some("crate::function::BoundFunction::class_mark")
        ),
        class_def!(
            "Function",
            "C_FUNCTION_DATA",
            "native_func_with_data",
            Some("Box<crate::callable::NativeFuncData>"),
            Some("crate::callable::NativeFunctionWithData::class_finalizer"),
            Some("crate::callable::NativeFunctionWithData::class_mark")
        ),
        class_def!(
            "GeneratorFunction",
            "GENERATOR_FUNCTION",
            "bc_function",
            Some("Box<crate::function::BCFuncPayload>"),
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "ForInIterPayload",
            "FOR_IN_ITERATOR",
            "for_in_iterator",
            Some("std::cell::RefCell<Box<crate::iterator::ForInIterPayload>>"),
            Some("crate::iterator::js_for_in_iterator_finalizer"),
            Some("crate::iterator::js_for_in_iterator_mark")
        ),
        class_def!(
            "RegExp",
            "REGEXP",
            "regexp",
            Some("Box<crate::regexp::RegExpData>"),
            Some("crate::regexp::RegExp::class_finalizer"),
            None
        ),
        class_def!(
            "ArrayBuffer",
            "ARRAY_BUFFER",
            "array_buffer",
            Some("crate::array_buffer::ArrayBufferPayload"),
            Some("crate::array_buffer::js_array_buffer_finalizer"),
            None
        ),
        class_def!(
            "SharedArrayBuffer",
            "SHARED_ARRAY_BUFFER",
            "array_buffer",
            Some("crate::array_buffer::ArrayBufferPayload"),
            Some("crate::array_buffer::js_array_buffer_finalizer"),
            None
        ),
        //
        // start of typed arrays
        //
        class_def!(
            "Uint8ClampedArray",
            "UINT8C_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Int8Array",
            "INT8_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Uint8Array",
            "UINT8_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Int16Array",
            "INT16_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Uint16Array",
            "UINT16_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Int32Array",
            "INT32_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Uint32Array",
            "UINT32_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "BigInt64Array",
            "BIG_INT64_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "BigUint64Array",
            "BIG_UINT64_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Float32Array",
            "FLOAT32_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Float64Array",
            "FLOAT64_ARRAY",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        // end of typed arrays
        class_def!(
            "DataView",
            "DATAVIEW",
            "typed_array",
            Some("std::cell::RefCell<Box<crate::typed_array::TypedArrayPayload>>"),
            None,
            None
        ),
        class_def!(
            "BigInt",
            "BIG_INT",
            "",
            None,
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        //
        // map set and iterators
        //
        class_def!(
            "Map",
            "MAP",
            "mapset",
            Some("std::cell::RefCell<Box<crate::mapset::JSMapState>>"),
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark")
        ),
        class_def!(
            "Set",
            "SET",
            "mapset",
            Some("std::cell::RefCell<Box<crate::mapset::JSMapState>>"),
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark")
        ),
        class_def!(
            "WeakMap",
            "WEAKMAP",
            "mapset",
            Some("std::cell::RefCell<Box<crate::mapset::JSMapState>>"),
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark")
        ),
        class_def!(
            "WeakSet",
            "WEAKSET",
            "mapset",
            Some("std::cell::RefCell<Box<crate::mapset::JSMapState>>"),
            Some("crate::mapset::JSMapState::class_finalizer"),
            Some("crate::mapset::JSMapState::class_mark")
        ),
        class_def!(
            "Map_Iterator",
            "MAP_ITERATOR",
            "mapset_iterator",
            Some("std::cell::RefCell<Box<crate::mapset::JSMapIteratorData>>"),
            Some("crate::mapset::js_map_iterator_finalizer"),
            Some("crate::mapset::js_map_iterator_mark")
        ),
        class_def!(
            "Set_Iterator",
            "SET_ITERATOR",
            "mapset_iterator",
            Some("std::cell::RefCell<Box<crate::mapset::JSMapIteratorData>>"),
            Some("crate::mapset::js_map_iterator_finalizer"),
            Some("crate::mapset::js_map_iterator_mark")
        ),
        class_def!(
            "Array_Iterator",
            "ARRAY_ITERATOR",
            "array_iterator",
            Some("Box<crate::iterator::ArrayIterPayload>"),
            Some("crate::iterator::ArrayIterPayload::class_finalizer"),
            Some("crate::iterator::ArrayIterPayload::class_mark")
        ),
        class_def!(
            "String_Iterator",
            "STRING_ITERATOR",
            "array_iterator",
            Some("Box<crate::iterator::ArrayIterPayload>"),
            Some("crate::iterator::ArrayIterPayload::class_finalizer"),
            Some("crate::iterator::ArrayIterPayload::class_mark")
        ),
        class_def!(
            "RegExp_String_Iterator",
            "REGEXP_STRING_ITERATOR",
            "regexp_string_iterator",
            Some("Box<crate::regexp::RegExpStringIterPayload>"),
            Some("crate::regexp::js_regexp_iterator_finalizer"),
            Some("crate::regexp::js_regexp_iterator_mark")
        ),
        //
        //
        //
        class_def!(
            "Generator",
            "GENERATOR",
            "generator",
            Some("Box<crate::generator::GeneratorPayload>"),
            Some("crate::generator::js_generator_finalizer"),
            Some("crate::generator::js_generator_mark")
        ),
        //
        // Proxy
        //
        class_def!(
            "Object", // Proxy class name is Object
            "PROXY",
            "proxy",
            Some("Box<crate::proxy::ProxyPayload>"),
            Some("crate::proxy::Proxy::class_finalizer"),
            Some("crate::proxy::Proxy::class_mark")
        ),
        //
        // Async stuff
        //
        class_def!(
            "Promise",
            "PROMISE",
            "promise",
            Some("Box<crate::promise::PromisePayload>"),
            Some("crate::promise::Promise::class_finalizer"),
            Some("crate::promise::Promise::class_mark")
        ),
        class_def!(
            "PromiseResolveFunction",
            "PROMISE_RESOLVE_FUNCTION",
            "promise_resolving_function",
            Some("Box<crate::promise::PromiseResolvePayload>"),
            Some("crate::promise::PromiseResolveFunc::class_finalizer"),
            Some("crate::promise::PromiseResolveFunc::class_mark")
        ),
        class_def!(
            "PromiseRejectFunction",
            "PROMISE_REJECT_FUNCTION",
            "promise_resolving_function",
            Some("Box<crate::promise::PromiseResolvePayload>"),
            Some("crate::promise::PromiseResolveFunc::class_finalizer"),
            Some("crate::promise::PromiseResolveFunc::class_mark")
        ),
        class_def!(
            "AsyncFunction",
            "ASYNC_FUNCTION",
            "bc_function",
            Some("Box<crate::function::BCFuncPayload>"),
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "AsyncFunctionResolve",
            "ASYNC_FUNCTION_RESOLVE",
            "async_function_resolving",
            Some("crate::function::AsyncFuncState"),
            Some("crate::function::AsyncFuncState::class_finalizer"),
            Some("crate::function::AsyncFuncState::class_mark")
        ),
        class_def!(
            "AsyncFunctionReject",
            "ASYNC_FUNCTION_REJECT",
            "async_function_resolving",
            Some("crate::function::AsyncFuncState"),
            Some("crate::function::AsyncFuncState::class_finalizer"),
            Some("crate::function::AsyncFuncState::class_mark")
        ),
        class_def!(
            "empty_string", // JS_ATOM_empty_string
            "ASYNC_FROM_SYNC_ITERATOR",
            "async_from_sync_iterator",
            Some("Box<crate::iterator::JSAsyncFromSyncIteratorData>"),
            Some("crate::iterator::js_async_from_sync_iterator_finalizer"),
            Some("crate::iterator::js_async_from_sync_iterator_mark")
        ),
        class_def!(
            "AsyncGeneratorFunction",
            "ASYNC_GENERATOR_FUNCTION",
            "bc_function",
            Some("Box<crate::function::BCFuncPayload>"),
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "AsyncGenerator",
            "ASYNC_GENERATOR",
            "async_generator",
            Some("Box<crate::generator::AGenPayload>"),
            Some("crate::generator::AsyncGeneratorFunction::class_finalizer"),
            Some("crate::generator::AsyncGeneratorFunction::class_mark")
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
        "pub const STD_CLASS_DEFS: [JSClassShortDef;{}] = [",
        class_defs.len()
    ));

    for (i, cdef) in class_defs.iter().enumerate() {
        if let Some(k) = cdef.key {
            debug_assert_eq!(
                cdef.payload,
                Some(CLASS_PAYLOAD_TYPE[k]),
                "{k}: {:?}, {}",
                cdef.payload,
                CLASS_PAYLOAD_TYPE[k]
            );
        }

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
            "// {i}: {}\nJSClassShortDef{{class_name:JS_ATOM_{name}, finalizer:{}, gc_mark:{} }},",
            cdef.const_name,
            cdef.finalizer
                .map_or("None".to_string(), |s| format!("Some({s})")),
            cdef.gc_mark
                .map_or("None".to_string(), |s| format!("Some({s})")),
        ));

        consts.push(format!(
            "pub const JS_CLASS_{}:JSClassID={};",
            cdef.const_name,
            i + 1
        ));
    }

    consts.push(format!(
        "pub const JS_CLASS_INIT_COUNT: usize={};",
        class_defs.len() + 1
    ));
    cdefs.push("];".to_string());

    let lines = [
        "use crate::{atom::consts::*, class::{JSClassID, JSClassShortDef}};\n".to_string(),
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
        items.push(if let Some(p) = cdef.payload {
            format!("{}({})", cdef.const_name, p)
        } else {
            cdef.const_name.to_string()
        });

        getid_arms.push(if cdef.payload.is_some() {
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
                if c.key == Some(k) {
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
        "use crate::{  class::{JSClassID }};\n".to_string(),
        // ObjectPayload
        "#[derive(Debug)]\npub enum ObjectPayload {\n".to_string(),
        items.join(",\n"),
        "}".to_string(),
        "".to_string(),
        // impl
        "impl ObjectPayload {".to_string(),
        // class_id()
        format!(
            "pub fn class_id(&self)->JSClassID {{ match self {{ {} }} }}",
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
