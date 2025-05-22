use std::ops::DerefMut;

use crate::{
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{CallArgs, JSCFunctionListEntry, JS_Call},
    gc::{JS_MarkFunc, JS_MarkValue},
    mem::js_malloc_typed,
    promise::{js_promise_resolve, PromiseValue},
    runtime::JSRuntime,
    utils::UncheckedInto,
    value::{JSValue, JSValueConst, JSValueRc, JS_FreeValue, JS_FreeValueRT, JS_ToBool},
    JS_CFUNC_MAGIC_DEF, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
};

use super::{js_create_iterator_result, JS_IteratorGetCompleteValue, JS_IteratorNext2};

#[derive(Debug)]
pub struct JSAsyncFromSyncIteratorData {
    pub sync_iter: JSValue,
    pub next_method: JSValue,
}

impl Default for JSAsyncFromSyncIteratorData {
    fn default() -> Self {
        Self {
            sync_iter: JSValue::UNDEFINED,
            next_method: JSValue::UNDEFINED,
        }
    }
}

pub const GEN_MAGIC_NEXT: i32 = 0;
pub const GEN_MAGIC_RETURN: i32 = 1;
pub const GEN_MAGIC_THROW: i32 = 2;

impl JSContext {
    /// Create an AsyncIterator from a SyncIterator.
    ///  `sync_iter` is not consumed.
    pub fn create_async_from_sync_iterator(&mut self, sync_iter: JSValue) -> JSResult {
        let next_method = sync_iter
            .get_property(self, JS_ATOM_next)
            .check_exception()?
            .into_rc(self.rt());

        let async_iter = self
            .create_object_with_classid(JS_CLASS_ASYNC_FROM_SYNC_ITERATOR)?
            .into_rc(self.rt());

        if let Some(p) = unsafe { js_malloc_typed::<JSAsyncFromSyncIteratorData>(self, 1, false) } {
            let mut s = unsafe { Box::from_raw(p.as_ptr()) };
            s.sync_iter = sync_iter.dup();
            s.next_method = next_method.into();

            unsafe {
                async_iter
                    .object_unchecked()
                    .set_opaque(Box::into_raw(s).cast());
            }

            Ok(async_iter.into())
        } else {
            Err(-1)
        }
    }
}

#[deprecated(note = "use JSContext::create_async_from_sync_iterator() instead")]
pub unsafe fn JS_CreateAsyncFromSyncIterator(
    ctx: *mut JSContext,
    sync_iter: JSValueConst,
) -> JSValue {
    (*ctx)
        .create_async_from_sync_iterator(sync_iter)
        .unwrap_or(JSValue::EXCEPTION)
}

pub fn js_async_from_sync_iterator_unwrap(ctx: &mut JSContext, args: CallArgs) -> JSResult {
    unsafe {
        let done = JS_ToBool(ctx, args.data_raw(0))?;
        js_create_iterator_result(ctx, args.get_raw(0).dup(), done).check_exception()
    }
}

pub unsafe fn js_async_from_sync_iterator_unwrap_func_create(
    ctx: &mut JSContext,
    done: bool,
) -> JSValue {
    ctx.create_native_callable_object(
        js_async_from_sync_iterator_unwrap,
        1,
        0,
        &[if done { JSValue::TRUE } else { JSValue::FALSE }],
    )
    .unwrap_or(JSValue::EXCEPTION)
}

