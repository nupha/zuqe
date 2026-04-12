// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

pub mod array;
pub mod array_buffer;
pub mod boolean;
pub mod error;
pub mod function;
pub mod generator;
pub mod global;
pub mod iterator;
pub mod number;
pub mod object;
pub mod promise;
pub mod string;
pub mod typed_array;

#[cfg(feature = "builtin_date")]
pub mod date;
#[cfg(feature = "builtin_json")]
pub mod json;
#[cfg(feature = "builtin_mapset")]
pub mod mapset;
#[cfg(feature = "builtin_math")]
pub mod math;
#[cfg(feature = "builtin_proxy")]
pub mod proxy;
#[cfg(feature = "builtin_reflect")]
pub mod reflect;
#[cfg(feature = "builtin_regexp")]
pub mod regexp;
#[cfg(feature = "builtin_symbol")]
pub mod symbol;

use crate::{
    Atom,
    class::{ClassDef, ClassInitType, js_arguments_exotic_methods},
    context::ScopedContext,
    error::JSResult,
    function::CallContext,
    module::js_module_ns_exotic_methods,
};

//
// simple class defs
//

pub const CLASS_DEF_ARGUMENTS: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Arguments,
    proto_entries: &[],
    ctor_entries: &[],
    call: None,
    exotic: Some(&js_arguments_exotic_methods),
    initialize: ClassInitType::None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_MAPPED_ARGUMENTS: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Arguments,
    proto_entries: &[],
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_MODULE_NS: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_Object,
    proto_entries: &[],
    ctor_entries: &[],
    call: None,
    exotic: Some(&js_module_ns_exotic_methods),
    initialize: ClassInitType::None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

pub const CLASS_DEF_BIG_INT: ClassDef = ClassDef {
    name: "",
    name_atom: Atom::CONST_BigInt,
    proto_entries: &[],
    ctor_entries: &[],
    call: None,
    exotic: None,
    initialize: ClassInitType::None,
    finalize: None,
    finalize_opaque: None,
    unregister: None,
};

//
// common proto methods
//
pub(crate) const fn js_get_this(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    Ok(cc.this())
}
