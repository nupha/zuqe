// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{cell::RefCell, ops::DerefMut, rc::Rc, sync::Arc};

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    Receiver, Sender, TryRecvError,
    builtins::promise::{Promise, PromiseResultSlot},
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    make_bounded_channel,
    object::{AsyncIterableIterator, JS_PROP_CONFIGURABLE, ObjectImpl, ObjectPayload},
    value::JSValue,
};

use super::make_iterator_next_result;

/// Message envelope produced by an [`AsyncIteratorFetcher`].
///
/// - `Value(JSValue)` — a value to yield (iteration continues).
/// - `Done`           — end of iteration.
/// - `Wait`           — data not ready
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
    /// - `Ok(IterFetchStatus::Value(v))`    — yield `v`, iteration continues.
    /// - `Ok(IterFetchStatus::Done)`        — iteration ended.
    /// - `Ok(IterFetchStatus::Wait)`        — no data yet, retry next tick.
    /// - `Err(_)`                           — error, reject the promise.
    fn try_fetch(&self, cx: &mut ScopedContext) -> Result<IterFetchStatus, i32>;

    /// Called when `return()` is invoked on the JS side.
    /// Override this to release resources (e.g. drop a receiver).
    fn close(&self) {}

    /// Custom `return(value)` handling. Returning `Some(promise)` replaces the
    /// generic `{value, done: true}` result with that promise's fulfillment or
    /// rejection (e.g. an iterator that must await an async cancel before
    /// settling, or reject when its source already errored). Returning `None`
    /// falls back to `close()` + the default resolved result.
    fn on_return(&self, _ctx: &mut ScopedContext, _value: JSValue) -> Result<Option<Promise>, i32> {
        Ok(None)
    }
}

/// Internal payload stored inside the JS `AsyncIterableIterator` object.
pub struct AsyncIterPayload {
    /// The fetcher that produces `IterFetchStatus` values.
    /// Set to `None` once `return()` is called.
    fetcher: Option<Rc<dyn AsyncIteratorFetcher>>,

    /// Whether the iteration has completed.
    completed: bool,
}

impl std::fmt::Debug for AsyncIterPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AsyncIterPayload")
            .field("completed", &self.completed)
            .finish()
    }
}

impl GcTrace for AsyncIterPayload {
    fn trace(&self, _tr: &mut GcTraceCtx) {}
}

impl AsyncIterPayload {
    #[inline(always)]
    const fn completed(&self) -> bool {
        self.completed
    }

    const fn set_completed(&mut self) {
        self.completed = true;
    }
}

/// Channel-backed fetcher
struct ChannelFetcher<D> {
    rx: RefCell<Option<Receiver<D>>>,

    /// convert returns:
    /// - `Ok(Some(v))`    — yield `v`, iteration continues.
    /// - `Ok(None)`       — iteration ended.
    /// - `Err(_)`         — error, reject the promise.
    convert: Box<dyn Fn(&mut ScopedContext, D) -> Result<Option<JSValue>, i32> + Send>,
}

impl<D: 'static> AsyncIteratorFetcher for ChannelFetcher<D> {
    fn try_fetch(&self, cx: &mut ScopedContext) -> Result<IterFetchStatus, i32> {
        let mut guard = self.rx.borrow_mut();

        if let Some(rx) = guard.as_ref() {
            match rx.try_recv() {
                Ok(data) => match (self.convert)(cx, data) {
                    Ok(Some(v)) => Ok(IterFetchStatus::Value(v)),
                    Ok(None) => {
                        guard.take();
                        Ok(IterFetchStatus::Done)
                    }
                    Err(e) => {
                        guard.take();
                        Err(e)
                    }
                },
                Err(TryRecvError::Empty) => Ok(IterFetchStatus::Wait),
                Err(TryRecvError::Disconnected) => {
                    guard.take();
                    Ok(IterFetchStatus::Done)
                }
            }
        } else {
            Ok(IterFetchStatus::Done)
        }
    }

    fn close(&self) {
        self.rx.borrow_mut().take();
    }
}

/// Slot-backed fetcher that polls a [`PromiseResultSlot`].
///
/// The fetcher holds the strong `Arc` for its whole lifetime, so a value
/// committed right before the committers drop their references stays
/// reachable; "committers are gone" is detected by `strong_count` reaching 1
/// (only the fetcher holds a strong reference). The pending `next()` promise
/// is then rejected deterministically instead of polling forever and hanging
/// the event loop.
struct SlotFetcher<D: Send + 'static> {
    data: Arc<crate::builtins::promise::SlotInner<D>>,
    /// convert D to JSValue, running in js thread. None to stop
    convert: Box<dyn Fn(&mut ScopedContext, D) -> Result<Option<JSValue>, i32>>,
}

