// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(clippy::missing_safety_doc)]
#![warn(clippy::missing_const_for_fn)]

mod array;
mod atom;
#[cfg(feature = "big_int")]
mod bigint;
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
#[cfg(feature = "persistent")]
pub mod persistent;
mod runtime;
mod stack_frame;
mod string;
mod utils;
mod value;
#[cfg(feature = "zapi")]
mod zapi;

pub use {
    array::Array,
    atom::Atom,
    class::{ClassCtorDef, ClassDef, ClassID, NativeObject},
    context::{Context, ContextImpl, ScopeResult, ScopedContext},
    error::{ErrCode, ErrorKind, Exception, ExceptionText, ExitStatus, JSResult},
    eval::{EvalFlags, EvalName, EvalType},
    function::{BoundFunction, Bytecode, CallContext, Callback, FieldDef, IteratorState},
    microtask::*,
    module::{ModuleDef, path_to_url, url_to_abspath},
    number::IntOrFloat,
    object::{
        JS_GPN_ENUM_ONLY, JS_GPN_STR_OR_INT_MASK, JS_GPN_SYMBOL_MASK, JS_PROP_C_W_E,
        JS_PROP_CONFIGURABLE, JS_PROP_ENUMERABLE, JS_PROP_FORCE, JS_PROP_GETSET, JS_PROP_WRITABLE,
        Object, drop_payload_fn, user_payload_trace,
    },
    runtime::{DeadlineWaker, Runtime, RuntimeImpl, WakeHandle},
    value::JSValue,
};

#[cfg(feature = "async_support")]
pub use runtime::RunFuture;

pub(crate) use value::JSValueTag;

mod stdcls {
    include!(concat!(env!("OUT_DIR"), "/std_class_def.rs"));
}
pub use stdcls::*;

pub mod builtins;
pub use builtins::iterator::JSIteratorKindEnum;

//
// channel helpers
//
#[cfg(not(feature = "crossbeam_channel"))]
mod channel {
    pub type Sender<D> = std::sync::mpsc::SyncSender<D>;
    pub type Receiver<D> = std::sync::mpsc::Receiver<D>;
    pub type TryRecvError = std::sync::mpsc::TryRecvError;
    pub type TrySendError<T> = std::sync::mpsc::TrySendError<T>;
    pub type SendError<T> = std::sync::mpsc::SendError<T>;

    pub fn make_bounded_channel<D: Send + 'static>(n: usize) -> (Sender<D>, Receiver<D>) {
        std::sync::mpsc::sync_channel(n)
    }

    /// The std backend has no unbounded `SyncSender`; map to a 0-capacity
    /// rendezvous channel (send blocks until the receiver takes it).
    pub fn make_unbounded_channel<D: Send + 'static>() -> (Sender<D>, Receiver<D>) {
        std::sync::mpsc::sync_channel(0)
    }

    pub fn make_local_unbounded_channel<D: 'static>() -> (Sender<D>, Receiver<D>) {
        std::sync::mpsc::sync_channel(0)
    }
}
#[cfg(feature = "crossbeam_channel")]
mod channel {
    pub type Sender<D> = crossbeam::channel::Sender<D>;
    pub type Receiver<D> = crossbeam::channel::Receiver<D>;
    pub type TryRecvError = crossbeam::channel::TryRecvError;
    pub type TrySendError<T> = crossbeam::channel::TrySendError<T>;
    pub type SendError<T> = crossbeam::channel::SendError<T>;

    pub fn make_bounded_channel<D: Send + 'static>(n: usize) -> (Sender<D>, Receiver<D>) {
        crossbeam::channel::bounded(n)
    }

    pub fn make_unbounded_channel<D: Send + 'static>() -> (Sender<D>, Receiver<D>) {
        crossbeam::channel::unbounded()
    }

    pub fn make_local_unbounded_channel<D: 'static>() -> (Sender<D>, Receiver<D>) {
        crossbeam::channel::unbounded()
    }
}
pub(crate) use channel::*;

/// A channel sender that wakes the parked event loop automatically:
///
/// * every successful [`WakeSender::send`] / [`WakeSender::try_send`] /
///   [`WakeSender::send_async`] wakes the loop that created this sender, so
///   the polling job observes the message on its next round;
/// * dropping the sender (task end / all senders gone → `Disconnected`)
///   wakes the loop too, so the poller can settle deterministically.
///
/// This is the infrastructure-level guarantee behind the "every `Again`
/// waiting point must have a wake source" contract: producers cannot forget
/// to wake, because the sender does it for them. Obtain one via
/// [`crate::ScopedContext::wake_channel`] (created with the loop's handle)
/// or [`WakeSender::wrap`].
pub struct WakeSender<D> {
    tx: Sender<D>,
    wake: WakeHandle,
}

impl<D> Clone for WakeSender<D> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl<D> WakeSender<D> {
    /// Attach a wake handle to an existing raw sender.
    pub const fn wrap(tx: Sender<D>, wake: WakeHandle) -> Self {
        Self { tx, wake }
    }

    /// The underlying raw sender, for paths that must not wake (e.g. a
    /// POSIX signal handler — use `wake_from_signal` there instead).
    pub const fn raw(&self) -> &Sender<D> {
        &self.tx
    }

    pub fn send(&self, value: D) -> Result<(), SendError<D>> {
        let r = self.tx.send(value);
        if r.is_ok() {
            self.wake.wake();
        }
        r
    }

    pub fn try_send(&self, value: D) -> Result<(), TrySendError<D>> {
        let r = self.tx.try_send(value);
        if r.is_ok() {
            self.wake.wake();
        }
        r
    }
}

impl<D: Send> WakeSender<D> {
    /// Async send for capacity-limited channels: retries the send from a
    /// task, waking the parked event loop both on success and when channel
    /// capacity frees (the consumer runs on the JS thread).
    #[cfg(feature = "async_support")]
    pub fn send_async(&self, value: D) -> WakeSendFut<D> {
        WakeSendFut {
            sender: self.clone(),
            value: Some(value),
        }
    }
}

impl<D> Drop for WakeSender<D> {
    fn drop(&mut self) {
        // Sender gone: a `Disconnected` can only be discovered by the next
        // poll — wake the parked loop so it happens now. Cheap no-op when
        // the loop is running or already gone.
        self.wake.wake();
    }
}

/// Future returned by [`WakeSender::send_async`].
#[cfg(feature = "async_support")]
pub struct WakeSendFut<D> {
    sender: WakeSender<D>,
    value: Option<D>,
}

#[cfg(feature = "async_support")]
impl<D: Send + Unpin> std::future::Future for WakeSendFut<D> {
    type Output = Result<(), D>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.get_mut();
        let value = this.value.take().expect("WakeSendFut polled after Ready");

        match this.sender.try_send(value) {
            // try_send already woke the loop on success.
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(crate::TrySendError::Full(v)) => {
                this.value.replace(v);
                // Capacity frees when the JS consumer receives — nudge the
                // event loop, and re-poll this task right after.
                this.sender.wake.wake();
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            Err(crate::TrySendError::Disconnected(v)) => std::task::Poll::Ready(Err(v)),
        }
    }
}

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

        fn poll(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let value = this.value.take().expect("PollSender polled after Ready");

            match this.sender.try_send(value) {
                Ok(_) => Poll::Ready(Ok(())),
                Err(crate::TrySendError::Full(v)) => {
                    this.value = Some(v);
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
