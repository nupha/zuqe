// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::{
    cell::RefCell,
    collections::HashMap,
    future::Future,
    pin::Pin,
    rc::Rc,
    task::{Context as TaskContext, Poll, Waker},
};

use crate::{
    JobState,
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, Callback, NativeFunction},
    value::JSValue,
};

use super::{JobGcTrace2, Promise};

thread_local! {
    static PROMISE_FUTURE_REGISTRY: RefCell<PromiseFutureRegistry> =
        RefCell::new(PromiseFutureRegistry::new());
}

struct PromiseFutureState {
    result: Option<Result<JSValue, JSValue>>,
    waker: Option<Waker>,
}

struct PromiseFutureRegistry {
    next_id: i32,
    entries: HashMap<i32, Rc<RefCell<PromiseFutureState>>>,
}

impl PromiseFutureRegistry {
    fn new() -> Self {
        Self {
            next_id: 1,
            entries: HashMap::new(),
        }
    }

    fn insert(&mut self, state: Rc<RefCell<PromiseFutureState>>) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.entries.insert(id, state);
        id
    }

    fn take(&mut self, id: i32) -> Option<Rc<RefCell<PromiseFutureState>>> {
        self.entries.remove(&id)
    }
}

pub struct PromiseFuture {
    state: Rc<RefCell<PromiseFutureState>>,
}

impl Future for PromiseFuture {
    type Output = Result<JSValue, JSValue>;

    fn poll(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let mut state = self.state.borrow_mut();

        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            state.waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

fn promise_future_reaction(_: &mut ScopedContext, cc: CallContext) -> JSResult {
    let id = match cc.data(0) {
        JSValue::Int(i) => i,
        _ => return Ok(JSValue::UNDEFINED),
    };

    let is_reject = cc.magic() != 0;
    let arg = cc.arg(0);

    PROMISE_FUTURE_REGISTRY.with(|lut| {
        if let Some(s) = lut.borrow_mut().take(id) {
            let mut state = s.borrow_mut();
            if state.result.is_none() {
                state.result = Some(if is_reject { Err(arg) } else { Ok(arg) });
                if let Some(w) = state.waker.take() {
                    w.wake();
                }
            }
        }
    });

    Ok(JSValue::UNDEFINED)
}

impl Promise {
    pub fn create_from_future<F>(ctx: &mut ScopedContext, fut: F) -> Result<Self, i32>
    where
        F: FnOnce(Box<dyn FnOnce(Result<JSValue, JSValue>) + 'static>),
    {
        let (promise, resolving_funcs) = Promise::create(ctx)?;

        let state = Rc::new(RefCell::new(None::<Result<JSValue, JSValue>>));

        let trace = JobGcTrace2 {
            a: resolving_funcs[0],
            b: resolving_funcs[1],
        };

        ctx.queue_job(
            {
                let state = state.clone();
                move |cx| -> Result<JobState, i32> {
                    let mut opt = state.borrow_mut();

                    if let Some(res) = opt.take() {
                        match res {
                            Ok(v) => {
                                resolving_funcs[0].call(
                                    cx,
                                    JSValue::UNDEFINED,
                                    std::slice::from_ref(&v),
                                )?;
                            }
                            Err(e) => {
                                resolving_funcs[1].call(
                                    cx,
                                    JSValue::UNDEFINED,
                                    std::slice::from_ref(&e),
                                )?;
                            }
                        }
                        debug_assert_eq!(
                            cx.rt().exception(),
                            crate::Exception::No,
                            "[promise create_from_future] future result handled but left exception behind"
                        );
                        Ok(JobState::Done)
                    } else {
                        Ok(JobState::Again)
                    }
                }
            },
            trace,
        );

        let setter_state = state.clone();
        fut(Box::new(move |res: Result<JSValue, JSValue>| {
            *setter_state.borrow_mut() = Some(res);
        }));

        Ok(promise)
    }

    pub fn into_future(mut self, ctx: &mut ScopedContext) -> Result<PromiseFuture, i32> {
        let state = Rc::new(RefCell::new(PromiseFutureState {
            result: None,
            waker: None,
        }));

        let id = PROMISE_FUTURE_REGISTRY.with(|lut| lut.borrow_mut().insert(state.clone()));

        let ok_func = {
            let data = Box::new([JSValue::Int(id)]);
            NativeFunction::create(
                ctx,
                Callback::Func(promise_future_reaction),
                Atom::CONST_empty_string,
                1,
                0,
                ctx.function_proto(),
                Some(data),
            )?
        };

        let err_func = {
            let data = Box::new([JSValue::Int(id)]);
            NativeFunction::create(
                ctx,
                Callback::Func(promise_future_reaction),
                Atom::CONST_empty_string,
                1,
                1,
                ctx.function_proto(),
                Some(data),
            )?
        };

        let _ = self.then(ctx, ok_func.into(), err_func.into())?;

        Ok(PromiseFuture { state })
    }
}