impl<D: Send + 'static> AsyncIteratorFetcher for SlotFetcher<D> {
    fn try_fetch(&self, cx: &mut ScopedContext) -> Result<IterFetchStatus, i32> {
        let Some(data) = self.data.value.lock().unwrap().take() else {
            if Arc::strong_count(&self.data) == 1 {
                debug_assert!(false, "[promise result slot] dropped without commit");
                return Err(cx.throw_internal_error("promise result slot dropped without commit"));
            }
            return Ok(IterFetchStatus::Wait);
        };
        match (self.convert)(cx, data) {
            Ok(Some(v)) => Ok(IterFetchStatus::Value(v)),
            Ok(None) => Ok(IterFetchStatus::Done),
            Err(e) => Err(e),
        }
    }
}

/// An `AsyncIterableIterator` object that wraps a data fetcher.
///
/// It implements the async iterator protocol (`next()` → `Promise<{value, done}>`),
/// suitable for use with `for await...of` loops in JavaScript.
///
/// # Example (Rust side)
///
/// ```ignore
/// let (iter, tx) = AsyncIterableIterator::create(ctx, |_, n: i32| {
///     Ok(Some(JSValue::from(n)))
/// }, None)?;
///
/// // With on_return callback:
/// let (iter, tx) = AsyncIterableIterator::create(
///     ctx,
///     |_, n: i32| Ok(Some(JSValue::from(n))),
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
impl AsyncIterableIterator {
    pub(crate) fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
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
        let data = Box::new(AsyncIterPayload {
            fetcher: Some(Rc::new(fetcher)),
            completed: false,
        });

        let obj = ctx
            .create_instance_with_payload(ObjectPayload::ASYNC_ITERABLE_ITERATOR(data))
            .map_err(|_| -1)?;

        Ok(Self(obj.0))
    }

    /// Convert data D to JSValue closure returns:
    /// - `Ok(Some(v))`    — yield `v`, iteration continues.
    /// - `Ok(None)`       — iteration ended.
    /// - `Err(_)`         — error, reject the promise.
    pub fn create_with_receiver<D: Send + 'static>(
        ctx: &mut ScopedContext,
        rx: Receiver<D>,
        convert: impl Fn(&mut ScopedContext, D) -> Result<Option<JSValue>, i32> + Send + 'static,
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
        convert: impl Fn(&mut ScopedContext, D) -> Result<Option<JSValue>, i32> + Send + 'static,
    ) -> Result<(Self, Sender<D>), i32> {
        let (tx, rx) = make_bounded_channel::<D>(1);
        let iter = Self::create_with_receiver(ctx, rx, convert)?;
        Ok((iter, tx))
    }

    /// Create an `AsyncIterableIterator` driven by a [`PromiseResultSlot`].
    ///
    /// Returns the iterator. The slot can be used, typically from another thread,
    /// to submit values of type `D` via [`PromiseResultSlot::commit`].
    ///
    /// The `done_value` is a special sentinel: when the slot receives this value,
    /// the iterator yields `{value: undefined, done: true}` and stops.
    ///
    /// The `convert` closure transforms each received `D` into an `Option<JSValue>`:
    /// - `Ok(Some(v))` — yield `v`, iteration continues.
    /// - `Ok(None)`    — iteration ended.
    /// - `Err(_)`      — error, reject the promise.
    ///
    /// Note: the `convert` closure is running in js thread
    pub fn create_with_result_slot<D: Send + 'static>(
        ctx: &mut ScopedContext,
        data_slot: PromiseResultSlot<D>,
        convert: impl Fn(&mut ScopedContext, D) -> Result<Option<JSValue>, i32> + 'static,
    ) -> Result<Self, i32> {
        // Commits from any slot clone (typically on IO threads) wake the
        // parked event loop so the fetcher polls promptly.
        data_slot.attach_wake(&ctx.rt_mut().park);
        Self::create(
            ctx,
            SlotFetcher {
                data: data_slot.arc(),
                convert: Box::new(convert),
            },
        )
    }

    #[inline(always)]
    pub fn payload(&self) -> &AsyncIterPayload {
        #[cfg(debug_assertions)]
        unsafe {
            self.0.as_ref().payload().async_iterator().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.0
                .as_ref()
                .payload()
                .async_iterator()
                .unwrap_unchecked()
        }
    }

    #[inline(always)]
    pub fn payload_mut(&mut self) -> &mut AsyncIterPayload {
        #[cfg(debug_assertions)]
        {
            self.deref_mut().payload_mut().async_iterator_mut().unwrap()
        }
        #[cfg(not(debug_assertions))]
        unsafe {
            self.deref_mut()
                .payload_mut()
                .async_iterator_mut()
                .unwrap_unchecked()
        }
    }

    #[inline(always)]
    pub const fn gc_ref(&self) -> GcRef<ObjectImpl> {
        self.0
    }
}

