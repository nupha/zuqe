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
    atom::Atom,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, Callback, NativeFunction},
    value::JSValue,
};

use super::{JobGcTrace2, JobState, Promise};

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
    id: i32,
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

        Ok(PromiseFuture { id, state })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{builtins::promise::PromiseState, runtime::Runtime};
    use std::{
        pin::Pin,
        task::{Context as TaskContext2, Poll as Poll2, RawWaker, RawWakerVTable, Waker},
    };

    fn with_rt_ctx(f: impl FnOnce(&mut Runtime, &mut ScopedContext)) {
        let mut rt = Runtime::create().unwrap();
        let mut ctx = rt.create_context(|_| Ok(())).unwrap();
        ctx.with_new_scope(|mut scope| {
            f(&mut rt, &mut scope);
        });
    }

    #[test]
    fn create_from_future_resolve() {
        with_rt_ctx(|rt, ctx| {
            let p = Promise::create_from_future(ctx, |notify| {
                notify(Ok(JSValue::Int(123)));
            })
            .unwrap();
            assert_eq!(p.state(), PromiseState::Pending);

            loop {
                match rt.tick_internal().unwrap() {
                    crate::runtime::TickStatus::Continue => {}
                    crate::runtime::TickStatus::Stop(_) => break,
                }
            }

            assert_eq!(p.state(), PromiseState::Fulfilled);
            assert_eq!(p.result(), JSValue::Int(123));
        });
    }

    #[test]
    fn create_from_future_reject() {
        with_rt_ctx(|rt, ctx| {
            let mut p = Promise::create_from_future(ctx, |notify| {
                notify(Err(JSValue::Int(456)));
            })
            .unwrap();
            assert_eq!(p.state(), PromiseState::Pending);

            loop {
                match rt.tick_internal().unwrap() {
                    crate::runtime::TickStatus::Continue => {}
                    crate::runtime::TickStatus::Stop(_) => break,
                }
            }

            assert_eq!(p.state(), PromiseState::Rejected);
            assert_eq!(p.result(), JSValue::Int(456));
        });
    }

    fn noop_raw_waker() -> RawWaker {
        fn clone(_: *const ()) -> RawWaker {
            noop_raw_waker()
        }

        fn wake(_: *const ()) {}

        fn wake_by_ref(_: *const ()) {}

        fn drop(_: *const ()) {}

        RawWaker::new(
            std::ptr::null(),
            &RawWakerVTable::new(clone, wake, wake_by_ref, drop),
        )
    }

    fn noop_waker() -> Waker {
        unsafe { Waker::from_raw(noop_raw_waker()) }
    }

    async fn afunc() -> Result<JSValue, JSValue> {
        Ok(JSValue::Int(789))
    }

    #[test]
    fn create_from_future_async_fn() {
        with_rt_ctx(|rt, ctx| {
            // 这里的执行逻辑相当于一个“手写的 mock async runtime”，直接对 async fn 返回的
            // Future 做一次 poll。真实场景下，如果使用 tokio，调用方更典型的写法是：
            //
            // Promise::create_from_future(ctx, |notify| {
            //     tokio::spawn(async move {
            //         let res = afunc().await;
            //         notify(res);
            //     });
            // })
            //
            // 这样，Future 的调度和唤醒完全交给 tokio 负责，而 Promise 这边只通过 notify
            // 收到最终结果，然后在 tick() 的 job 中完成 resolve/reject。

            let p = Promise::create_from_future(ctx, |notify| {
                let mut fut = Box::pin(afunc());
                let waker = noop_waker();
                let mut cx = TaskContext2::from_waker(&waker);

                match Future::poll(fut.as_mut(), &mut cx) {
                    Poll2::Ready(res) => notify(res),
                    Poll2::Pending => notify(Err(JSValue::Int(-1))),
                }
            })
            .unwrap();
            assert_eq!(p.state(), PromiseState::Pending);

            loop {
                match rt.tick_internal().unwrap() {
                    crate::runtime::TickStatus::Continue => {}
                    crate::runtime::TickStatus::Stop(_) => break,
                }
            }

            assert_eq!(p.state(), PromiseState::Fulfilled);
            assert_eq!(p.result(), JSValue::Int(789));
        });
    }
}
