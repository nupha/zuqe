// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

pub mod arguments;
pub mod array;
pub mod array_buffer;
pub mod atomics;
#[cfg(feature = "big_int")]
pub mod bigint;
pub mod boolean;
pub(crate) mod common;
pub mod error;
pub mod function;
pub mod generator;
pub mod global;
pub mod iterator;
pub mod number;
pub mod object;
pub mod promise;
pub mod string;
pub mod symbol;
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
#[cfg(feature = "builtin_weakref")]
pub mod weakref;

use crate::{
    Atom, ClassCtorDef, class::ClassDef, context::ScopedContext, error::JSResult,
    function::CallContext, module::js_module_ns_exotic_methods,
};

//
// simple class defs
//

pub(crate) static CLASS_DEF_MODULE_NS: ClassDef =
    ClassDef::builtin(Atom::CONST_Object, &[], None).exotic(&js_module_ns_exotic_methods);

#[cfg(feature = "big_int")]
pub(crate) static CLASS_DEF_BIG_INT: ClassDef = ClassDef::builtin(
    Atom::CONST_BigInt,
    bigint::js_bigint_proto_funcs,
    Some(
        ClassCtorDef::new(bigint::js_bigint_constructor, 1, true)
            .entries(bigint::js_bigint_funcs)
            .expose_to_global(),
    ),
);

#[cfg(not(feature = "big_int"))]
pub(crate) static CLASS_DEF_BIG_INT: ClassDef = ClassDef::builtin(Atom::CONST_BigInt, &[], None);

//
// common proto methods
//
pub(crate) const fn js_get_this(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    Ok(cc.this())
}
