use std::path::Path;

struct StdClassDef {
    class_name: &'static str,
    const_name: &'static str,
    finalizer: Option<&'static str>,
    gc_mark: Option<&'static str>,
}

macro_rules! class_def {
    ($name:expr, $const_name:expr, $fin:expr, $mark:expr) => {
        StdClassDef {
            class_name: $name,
            const_name: $const_name,
            finalizer: $fin,
            gc_mark: $mark,
        }
    };
}

pub fn gen_std_class_defs(outfile: &Path) {
    let class_defs = vec![
        class_def!("Object", "OBJECT", None, None),
        class_def!(
            "Array",
            "ARRAY",
            Some("crate::array::js_array_finalizer"),
            Some("crate::array::js_array_mark")
        ),
        class_def!("Error", "ERROR", None, None),
        class_def!(
            "Number",
            "NUMBER",
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "String",
            "STRING",
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "Boolean",
            "BOOLEAN",
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "Symbol",
            "SYMBOL",
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!(
            "Arguments",
            "ARGUMENTS",
            Some("crate::array::js_array_finalizer"),
            Some("crate::array::js_array_mark")
        ),
        class_def!("Arguments", "MAPPED_ARGUMENTS", None, None),
        class_def!(
            "Date",
            "DATE",
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        class_def!("Object", "MODULE_NS", None, None),
        class_def!(
            "Function",
            "C_FUNCTION",
            Some("crate::function::NativeFunctionValue::hook_finalizer"),
            Some("crate::function::NativeFunctionValue::hook_mark")
        ),
        class_def!(
            "Function",
            "BYTECODE_FUNCTION",
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "Function",
            "BOUND_FUNCTION",
            Some("crate::function::BoundFunctionValue::hook_finalizer"),
            Some("crate::function::BoundFunctionValue::hook_mark")
        ),
        class_def!(
            "Function",
            "C_FUNCTION_DATA",
            Some("crate::callable::CallableValue::hook_finalizer"),
            Some("crate::callable::CallableValue::hook_mark")
        ),
        class_def!(
            "GeneratorFunction",
            "GENERATOR_FUNCTION",
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "ForInIterator",
            "FOR_IN_ITERATOR",
            Some("crate::iterator::js_for_in_iterator_finalizer"),
            Some("crate::iterator::js_for_in_iterator_mark")
        ),
        class_def!(
            "RegExp",
            "REGEXP",
            Some("crate::regexp::js_regexp_finalizer"),
            None
        ),
        class_def!(
            "ArrayBuffer",
            "ARRAY_BUFFER",
            Some("crate::array_buffer::js_array_buffer_finalizer"),
            None
        ),
        class_def!(
            "SharedArrayBuffer",
            "SHARED_ARRAY_BUFFER",
            Some("crate::array_buffer::js_array_buffer_finalizer"),
            None
        ),
        //
        // start of typed arrays
        //
        class_def!(
            "Uint8ClampedArray",
            "UINT8C_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Int8Array",
            "INT8_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Uint8Array",
            "UINT8_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Int16Array",
            "INT16_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Uint16Array",
            "UINT16_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Int32Array",
            "INT32_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Uint32Array",
            "UINT32_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "BigInt64Array",
            "BIG_INT64_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "BigUint64Array",
            "BIG_UINT64_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Float32Array",
            "FLOAT32_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        class_def!(
            "Float64Array",
            "FLOAT64_ARRAY",
            Some("crate::typed_array::js_typed_array_finalizer"),
            Some("crate::typed_array::js_typed_array_mark")
        ),
        // end of typed arrays
        class_def!("DataView", "DATAVIEW", None, None),
        class_def!(
            "BigInt",
            "BIG_INT",
            Some("crate::value::js_object_data_finalizer"),
            Some("crate::value::js_object_data_mark")
        ),
        //
        //
        //
        class_def!(
            "Map",
            "MAP",
            Some("crate::map::js_map_finalizer"),
            Some("crate::map::js_map_mark")
        ),
        class_def!(
            "Set",
            "SET",
            Some("crate::map::js_map_finalizer"),
            Some("crate::map::js_map_mark")
        ),
        class_def!(
            "WeakMap",
            "WEAKMAP",
            Some("crate::map::js_map_finalizer"),
            Some("crate::map::js_map_mark")
        ),
        class_def!(
            "WeakSet",
            "WEAKSET",
            Some("crate::map::js_map_finalizer"),
            Some("crate::map::js_map_mark")
        ),
        class_def!(
            "Map_Iterator",
            "MAP_ITERATOR",
            Some("crate::map::js_map_iterator_finalizer"),
            Some("crate::map::js_map_iterator_mark")
        ),
        class_def!(
            "Set_Iterator",
            "SET_ITERATOR",
            Some("crate::map::js_map_iterator_finalizer"),
            Some("crate::map::js_map_iterator_mark")
        ),
        class_def!(
            "Array_Iterator",
            "ARRAY_ITERATOR",
            Some("crate::iterator::JSArrayIteratorData::hook_finalizer"),
            Some("crate::iterator::JSArrayIteratorData::hook_mark")
        ),
        class_def!(
            "String_Iterator",
            "STRING_ITERATOR",
            Some("crate::iterator::JSArrayIteratorData::hook_finalizer"),
            Some("crate::iterator::JSArrayIteratorData::hook_mark")
        ),
        class_def!(
            "RegExp_String_Iterator",
            "REGEXP_STRING_ITERATOR",
            Some("crate::regexp::js_regexp_iterator_finalizer"),
            Some("crate::regexp::js_regexp_iterator_mark")
        ),
        class_def!(
            "Generator",
            "GENERATOR",
            Some("crate::generator::js_generator_finalizer"),
            Some("crate::generator::js_generator_mark")
        ),
        //
        // Proxy
        //
        class_def!(
            "Proxy",
            "PROXY",
            Some("crate::proxy::js_proxy_finalizer"),
            Some("crate::proxy::js_proxy_mark")
        ),
        //
        // Async stuff
        //
        class_def!(
            "Promise",
            "PROMISE",
            Some("crate::promise::PromiseValue::hook_finalizer"),
            Some("crate::promise::PromiseValue::hook_mark")
        ),
        class_def!(
            "PromiseResolveFunction",
            "PROMISE_RESOLVE_FUNCTION",
            Some("crate::promise::PromiseResolvingFuncValue::hook_finalizer"),
            Some("crate::promise::PromiseResolvingFuncValue::hook_mark")
        ),
        class_def!(
            "PromiseRejectFunction",
            "PROMISE_REJECT_FUNCTION",
            Some("crate::promise::PromiseResolvingFuncValue::hook_finalizer"),
            Some("crate::promise::PromiseResolvingFuncValue::hook_mark")
        ),
        class_def!(
            "AsyncFunction",
            "ASYNC_FUNCTION",
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "AsyncFunctionResolve",
            "ASYNC_FUNCTION_RESOLVE",
            Some("crate::function::js_async_function_resolve_finalizer"),
            Some("crate::function::js_async_function_resolve_mark")
        ),
        class_def!(
            "AsyncFunctionReject",
            "ASYNC_FUNCTION_REJECT",
            Some("crate::function::js_async_function_resolve_finalizer"),
            Some("crate::function::js_async_function_resolve_mark")
        ),
        class_def!(
            "empty_string",
            "ASYNC_FROM_SYNC_ITERATOR",
            Some("crate::iterator::js_async_from_sync_iterator_finalizer"),
            Some("crate::iterator::js_async_from_sync_iterator_mark")
        ),
        class_def!(
            "AsyncGeneratorFunction",
            "ASYNC_GENERATOR_FUNCTION",
            Some("crate::function::js_bytecode_function_finalizer"),
            Some("crate::function::js_bytecode_function_mark")
        ),
        class_def!(
            "AsyncGeneratorValue",
            "ASYNC_GENERATOR",
            Some("crate::generator::AsyncGeneratorValue::hook_finalizer"),
            Some("crate::generator::AsyncGeneratorValue::hook_mark")
        ),
    ];

    let mut enums = Vec::<String>::with_capacity(128);
    let mut cdefs = Vec::<String>::with_capacity(128);
    let mut consts = Vec::<String>::with_capacity(128);

    let mut async_start: Option<usize> = None;
    let mut async_end: Option<usize> = None;

    enums.push(
        "#[repr(u32)]\n#[derive(Debug,Clone,Copy,PartialEq,Eq,PartialOrd,Ord)]\npub enum JSClassStdId {"
            .to_string(),
    );
    cdefs.push(format!(
        "pub const STD_CLASS_DEFS: [JSClassShortDef;{}] = [",
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

        if i == 0 {
            enums.push(format!("{}=1,", cdef.const_name));
        } else {
            enums.push(format!("{},", cdef.const_name));
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
        "pub const JS_CLASS_INIT_COUNT: JSClassID={};",
        class_defs.len() + 1
    ));

    enums.push("InitCount, // for count only".to_string());
    enums.push("}".to_string());
    cdefs.push("];".to_string());

    let lines = vec![
        "use crate::{atom::consts::*, class::{JSClassID, JSClassShortDef}};\n".to_string(),
        // enums.join("\n"),
        // "".to_string(),
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
