// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use gc_lite::{GcRef, GcTrace, GcTraceCtx};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel},
};

use crate::{
    JS_CLASS_ASYNC_ITERATOR,
    builtins::promise::Promise,
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::{JS_PROP_CONFIGURABLE, Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

use super::make_iterator_next_result;

// ---------------------------------------------------------------------------
// Payload
// ---------------------------------------------------------------------------

/// Message envelope produced by an [`AsyncIteratorFetcher`].
///
/// - `Value(JSValue)` — a value to yield (iteration continues).
/// - `Done`           — end of iteration.
/// - `Error`          — an error occurred (reject the promise).
#[derive(Debug)]
pub enum IterFetchStatus {
    Value(JSValue),
    Wait,
    Done,
}

/// Trait for fetching data from an arbitrary source into [`IterFetchStatus`].
///
/// Implementors decide how to produce values (e.g. from a channel, file,
/// or any other data source). The [`try_fetch`](Self::try_fetch) method is
/// called on every engine tick by the polling Promise job created by `next()`.
pub trait AsyncIteratorFetcher {
    /// Try to fetch the next value.
    ///
    /// Returns:
    /// - `Ok(IterFetchStatus::Value(v))`.   — yield `v`, iteration continues.
    /// - `Ok(IterFetchStatus::Done)`        — iteration ended.
    /// - `Ok(IterFetchStatus::Wait)`        — no data yet, retry next tick.
    /// - `Err(_)`                           — error, reject the promise.
    fn try_fetch(&self, cx: &mut ScopedContext) -> Result<IterFetchStatus, i32>;

    /// Called when `return()` is invoked on the JS side.
    /// Override this to release resources (e.g. drop a receiver).
    fn close(&self) {}
}

/// Internal payload stored inside the JS `AsyncIterator` object.
pub struct AsyncIteratorPayload {
    /// The fetcher that produces `IterFetchStatus` values.
    /// Set to `None` once `return()` is called.
    fetcher: Option<Rc<dyn AsyncIteratorFetcher>>,

    /// Whether the iteration has completed.
    completed: bool,
}

impl std::fmt::Debug for AsyncIteratorPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncIteratorPayload")
            .field("completed", &self.completed)
            .finish()
    }
}

impl GcTrace for AsyncIteratorPayload {
    fn trace(&self, _tr: &mut GcTraceCtx) {
        // The fetcher is not GC-managed. JSValues are only surfaced
        // after try_fetch() succeeds.
    }
}

impl AsyncIteratorPayload {
    #[inline(always)]
    const fn completed(&self) -> bool {
        self.completed
    }

    const fn set_completed(&mut self) {
        self.completed = true;
    }
}

// ---------------------------------------------------------------------------
// Default fetcher: channel-backed
// ---------------------------------------------------------------------------

struct ChannelFetcher<D> {
    rx: RefCell<Option<Receiver<D>>>,
    convert: Box<dyn Fn(&mut ScopedContext, D) -> Result<IterFetchStatus, i32> + Send>,
}

impl<D: Send + 'static> AsyncIteratorFetcher for ChannelFetcher<D> {
    fn try_fetch(&self, cx: &mut ScopedContext) -> Result<IterFetchStatus, i32> {
        let mut guard = self.rx.borrow_mut();
        let Some(rx) = guard.as_ref() else {
            return Ok(IterFetchStatus::Done);
        };

        match rx.try_recv() {
            Ok(data) => {
                let r = (self.convert)(cx, data);
                if matches!(r, Ok(IterFetchStatus::Done) | Err(_)) {
                    guard.take();
                }
                r
            }
            Err(TryRecvError::Empty) => Ok(IterFetchStatus::Wait),
            Err(TryRecvError::Disconnected) => {
                guard.take();
                Ok(IterFetchStatus::Done)
            }
        }
    }

    fn close(&self) {
        self.rx.borrow_mut().take();
    }
}

// ---------------------------------------------------------------------------
// AsyncIterator — typed wrapper (like struct Promise)
// ---------------------------------------------------------------------------

/// An `AsyncIterator` object that wraps a data fetcher.
///
/// It implements the async iterator protocol (`next()` → `Promise<{value, done}>`),
/// suitable for use with `for await...of` loops in JavaScript.
///
/// # Example (Rust side)
///
/// ```ignore
/// let (iter, tx) = AsyncIterator::create(ctx, |_, n: i32| {
///     Ok(Some(JSValue::Int(n)))
/// }, None)?;
///
/// // With on_return callback:
/// let (iter, tx) = AsyncIterator::create(
///     ctx,
///     |_, n: i32| Ok(Some(JSValue::Int(n))),
///     Some(|| { println!("iterator closed"); }),
/// )?;
///
/// // In another thread or later:
/// tx.send(42).unwrap();   // yields {value: 42, done: false}
/// tx.send(0).unwrap();    // yields {value: 0, done: false}
/// drop(tx);               // channel closed → {value: undefined, done: true}
/// ```
///
/// # JS side
///
/// ```js
/// for await (const value of asyncIterator) {
///     console.log(value);
/// }
/// ```
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct AsyncIterator(GcRef<ObjectImpl>);

