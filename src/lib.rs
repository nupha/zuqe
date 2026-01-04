// SPDX-License-Identifier: Apache-2.0
// Copyright (c) 2025 John Ray <996351336@qq.com>

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::missing_const_for_fn)]

mod atom;
mod bytecode;
mod class;
mod context;
mod error;
mod eval;
mod function;
mod gc;
mod module;
mod number;
mod object;
mod parse;
mod resource;
mod runtime;
mod stack_frame;
mod string;
mod utils;
mod value;

pub mod builtins;

pub use {
    atom::Atom,
    class::{ClassDef, ClassID, StdClassDef},
    context::{Context, ContextBuilder},
    error::{ErrorText, JS_SetUncatchableError, JSResult},
    eval::{EvalFlags, EvalType},
    function::{CallContext, Callback, FieldDef},
    module::ModuleDef,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_WRITABLE, ObjectRef,
    },
    runtime::{Runtime, RuntimeImpl},
    value::JSValue,
};

mod stdcls {
    include!(concat!(env!("OUT_DIR"), "/std_class_def.rs"));
}
pub use stdcls::*;