pub unsafe fn js_async_from_sync_iterator_next(
    ctx: *mut JSContext,
    this_val: JSValueConst,
    argc: i32,
    argv: *mut JSValueConst,
    magic: i32,
) -> JSValue {
    let done_resolve =
        |rr: [JSValueRc; 2], mut err: JSValue, is_reject: usize, promise: JSValueRc| -> JSValue {
            let res2 = JS_Call(
                ctx.as_mut().unwrap(),
                *rr[is_reject],
                JSValue::UNDEFINED,
                1,
                &mut err,
            );
            JS_FreeValue(ctx, err);
            JS_FreeValue(ctx, res2);
            return promise.into();
        };

    let (promise, resolving_funcs) = if let Ok((pr, rr)) = (*ctx).create_promise(JSValue::UNDEFINED)
    {
        (
            pr.as_value().into_rc((*ctx).rt()),
            [rr[0].into_rc((*ctx).rt()), rr[1].into_rc((*ctx).rt())],
        )
    } else {
        return JSValue::EXCEPTION;
    };

    if let Some(p) = this_val
        .object_unchecked()
        .opaque::<JSAsyncFromSyncIteratorData>(JS_CLASS_ASYNC_FROM_SYNC_ITERATOR)
        .map(|mut x| x.as_mut())
    {
        let method: JSValueRc;

        if magic == GEN_MAGIC_NEXT {
            method = p.next_method.dup().into_rc((*ctx).rt());
        } else {
            method = p
                .sync_iter
                .get_property(
                    ctx.as_mut().unwrap(),
                    if magic == GEN_MAGIC_RETURN {
                        JS_ATOM_return
                    } else {
                        JS_ATOM_throw
                    },
                )
                .into_rc((*ctx).rt());

            if method.is_exception() {
                return done_resolve(resolving_funcs, (*ctx).take_exception(), 1, promise);
            } else if method.is_undefined_or_null() {
                let (err, is_reject) = if magic == GEN_MAGIC_RETURN {
                    (
                        js_create_iterator_result(
                            ctx.as_mut().unwrap(),
                            (*argv.offset(0)).dup(),
                            true,
                        ),
                        0,
                    )
                } else {
                    ((*argv.offset(0)).dup(), 1)
                };
                return done_resolve(resolving_funcs, err, is_reject, promise);
            }
        }

        let mut done = 0;
        let mut value = JS_IteratorNext2(
            ctx,
            (*p).sync_iter,
            *method,
            if argc >= 1 { 1 } else { 0 },
            argv,
            &mut done,
        )
        .into_rc((*ctx).rt());

        if value.is_exception() {
            return done_resolve(resolving_funcs, (*ctx).take_exception(), 1, promise);
        }

        if done == 2 {
            let obj = value;
            let mut _done = false;
            value = JS_IteratorGetCompleteValue(ctx, *obj, &mut _done).into_rc((*ctx).rt());
            if value.is_exception() {
                let err = (*ctx).take_exception();
                return done_resolve(resolving_funcs, err, 1, promise);
            }
            done = _done as i32;
        }

        if value.is_exception() {
            return done_resolve(resolving_funcs, (*ctx).take_exception(), 1, promise);
        }

        let value_wrapper_promise: JSValueRc = {
            let a = [*value];
            let cc = CallArgs::new(ctx, (*ctx).promise_ctor, &a, 0, None);
            if let Ok(r) = js_promise_resolve(ctx.as_mut().unwrap(), cc) {
                r.into_rc((*ctx).rt())
            } else {
                return done_resolve(resolving_funcs, (*ctx).take_exception(), 1, promise);
            }
        };

        let resolve_reject: [JSValueRc; 2] = [
            {
                let f = js_async_from_sync_iterator_unwrap_func_create(
                    ctx.as_mut().unwrap(),
                    done != 0,
                )
                .into_rc((*ctx).rt());
                if f.is_exception() {
                    return JSValue::EXCEPTION;
                }
                f
            },
            JSValueRc::undefined((*ctx).rt()),
        ];

        let res = UncheckedInto::<PromiseValue>::unchecked_into(*value_wrapper_promise).reaction(
            ctx.as_mut().unwrap(),
            &[*resolve_reject[0], *resolve_reject[1]],
            &[*resolving_funcs[0], *resolving_funcs[1]],
        );
        if res.is_err() {
            return JSValue::EXCEPTION;
        }

        promise.into()
    } else {
        (*ctx).JS_ThrowTypeError(c"not an Async-from-Sync Iterator");
        let err = (*ctx).take_exception();
        return done_resolve(resolving_funcs, err, 1, promise);
    }
}

pub unsafe fn js_async_from_sync_iterator_finalizer(rt: *mut JSRuntime, val: JSValue) {
    if let Some(mut p) = val
        .object_unchecked()
        .opaque::<JSAsyncFromSyncIteratorData>(JS_CLASS_ASYNC_FROM_SYNC_ITERATOR)
    {
        JS_FreeValueRT(rt, p.as_mut().sync_iter);
        JS_FreeValueRT(rt, p.as_mut().next_method);

        drop(Box::from_raw(p.as_ptr())); // Box<JSAsyncFromSyncIteratorData>
    }
}

pub unsafe fn js_async_from_sync_iterator_mark(
    rt: *mut JSRuntime,
    val: JSValueConst,
    mark_func: JS_MarkFunc,
) {
    if let Some(p) = val
        .object_unchecked()
        .opaque::<JSAsyncFromSyncIteratorData>(JS_CLASS_ASYNC_FROM_SYNC_ITERATOR)
    {
        JS_MarkValue(rt, p.as_ref().sync_iter, mark_func);
        JS_MarkValue(rt, p.as_ref().next_method, mark_func);
    }
}

pub const js_async_from_sync_iterator_proto_funcs: [JSCFunctionListEntry; 3] = [
    JS_CFUNC_MAGIC_DEF!(
        c"next",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_NEXT as _
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"return",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_RETURN as _
    ),
    JS_CFUNC_MAGIC_DEF!(
        c"throw",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_THROW as _
    ),
];