object_cast!(AsyncIterator, JS_CLASS_ASYNC_ITERATOR);

impl AsyncIterator {
    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        let aiter_proto = ctx.async_iterator_proto()?;
        let proto = ctx.create_base_object_with_proto(Some(aiter_proto))?;
        ctx.init_class_proto_internal(class_id, proto, js_async_iterator_proto_funcs)?;
        Ok(())
    }

    pub fn create(
        ctx: &mut ScopedContext,
        fetcher: impl AsyncIteratorFetcher + 'static,
    ) -> Result<Self, i32> {
        let data = Box::new(AsyncIteratorPayload {
            fetcher: Some(Rc::new(fetcher)),
            completed: false,
        });

        let obj = ctx
            .create_instance_with_payload(ObjectPayload::ASYNC_ITERATOR(data))
            .map_err(|_| -1)?;

        Ok(Self(obj.0))
    }

    pub fn create_with_receiver<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: Receiver<D>,
        convert: impl Fn(&mut ScopedContext, D) -> Result<IterFetchStatus, i32> + Send + 'static,
    ) -> Result<Self, i32> {
        Self::create(
            ctx,
            ChannelFetcher {
                rx: RefCell::new(Some(rx)),
                convert: Box::new(convert),
            },
        )
    }

    pub fn create_with_channel<D: Send + 'static>(
        ctx: &mut ScopedContext,
        convert: impl Fn(&mut ScopedContext, D) -> Result<IterFetchStatus, i32> + Send + 'static,
    ) -> Result<(Self, SyncSender<D>), i32> {
        let (tx, rx) = sync_channel::<D>(1);
        let iter = Self::create_with_receiver(ctx, rx, convert)?;
        Ok((iter, tx))
    }

    #[inline(always)]
    pub fn payload(&self) -> &AsyncIteratorPayload {
        #[cfg(debug_assertions)]
        {
            self.0.payload().async_iterator().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0.payload().async_iterator().unwrap_unchecked()
        }
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut AsyncIteratorPayload {
        #[cfg(debug_assertions)]
        {
            self.0.payload_mut().async_iterator_mut().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0.payload_mut().async_iterator_mut().unwrap_unchecked()
        }
    }

    #[inline(always)]
    pub const fn gc_ref(&self) -> GcRef<ObjectImpl> {
        self.0
    }
}

/// `AsyncIterator.prototype.next(value)` — returns a `Promise<{value, done}>`.
///
/// Each call creates a new Promise backed by a polling job that calls
/// `AsyncIteratorFetcher::try_fetch` on every engine tick.
fn js_async_iterator_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();

    let Some(ait) = this.as_object().and_then(|o| o.payload().async_iterator()) else {
        ctx.throw_type_error("not an AsyncIterator");
        return Err(-1);
    };

    if ait.completed() {
        make_iterator_next_result(ctx, JSValue::UNDEFINED, true)
    } else if let Some(fetcher) = ait.fetcher.clone() {
        let promise = Promise::create_with_job(ctx, move |cx| match fetcher.try_fetch(cx) {
            Ok(IterFetchStatus::Value(value)) => {
                make_iterator_next_result(cx, value, false).map(Some)
            }
            Ok(IterFetchStatus::Done) => {
                make_iterator_next_result(cx, JSValue::UNDEFINED, true).map(Some)
            }
            Ok(IterFetchStatus::Wait) => Ok(None),
            Err(e) => {
                cx.throw_internal_error("fail to fetch next value");
                Err(-1)
            }
        })?;
        Ok(promise.into())
    } else {
        make_iterator_next_result(ctx, JSValue::UNDEFINED, true)
    }
}

/// `AsyncIterator.prototype.return(value)` — ends the iteration immediately.
///
/// Invokes the `on_return` callback (if set) and notifies the fetcher to
/// release its resources.
fn js_async_iterator_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();

    if let Some(ait) = this
        .as_object_mut()
        .and_then(|obj| obj.payload_mut().async_iterator_mut())
    {
        ait.set_completed();

        if let Some(fetcher) = ait.fetcher.take() {
            fetcher.close();
        }
    }

    let value = cc.arg(0);
    let result = make_iterator_next_result(ctx, value, true)?;
    Ok(result)
}

const js_async_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL("next", 1, js_async_iterator_next, 0),
    FieldDef::CALL("return", 1, js_async_iterator_return, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "AsyncIterator",
        JS_PROP_CONFIGURABLE,
    ),
];
