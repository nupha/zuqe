// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::missing_const_for_fn)]

mod array;
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
mod runtime;
mod stack_frame;
mod string;
mod utils;
mod value;

pub use {
    array::Array,
    atom::Atom,
    class::{ClassCtorDef, ClassDef, ClassID},
    context::{Context, ScopedContext},
    error::{ERR_OUT_OF_MEMORY, ERR_TOO_MANY_CLASSES, ErrorText, JS_SetUncatchableError, JSResult},
    eval::{EvalFlags, EvalType},
    function::{CallContext, Callback, FieldDef},
    module::ModuleDef,
    number::IntOrFloat,
    object::{
        JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_FORCE, JS_PROP_WRITABLE,
        Object,
    },
    runtime::{Runtime, RuntimeImpl},
    value::{JSValue, Value},
};

#[cfg(feature = "async_support")]
pub use runtime::RuntimeFuture;

mod stdcls {
    include!(concat!(env!("OUT_DIR"), "/std_class_def.rs"));
}
pub use stdcls::*;

pub mod builtins;

//
// channel helpers
//
#[cfg(not(feature = "crossbeam_channel"))]
mod channel {
    pub type Sender<D> = std::sync::mpsc::SyncSender<D>;
    pub type Receiver<D> = std::sync::mpsc::Receiver<D>;
    pub type TryRecvError = std::sync::mpsc::TryRecvError;
    //pub type SendError<T> = std::sync::mpsc::SendError<T>;

    pub fn make_bounded_channel<D: Send + 'static>(n: usize) -> (Sender<D>, Receiver<D>) {
        std::sync::mpsc::sync_channel(n)
    }
}
#[cfg(feature = "crossbeam_channel")]
mod channel {
    pub type Sender<D> = crossbeam::channel::Sender<D>;
    pub type Receiver<D> = crossbeam::channel::Receiver<D>;
    pub type TryRecvError = crossbeam::channel::TryRecvError;
    //pub type SendError<T> = crossbeam::channel::SendError<T>;

    pub fn make_bounded_channel<D: Send + 'static>(n: usize) -> (Sender<D>, Receiver<D>) {
        crossbeam::channel::bounded(n)
    }
}
pub(crate) use channel::*;

// re-export gc-lite
pub use gc_lite;
