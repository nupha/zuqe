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
mod microtask;
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
    error::{ErrorKind, Exception, ExceptionText, JS_SetUncatchableError, JSResult},
    eval::{EvalFlags, EvalType},
    function::{CallContext, Callback, FieldDef},
    microtask::*,
    module::ModuleDef,
    number::IntOrFloat,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_INDEX_MASK, JS_PROP_C_W_E, JS_PROP_CONFIGURABLE,
        JS_PROP_ENUMERABLE, JS_PROP_FORCE, JS_PROP_WRITABLE, Object,
    },
    runtime::{Runtime, RuntimeImpl},
    value::{JSValue, Value},
};

#[cfg(feature = "async_support")]
pub use runtime::RunFuture;

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
    pub type TrySendError<T> = std::sync::mpsc::TrySendError<T>;

    pub fn make_bounded_channel<D: Send + 'static>(n: usize) -> (Sender<D>, Receiver<D>) {
        std::sync::mpsc::sync_channel(n)
    }
}
#[cfg(feature = "crossbeam_channel")]
mod channel {
    pub type Sender<D> = crossbeam::channel::Sender<D>;
    pub type Receiver<D> = crossbeam::channel::Receiver<D>;
    pub type TryRecvError = crossbeam::channel::TryRecvError;
    pub type TrySendError<T> = crossbeam::channel::TrySendError<T>;

    pub fn make_bounded_channel<D: Send + 'static>(n: usize) -> (Sender<D>, Receiver<D>) {
        crossbeam::channel::bounded(n)
    }
}
pub(crate) use channel::*;

#[cfg(feature = "async_support")]
mod async_sender {
    use std::{
        pin::Pin,
        task::{Context as TaskContext, Poll},
    };

    use super::*;

    pub struct PollSender<D>(Sender<D>);

    impl<D> PollSender<D> {
        pub const fn new(sender: Sender<D>) -> Self {
            Self(sender)
        }

        pub fn send_async(&self, value: D) -> SendAsyncFut<D> {
            SendAsyncFut::new(self.0.clone(), value)
        }
    }

    impl<D> From<crate::Sender<D>> for PollSender<D> {
        fn from(sender: crate::Sender<D>) -> Self {
            Self(sender)
        }
    }

    pub struct SendAsyncFut<D> {
        sender: crate::Sender<D>,
        value: Option<D>,
    }

    impl<D> SendAsyncFut<D> {
        pub const fn new(sender: crate::Sender<D>, value: D) -> Self {
            Self {
                sender,
                value: Some(value),
            }
        }
    }

    impl<D: Unpin> Future for SendAsyncFut<D> {
        type Output = Result<(), D>;

        fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let value = this.value.take().expect("PollSender polled after Ready");

            match this.sender.try_send(value) {
                Ok(_) => Poll::Ready(Ok(())),
                Err(crate::TrySendError::Full(v)) => {
                    this.value = Some(v);
                    // Re-schedule for re-polling on the next executor tick.
                    cx.waker().wake_by_ref();
                    Poll::Pending
                }
                Err(crate::TrySendError::Disconnected(v)) => Poll::Ready(Err(v)),
            }
        }
    }
}
#[cfg(feature = "async_support")]
pub use async_sender::*;

// re-export gc-lite
pub use gc_lite;