/// `AsyncIterableIterator.prototype.next(value)` — returns a `Promise<{value, done}>`.
///
/// The fetch runs synchronously first, so a source that produces data on demand
/// (e.g. a pull-based stream) settles `next()` immediately — a subsequent
/// `return()` cannot preempt the already-issued read. Only when the fetch is not
/// ready does a polling job retry `try_fetch` on every engine tick.
fn js_async_iterator_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();

    let Some(o) = this.as_object() else {
        return Err(ctx.throw_type_error("not an AsyncIterableIterator"));
    };
    let Some(ait) = o.payload().async_iterator() else {
        return Err(ctx.throw_type_error("not an AsyncIterableIterator"));
    };

    // A completed iterator (return() called, or a final error) yields
    // `{done: true}` forever; next() still returns a promise. Resolve it via a
    // polling job (not a pre-fulfilled promise) so an in-flight return() whose
    // cancel is still settling resolves before this next() — matching the
    // spec's "next() resolves after return() resolves" ordering.
    if ait.completed() {
        let promise = Promise::create_with_job(ctx, move |cx| {
            let result = make_iterator_next_result(cx, JSValue::UNDEFINED, true)?;
            Ok(Some(result))
        })?;
        return Ok(promise.into());
    }

    let Some(fetcher) = ait.fetcher.clone() else {
        let promise = Promise::create_with_job(ctx, move |cx| {
            let result = make_iterator_next_result(cx, JSValue::UNDEFINED, true)?;
            Ok(Some(result))
        })?;
        return Ok(promise.into());
    };

    match fetcher.try_fetch(ctx) {
        Ok(IterFetchStatus::Value(value)) => {
            let result = make_iterator_next_result(ctx, value, false)?;
            Promise::from_resolve_value(ctx, result).map(JSValue::from)
        }
        Ok(IterFetchStatus::Done) => {
            let result = make_iterator_next_result(ctx, JSValue::UNDEFINED, true)?;
            Promise::from_resolve_value(ctx, result).map(JSValue::from)
        }
        Ok(IterFetchStatus::Wait) => {
            let promise = Promise::create_with_job(ctx, move |cx| {
                fetcher.try_fetch(cx).and_then(|s| {
                    let ret = match s {
                        IterFetchStatus::Value(value) => {
                            make_iterator_next_result(cx, value, false).map(Some)
                        }
                        IterFetchStatus::Done => {
                            make_iterator_next_result(cx, JSValue::UNDEFINED, true).map(Some)
                        }
                        IterFetchStatus::Wait => Ok(None),
                    };
                    debug_assert_eq!(
                        cx.rt().exception(),
                        crate::Exception::No,
                        "[async_iter next] try_fetch succeeded but left exception behind"
                    );
                    ret
                })
            })?;

            Ok(promise.into())
        }
        Err(_) => {
            // try_fetch threw (e.g. the stream errored) — reject next() with it.
            let e = ctx.take_exception().to_value(ctx);
            Promise::from_reject_value(ctx, e).map(JSValue::from)
        }
    }
}

/// `AsyncIterableIterator.prototype.return(value)` — ends the iteration immediately.
///
/// Invokes the `on_return` callback (if set) and notifies the fetcher to
/// release its resources.
fn js_async_iterator_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let value = cc.arg(0);

    if let Some(mut obj) = this.as_object()
        && let Some(ait) = obj.payload_mut().async_iterator_mut()
    {
        ait.set_completed();

        if let Some(fetcher) = ait.fetcher.take() {
            // A fetcher that supplies its own return semantics (e.g. awaiting
            // an async cancel, or rejecting when the source errored) takes
            // over the return() result; otherwise fall through to the
            // default `{value, done: true}`.
            if let Some(promise) = fetcher.on_return(ctx, value)? {
                return Ok(promise.into());
            }

            fetcher.close();
        }
    }

    let result = make_iterator_next_result(ctx, value, true)?;
    Promise::from_resolve_value(ctx, result).map(JSValue::from)
}

const js_async_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC_ENUM("next", 0, js_async_iterator_next, 0),
    FieldDef::FUNC_ENUM("return", 1, js_async_iterator_return, 0),
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "AsyncIterableIterator",
        JS_PROP_CONFIGURABLE,
    ),
];
