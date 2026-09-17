// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::path::Path;

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
    #[cfg(feature = "big_int")]
    BigInt,
    // Intl classes are grouped contiguously so that their class ids form a
    // single `..=` range. All gated behind the `intl402` feature.
    #[cfg(feature = "intl402")]
    IntlCollator,
    #[cfg(feature = "intl402")]
    IntlDateTimeFormat,
    #[cfg(feature = "intl402")]
    IntlDurationFormat,
    #[cfg(feature = "intl402")]
    IntlDisplayNames,
    #[cfg(feature = "intl402")]
    IntlListFormat,
    #[cfg(feature = "intl402")]
    IntlLocale,
    #[cfg(feature = "intl402")]
    IntlNumberFormat,
    #[cfg(feature = "intl402")]
    IntlPluralRules,
    #[cfg(feature = "intl402")]
    IntlRelativeTimeFormat,
    #[cfg(feature = "intl402")]
    IntlSegmenter,
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
            #[cfg(feature = "big_int")]
            PayloadKey::BigInt => "bigint",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlCollator => "intl_collator",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlDateTimeFormat => "intl_date_time_format",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlDurationFormat => "intl_duration_format",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlDisplayNames => "intl_display_names",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlListFormat => "intl_list_format",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlLocale => "intl_locale",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlNumberFormat => "intl_number_format",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlPluralRules => "intl_plural_rules",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlRelativeTimeFormat => "intl_relative_time_format",
            #[cfg(feature = "intl402")]
            PayloadKey::IntlSegmenter => "intl_segmenter",
        }
    }

    /// Rust type path for this payload kind, used by codegen to emit
    /// `drop_payload_fn::<T>`, `trace_payload` impls, etc.
    /// Returns `None` for `PayloadKey::None` (payload-less classes).
    pub fn type_str(&self) -> Option<&'static str> {
        match self {
            PayloadKey::None => None,
            PayloadKey::Array => Some("crate::array::ArrayPayload"),
            PayloadKey::Bool => Some("bool"),
            PayloadKey::String => Some("crate::string::StringRef"),
            PayloadKey::Value => Some("crate::value::JSValue"),
            PayloadKey::NativeFunc => Some("Box<crate::function::NFuncPayload>"),
            PayloadKey::BcFunction => Some("Box<crate::function::BFuncPayload>"),
            PayloadKey::BoundFunction => Some("Box<crate::function::BoundFunctionPayload>"),
            #[cfg(feature = "builtin_regexp")]
            PayloadKey::RegExp => Some("Box<crate::builtins::regexp::RegExpData>"),
            PayloadKey::ForInIterator => Some("Box<crate::builtins::iterator::ForInIterPayload>"),
            PayloadKey::ArrayBuffer => {
                Some("Box<crate::builtins::array_buffer::ArrayBufferPayload>")
            }
            PayloadKey::TypedArray => Some("Box<crate::builtins::typed_array::TypedArrayPayload>"),
            #[cfg(feature = "big_int")]
            PayloadKey::BigInt => Some("crate::bigint::BigIntRef"),
            #[cfg(feature = "intl402")]
            PayloadKey::IntlCollator => Some("Box<crate::builtins::intl::collator::CollatorData>"),
            #[cfg(feature = "intl402")]
            PayloadKey::IntlDateTimeFormat => {
                Some("Box<crate::builtins::intl::date_time_format::DateTimeFormatData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlDurationFormat => {
                Some("Box<crate::builtins::intl::duration_format::DurationFormatData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlDisplayNames => {
                Some("Box<crate::builtins::intl::display_names::DisplayNamesData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlListFormat => {
                Some("Box<crate::builtins::intl::list_format::ListFormatData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlLocale => Some("Box<crate::builtins::intl::locale::LocaleData>"),
            #[cfg(feature = "intl402")]
            PayloadKey::IntlNumberFormat => {
                Some("Box<crate::builtins::intl::number_format::NumberFormatData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlPluralRules => {
                Some("Box<crate::builtins::intl::plural_rules::PluralRulesData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlRelativeTimeFormat => {
                Some("Box<crate::builtins::intl::relative_time_format::RelativeTimeFormatData>")
            }
            #[cfg(feature = "intl402")]
            PayloadKey::IntlSegmenter => {
                Some("Box<crate::builtins::intl::segmenter::SegmenterData>")
            }
            PayloadKey::DataView => Some("Box<crate::builtins::typed_array::DataViewPayload>"),
            #[cfg(feature = "builtin_mapset")]
            PayloadKey::MapSet => Some("Box<crate::builtins::mapset::JSMapState>"),
            #[cfg(feature = "builtin_mapset")]
            PayloadKey::MapSetIterator => Some("Box<crate::builtins::mapset::JSMapIteratorData>"),
            PayloadKey::ArrayIterator => Some("Box<crate::builtins::iterator::ArrayIterPayload>"),
            #[cfg(feature = "builtin_regexp")]
            PayloadKey::RegExpStringIterator => {
                Some("Box<crate::builtins::regexp::RegExpStringIterPayload>")
            }
            PayloadKey::Generator => Some("Box<crate::builtins::generator::GeneratorPayload>"),
            #[cfg(feature = "builtin_proxy")]
            PayloadKey::Proxy => Some("Box<crate::builtins::proxy::ProxyPayload>"),
            PayloadKey::Promise => Some("Box<crate::builtins::promise::PromisePayload>"),
            PayloadKey::PromiseResolvingFunction => {
                Some("Box<crate::builtins::promise::PromiseResolvePayload>")
            }
            PayloadKey::AsyncFunctionResolving => Some("crate::function::AsyncState"),
            PayloadKey::AsyncFromSyncIterator => {
                Some("Box<crate::builtins::iterator::JSAsyncFromSyncIteratorData>")
            }
            PayloadKey::AsyncGenerator => Some("Box<crate::builtins::generator::AGenPayload>"),
            PayloadKey::AsyncIterableIterator => {
                Some("Box<crate::builtins::iterator::AsyncIterPayload>")
            }
            PayloadKey::IteratorHelper => {
                Some("Box<crate::builtins::iterator::IteratorHelperData>")
            }
            PayloadKey::IteratorWrap => Some("Box<crate::builtins::iterator::IteratorWrapData>"),
            PayloadKey::IteratorConcat => {
                Some("Box<crate::builtins::iterator::IteratorConcatData>")
            }
            PayloadKey::IteratorZip => Some("Box<crate::builtins::iterator::IteratorZipData>"),
            #[cfg(feature = "builtin_date")]
            PayloadKey::Datetime => Some("crate::builtins::date::Datetime"),
            #[cfg(feature = "builtin_weakref")]
            PayloadKey::WeakRef => Some("Box<crate::builtins::weakref::WeakRefData>"),
            #[cfg(feature = "builtin_weakref")]
            PayloadKey::FinalizationRegistry => {
                Some("Box<crate::builtins::weakref::FinalizationRegistryData>")
            }
        }
    }
}

impl std::fmt::Display for PayloadKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug)]
struct BuiltinClassInfo {
    const_name: &'static str,
    class_def: &'static str,
    key: PayloadKey,
    narrow: Option<&'static str>,
}

impl BuiltinClassInfo {
    const fn new(const_name: &'static str, class_def: &'static str, key: PayloadKey) -> Self {
        BuiltinClassInfo {
            const_name,
            class_def,
            key,
            narrow: None,
        }
    }

    const fn new_narrow(
        const_name: &'static str,
        class_def: &'static str,
        key: PayloadKey,
        narrow: &'static str,
    ) -> Self {
        BuiltinClassInfo {
            const_name,
            class_def,
            key,
            narrow: Some(narrow),
        }
    }
}

const BUILTIN_CLASS_DEF_LIST: &[BuiltinClassInfo] = &[
    // CORE CLASSES START
    BuiltinClassInfo::new(
        "OBJECT",
        "crate::builtins::object::CLASS_DEF_OBJECT",
        PayloadKey::None,
    ),
    BuiltinClassInfo::new_narrow(
        "ARRAY",
        "crate::builtins::array::CLASS_DEF_ARRAY",
        PayloadKey::Array,
        "Array",
    ),
    BuiltinClassInfo::new(
        "ERROR",
        "crate::builtins::error::CLASS_DEF_ERROR",
        PayloadKey::None,
    ),
    BuiltinClassInfo::new(
        "NUMBER",
        "crate::builtins::number::CLASS_DEF_NUMBER",
        PayloadKey::Value,
    ),
    BuiltinClassInfo::new(
        "BOOLEAN",
        "crate::builtins::boolean::CLASS_DEF_BOOLEAN",
        PayloadKey::Bool,
    ),
    BuiltinClassInfo::new(
        "STRING",
        "crate::builtins::string::CLASS_DEF_STRING",
        PayloadKey::String,
    ),
    BuiltinClassInfo::new(
        "SYMBOL",
        "crate::builtins::symbol::CLASS_DEF_SYMBOL",
        PayloadKey::String,
    ),
    BuiltinClassInfo::new_narrow(
        "ARGUMENTS",
        "crate::builtins::arguments::CLASS_DEF_ARGUMENTS",
        PayloadKey::Array,
        "Array",
    ),
    BuiltinClassInfo::new(
        "MAPPED_ARGUMENTS",
        "crate::builtins::arguments::CLASS_DEF_MAPPED_ARGUMENTS",
        PayloadKey::None,
    ),
    #[cfg(feature = "builtin_date")]
    BuiltinClassInfo::new(
        "DATE",
        "crate::builtins::date::CLASS_DEF_DATE",
        PayloadKey::Datetime,
    ),
    // CORE CLASSES END
    BuiltinClassInfo::new(
        "MODULE_NS",
        "crate::builtins::CLASS_DEF_MODULE_NS",
        PayloadKey::None,
    ),
    BuiltinClassInfo::new_narrow(
        "C_FUNCTION",
        "crate::builtins::function::CLASS_DEF_C_FUNCTION",
        PayloadKey::NativeFunc,
        "NativeFunction",
    ),
    BuiltinClassInfo::new_narrow(
        "BYTECODE_FUNCTION",
        "crate::builtins::function::CLASS_DEF_BYTECODE_FUNCTION",
        PayloadKey::BcFunction,
        "BytecodeFunction",
    ),
    BuiltinClassInfo::new_narrow(
        "BOUND_FUNCTION",
        "crate::builtins::function::CLASS_DEF_BOUND_FUNCTION",
        PayloadKey::BoundFunction,
        "BoundFunction",
    ),
    BuiltinClassInfo::new_narrow(
        "C_FUNCTION_DATA",
        "crate::builtins::function::CLASS_DEF_C_FUNCTION_DATA",
        PayloadKey::NativeFunc,
        "NativeFunction",
    ),
    BuiltinClassInfo::new(
        "FOR_IN_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_FOR_IN_ITERATOR",
        PayloadKey::ForInIterator,
    ),
    #[cfg(feature = "builtin_regexp")]
    BuiltinClassInfo::new_narrow(
        "REGEXP",
        "crate::builtins::regexp::CLASS_DEF_REGEXP",
        PayloadKey::RegExp,
        "RegExp",
    ),
    BuiltinClassInfo::new_narrow(
        "ARRAY_BUFFER",
        "crate::builtins::array_buffer::CLASS_DEF_ARRAY_BUFFER",
        PayloadKey::ArrayBuffer,
        "ArrayBuffer",
    ),
    BuiltinClassInfo::new_narrow(
        "SHARED_ARRAY_BUFFER",
        "crate::builtins::array_buffer::CLASS_DEF_SHARED_ARRAY_BUFFER",
        PayloadKey::ArrayBuffer,
        "ArrayBuffer",
    ),
    //
    // start of typed arrays
    //
    BuiltinClassInfo::new_narrow(
        "UINT8C_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT8C_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "INT8_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT8_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "UINT8_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT8_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "INT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT16_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "UINT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT16_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "INT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_INT32_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "UINT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_UINT32_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "BIG_INT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_BIG_INT64_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "BIG_UINT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_BIG_UINT64_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "FLOAT16_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT16_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "FLOAT32_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT32_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    BuiltinClassInfo::new_narrow(
        "FLOAT64_ARRAY",
        "crate::builtins::typed_array::CLASS_DEF_FLOAT64_ARRAY",
        PayloadKey::TypedArray,
        "TypedArray",
    ),
    // end of typed arrays
    BuiltinClassInfo::new_narrow(
        "DATAVIEW",
        "crate::builtins::typed_array::CLASS_DEF_DATAVIEW",
        PayloadKey::DataView,
        "DataView",
    ),
    #[cfg(feature = "big_int")]
    BuiltinClassInfo::new(
        "BIG_INT",
        "crate::builtins::CLASS_DEF_BIG_INT",
        PayloadKey::BigInt,
    ),
    #[cfg(not(feature = "big_int"))]
    BuiltinClassInfo::new(
        "BIG_INT",
        "crate::builtins::CLASS_DEF_BIG_INT",
        PayloadKey::None,
    ),
    #[cfg(feature = "builtin_json")]
    BuiltinClassInfo::new(
        "RAWJSON",
        "crate::builtins::json::CLASS_DEF_RAWJSON",
        PayloadKey::None,
    ),
    //
    // map set and iterators
    //
    #[cfg(feature = "builtin_mapset")]
    BuiltinClassInfo::new_narrow(
        "MAP",
        "crate::builtins::mapset::CLASS_DEF_MAP",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    BuiltinClassInfo::new_narrow(
        "SET",
        "crate::builtins::mapset::CLASS_DEF_SET",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    BuiltinClassInfo::new_narrow(
        "WEAKMAP",
        "crate::builtins::mapset::CLASS_DEF_WEAK_MAP",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    BuiltinClassInfo::new_narrow(
        "WEAKSET",
        "crate::builtins::mapset::CLASS_DEF_WEAK_SET",
        PayloadKey::MapSet,
        "MapSet",
    ),
    #[cfg(feature = "builtin_mapset")]
    BuiltinClassInfo::new_narrow(
        "MAP_ITERATOR",
        "crate::builtins::mapset::CLASS_DEF_MAP_ITERATOR",
        PayloadKey::MapSetIterator,
        "MapIterator",
    ),
    #[cfg(feature = "builtin_mapset")]
    BuiltinClassInfo::new(
        "SET_ITERATOR",
        "crate::builtins::mapset::CLASS_DEF_SET_ITERATOR",
        PayloadKey::MapSetIterator,
    ),
    BuiltinClassInfo::new(
        "ARRAY_ITERATOR",
        "crate::builtins::array::CLASS_DEF_ARRAY_ITERATOR",
        PayloadKey::ArrayIterator,
    ),
    BuiltinClassInfo::new(
        "STRING_ITERATOR",
        "crate::builtins::string::CLASS_DEF_STRING_ITERATOR",
        PayloadKey::ArrayIterator,
    ),
    #[cfg(feature = "builtin_regexp")]
    BuiltinClassInfo::new(
        "REGEXP_STRING_ITERATOR",
        "crate::builtins::regexp::CLASS_DEF_REGEXP_STRING_ITERATOR",
        PayloadKey::RegExpStringIterator,
    ),
    //
    //
    //
    BuiltinClassInfo::new(
        "GENERATOR",
        "crate::builtins::generator::CLASS_DEF_GENERATOR",
        PayloadKey::Generator,
    ),
    BuiltinClassInfo::new_narrow(
        "GENERATOR_FUNCTION",
        "crate::builtins::generator::CLASS_DEF_GENERATOR_FUNCTION",
        PayloadKey::BcFunction,
        "GeneratorFunction",
    ),
    //
    // Proxy
    //
    #[cfg(feature = "builtin_proxy")]
    BuiltinClassInfo::new_narrow(
        "PROXY",
        "crate::builtins::proxy::CLASS_DEF_PROXY",
        PayloadKey::Proxy,
        "Proxy",
    ),
    //
    // Async stuff
    //
    BuiltinClassInfo::new_narrow(
        "PROMISE",
        "crate::builtins::promise::CLASS_DEF_PROMISE",
        PayloadKey::Promise,
        "Promise",
    ),
    BuiltinClassInfo::new_narrow(
        "PROMISE_RESOLVE_FUNCTION",
        "crate::builtins::promise::CLASS_DEF_PROMISE_RESOLVE_FUNCTION",
        PayloadKey::PromiseResolvingFunction,
        "PromiseResolveFunc",
    ),
    BuiltinClassInfo::new_narrow(
        "PROMISE_REJECT_FUNCTION",
        "crate::builtins::promise::CLASS_DEF_PROMISE_REJECT_FUNCTION",
        PayloadKey::PromiseResolvingFunction,
        "PromiseResolveFunc",
    ),
    BuiltinClassInfo::new_narrow(
        "ASYNC_FUNCTION",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION",
        PayloadKey::BcFunction,
        "AsyncFunction",
    ),
    BuiltinClassInfo::new(
        "ASYNC_FUNCTION_RESOLVE",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION_RESOLVE",
        PayloadKey::AsyncFunctionResolving,
    ),
    BuiltinClassInfo::new(
        "ASYNC_FUNCTION_REJECT",
        "crate::builtins::function::CLASS_DEF_ASYNC_FUNCTION_REJECT",
        PayloadKey::AsyncFunctionResolving,
    ),
    BuiltinClassInfo::new_narrow(
        "ASYNC_FROM_SYNC_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ASYNC_FROM_SYNC_ITERATOR",
        PayloadKey::AsyncFromSyncIterator,
        "AsyncFromSyncIterator",
    ),
    BuiltinClassInfo::new(
        "ASYNC_GENERATOR",
        "crate::builtins::generator::CLASS_DEF_ASYNC_GENERATOR",
        PayloadKey::AsyncGenerator,
    ),
    BuiltinClassInfo::new_narrow(
        "ASYNC_GENERATOR_FUNCTION", // depends on ASYNC_GENERATOR
        "crate::builtins::generator::CLASS_DEF_ASYNC_GENERATOR_FUNCTION",
        PayloadKey::BcFunction,
        "AsyncGeneratorFunction",
    ),
    BuiltinClassInfo::new_narrow(
        "ASYNC_ITERABLE_ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ASYNC_ITERABLE_ITERATOR",
        PayloadKey::AsyncIterableIterator,
        "AsyncIterableIterator",
    ),
    //
    // ES2024+ Iterator global object
    //
    BuiltinClassInfo::new(
        "ITERATOR",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR",
        PayloadKey::None,
    ),
    BuiltinClassInfo::new(
        "ITERATOR_HELPER",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_HELPER",
        PayloadKey::IteratorHelper,
    ),
    BuiltinClassInfo::new(
        "ITERATOR_WRAP",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_WRAP",
        PayloadKey::IteratorWrap,
    ),
    BuiltinClassInfo::new(
        "ITERATOR_CONCAT",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_CONCAT",
        PayloadKey::IteratorConcat,
    ),
    BuiltinClassInfo::new(
        "ITERATOR_ZIP",
        "crate::builtins::iterator::CLASS_DEF_ITERATOR_ZIP",
        PayloadKey::IteratorZip,
    ),
    #[cfg(feature = "builtin_weakref")]
    BuiltinClassInfo::new(
        "WEAKREF",
        "crate::builtins::weakref::CLASS_DEF_WEAKREF",
        PayloadKey::WeakRef,
    ),
    #[cfg(feature = "builtin_weakref")]
    BuiltinClassInfo::new(
        "FINALIZATION_REGISTRY",
        "crate::builtins::weakref::CLASS_DEF_FINALIZATION_REGISTRY",
        PayloadKey::FinalizationRegistry,
    ),
    //
    // Intl classes. Kept contiguous (and all behind `intl402`) so their class
    // ids form the contiguous range `JS_CLASS_INTL_COLLATOR ..= JS_CLASS_INTL_SEGMENTER`.
    //
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_COLLATOR",
        "crate::builtins::intl::collator::CLASS_DEF_COLLATOR",
        PayloadKey::IntlCollator,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_DATE_TIME_FORMAT",
        "crate::builtins::intl::date_time_format::CLASS_DEF_DATE_TIME_FORMAT",
        PayloadKey::IntlDateTimeFormat,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_DURATION_FORMAT",
        "crate::builtins::intl::duration_format::CLASS_DEF_DURATION_FORMAT",
        PayloadKey::IntlDurationFormat,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_DISPLAY_NAMES",
        "crate::builtins::intl::display_names::CLASS_DEF_DISPLAY_NAMES",
        PayloadKey::IntlDisplayNames,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_LIST_FORMAT",
        "crate::builtins::intl::list_format::CLASS_DEF_LIST_FORMAT",
        PayloadKey::IntlListFormat,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_LOCALE",
        "crate::builtins::intl::locale::CLASS_DEF_LOCALE",
        PayloadKey::IntlLocale,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_NUMBER_FORMAT",
        "crate::builtins::intl::number_format::CLASS_DEF_NUMBER_FORMAT",
        PayloadKey::IntlNumberFormat,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_PLURAL_RULES",
        "crate::builtins::intl::plural_rules::CLASS_DEF_PLURAL_RULES",
        PayloadKey::IntlPluralRules,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_RELATIVE_TIME_FORMAT",
        "crate::builtins::intl::relative_time_format::CLASS_DEF_RELATIVE_TIME_FORMAT",
        PayloadKey::IntlRelativeTimeFormat,
    ),
    #[cfg(feature = "intl402")]
    BuiltinClassInfo::new(
        "INTL_SEGMENTER",
        "crate::builtins::intl::segmenter::CLASS_DEF_SEGMENTER",
        PayloadKey::IntlSegmenter,
    ),
];

pub fn gen_std_class_defs(outfile: &Path) {
    let mut consts = Vec::<String>::with_capacity(128);

    let mut async_start: Option<usize> = None;
    let mut async_end: Option<usize> = None;

    let mut builtin_defs = Vec::<String>::with_capacity(BUILTIN_CLASS_DEF_LIST.len());

    for (i, cdef) in BUILTIN_CLASS_DEF_LIST.iter().enumerate() {
        match cdef.const_name {
            "PROMISE" => {
                async_start = Some(i);
            }
            "ASYNC_ITERABLE_ITERATOR" => {
                async_end = Some(i);
            }
            _ => {}
        }

        consts.push(format!(
            "pub const JS_CLASS_{}:ClassID={};",
            cdef.const_name,
            i + 1
        ));

        // Each class generates a const `<Struct>_DEF` via the def_native_object!
        // macro (paste); BUILTIN_CLASS_DEFS indexes them in id order.
        builtin_defs.push(format!("    {}_DEF,", cdef.class_def));
    }

    consts.push(format!(
        "pub const HOST_CLASS_ID_START: ClassID={};",
        BUILTIN_CLASS_DEF_LIST.len() + 1
    ));

    let mut lines = vec![
        "use crate::class::ClassID;\n".to_string(),
        "use crate::class::ClassDef;".to_string(),
        consts.join("\n"),
        String::new(),
        "/// Builtin class definitions, indexed by `JS_CLASS_*` id (id = index).".to_string(),
        "pub static BUILTIN_CLASS_DEFS: &[ClassDef] = &[".to_string(),
    ];
    lines.extend(builtin_defs);
    lines.push("];".to_string());
    lines.push("// end of std class".to_string());
    lines.push(format!(
        "pub const STD_CLASS_DEFS_ASYNC_INDEX_RANGE: std::ops::RangeInclusive<usize> = {}..={};",
        async_start.unwrap(),
        async_end.unwrap()
    ));

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

pub fn gen_payload_accessors(outfile: &Path) {
    // Build grouped accessor methods (same grouping as before, but on ObjectImpl)
    let mut trace_arms = Vec::<String>::new();
    let mut name_arms = Vec::<String>::new();
    let mut get_payload_methods = Vec::<String>::new();

    // Collect unique (PayloadKey, type_str) pairs from BUILTIN_CLASS_DEF_LIST
    let mut seen = std::collections::BTreeSet::<PayloadKey>::new();
    let mut keys_with_types = Vec::<(PayloadKey, &str)>::new();
    for cdef in BUILTIN_CLASS_DEF_LIST {
        if cdef.key == PayloadKey::None || !seen.insert(cdef.key) {
            continue;
        }
        if let Some(pty) = cdef.key.type_str() {
            keys_with_types.push((cdef.key, pty));
        }
    }

    for (k, pty) in &keys_with_types {
        let class_ids = BUILTIN_CLASS_DEF_LIST
            .iter()
            .filter_map(|c| {
                if c.key == *k {
                    Some(format!("crate::JS_CLASS_{}", c.const_name))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        if class_ids.is_empty() {
            continue;
        }

        let inner_ty = if pty.starts_with("Box<") && pty.ends_with(">") {
            &pty[4..pty.len() - 1]
        } else {
            pty
        };
        let class_id_pattern = class_ids.join(" | ");

        trace_arms.push(format!(
            "{class_id_pattern} => {{ (*(ptr as *const {inner_ty})).trace(tr); }}"
        ));

        name_arms.push(format!(
            "{class_id_pattern} => Some(std::any::type_name::<{inner_ty}>()),"
        ));

        get_payload_methods.push(format!(
            "#[inline]\npub const fn payload_{k}(&self) -> Option<&{inner_ty}> {{ \
             match self.class_id() {{ {class_id_pattern} => Some(unsafe {{ &*(self.payload as *const {inner_ty}) }}), _ => None }} }}"
        ));
        get_payload_methods.push(format!(
            "#[inline]\npub fn payload_{k}_mut(&mut self) -> Option<&mut {inner_ty}> {{ \
             match self.class_id() {{ {class_id_pattern} => Some(unsafe {{ &mut *(self.payload as *mut {inner_ty}) }}), _ => None }} }}"
        ));
    }

    let lines: Vec<String> = vec![
        "// @generated by build_stdclass.rs gen_payload_accessors. DO NOT EDIT.".into(),
        String::new(),
        "pub(crate) fn trace_payload(class_id: ClassID, ptr: *const c_void, tr: &mut gc_lite::GcTraceCtx) {".into(),
        "    if ptr.is_null() { return; }".into(),
        "    unsafe {".into(),
        "        match class_id {".into(),
        format!("            {},", trace_arms.join("\n            ")),
        "            _ => debug_assert!(ptr.is_null(), \"trace_payload: class {class_id} has no registered payload type\"),".into(),
        "        }".into(),
        "    }".into(),
        "}".into(),
        String::new(),
        // Debug-only type ledger: maps class_id -> the inner payload type the
        // drop/trace dispatch expects. `PayloadPtr::new` asserts call sites box
        // exactly this type, so a mismatched T fails loudly instead of turning
        // into UB at drop time.
        "#[cfg(debug_assertions)]".into(),
        "pub(crate) fn payload_type_name(class_id: ClassID) -> Option<&'static str> {".into(),
        "    match class_id {".into(),
        format!("        {}", name_arms.join("\n        ")),
        "        _ => None".into(),
        "    }".into(),
        "}".into(),
        String::new(),
        "impl crate::object::ObjectImpl {".into(),
        get_payload_methods.join("\n"),
        "}".into(),
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
/// types. The set of narrow types is aggregated from `BUILTIN_CLASS_DEF_LIST`: every
/// class whose `BuiltinClassInfo.narrow` is `Some(name)` joins the named narrow
/// type. `BCWrapper` is the single exception: it groups four function classes
/// (BYTECODE_FUNCTION, GENERATOR_FUNCTION, ASYNC_FUNCTION,
/// ASYNC_GENERATOR_FUNCTION) that each already map to their own narrow type,
/// so it is hand-written in `src/object/casts.rs` instead of here.
///
/// Emits `$OUT_DIR/object_casts.rs` which is `include!`d from
/// `src/object/casts.rs` (inside `crate::object`, where `Object`, `ObjectImpl`,
/// `JSValue`, `GcRef` and the `object_cast!` macro are all in scope).
///
/// Feature handling is implicit: `BUILTIN_CLASS_DEF_LIST` entries are gated with
/// `#[cfg(feature)]`, so when a feature is off its classes are not present in
/// the list and the corresponding narrow type is simply not generated. Each
/// generated struct is a transparent, `Copy` wrapper with a `pub(crate)` GcRef
/// field so inherent-method `impl` blocks in the original modules can still
/// access `.0`.
pub fn gen_object_casts(outfile: &Path) {
    let mut groups: std::collections::BTreeMap<&str, Vec<&BuiltinClassInfo>> =
        std::collections::BTreeMap::new();
    for cdef in BUILTIN_CLASS_DEF_LIST {
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
