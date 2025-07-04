use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::consts::*,
    context::JSContext,
    error::JSResult,
    function::{CallContext, JS_Call, NativeFieldDef},
    object::JSObjectIsa,
    promise::{PromiseValue, js_promise_resolve},
    runtime::JSRuntime,
    utils::UncheckedInto,
    value::{JSValue, JSValueConst},
};

use super::{JS_IteratorGetCompleteValue, JS_IteratorNext2, js_create_iterator_result};

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
    pub fn create_async_from_sync_iterator(&self, sync_iter: JSValue) -> JSResult {
        let next_method = sync_iter
            .get_property(self, JS_ATOM_next)
            .check_exception()?;

        let s = JSAsyncFromSyncIteratorData {
            sync_iter,
            next_method,
        };

        self.create_instance(JSObjectIsa::ASYNC_FROM_SYNC_ITERATOR(s))
            .map_err(|_| -1)
    }
}

#[deprecated(note = "use JSContext::create_async_from_sync_iterator() instead")]
pub unsafe fn JS_CreateAsyncFromSyncIterator(ctx: &JSContext, sync_iter: JSValueConst) -> JSValue {
    ctx.create_async_from_sync_iterator(sync_iter)
        .unwrap_or(JSValue::EXCEPTION)
}

pub fn js_async_from_sync_iterator_unwrap(ctx: &JSContext, args: CallContext) -> JSResult {
    let done = args.data_raw(0).coerce_to_bool(ctx)?;
    js_create_iterator_result(ctx, args.arg(0).clone(), done)
}

pub unsafe fn js_async_from_sync_iterator_unwrap_func_create(
    ctx: &JSContext,
    done: bool,
) -> JSValue {
    ctx.create_native_callable_object(
        js_async_from_sync_iterator_unwrap,
        1,
        0,
        &[JSValue::Bool(done)],
    )
    .unwrap_or(JSValue::EXCEPTION)
}

pub fn js_async_from_sync_iterator_next(ctx: &JSContext, cc: CallContext) -> JSResult {
    let done_resolve =
        |rr: [JSValue; 2], mut err: JSValue, is_reject: usize, promise: JSValue| -> JSValue {
            let res2 = JS_Call(ctx, &rr[is_reject], &JSValue::UNDEFINED, &[err]);
            return promise;
        };

    let (promise, resolving_funcs) = ctx.create_promise(&JSValue::UNDEFINED)?;
    let this_val = cc.this();

    let p = unsafe { this_val.object_unchecked() };
    if let JSObjectIsa::ASYNC_FROM_SYNC_ITERATOR(p) = p.0.isa.borrow().deref() {
        let method = if cc.magic() == GEN_MAGIC_NEXT {
            p.next_method.clone()
        } else {
            let fval = p.sync_iter.get_property(
                ctx,
                if cc.magic() == GEN_MAGIC_RETURN {
                    JS_ATOM_return
                } else {
                    JS_ATOM_throw
                },
            );

            if fval.is_exception() {
                return Ok(done_resolve(
                    resolving_funcs,
                    ctx.take_exception(),
                    1,
                    promise.into(),
                ));
            }

            if fval.is_undefined_or_null() {
                let (err, is_reject) = if cc.magic() == GEN_MAGIC_RETURN {
                    (js_create_iterator_result(ctx, cc.arg(0).clone(), true)?, 0)
                } else {
                    (cc.arg(0).clone(), 1)
                };
                return Ok(done_resolve(
                    resolving_funcs,
                    err,
                    is_reject,
                    promise.into(),
                ));
            }

            fval
        };

        let mut done = 0;
        let mut value = unsafe {
            JS_IteratorNext2(
                ctx,
                &p.sync_iter,
                &method,
                if cc.length() == 0 {
                    &[]
                } else {
                    &cc.arguments()[0..1]
                },
                &mut done,
            )
        };

        if value.is_exception() {
            return Ok(done_resolve(
                resolving_funcs,
                ctx.take_exception(),
                1,
                promise.into(),
            ));
        }

        if done == 2 {
            let obj = value;
            let mut _done = false;
            value = unsafe { JS_IteratorGetCompleteValue(ctx, &obj, &mut _done) };
            if value.is_exception() {
                let err = ctx.take_exception();
                return Ok(done_resolve(resolving_funcs, err, 1, promise.into()));
            }
            done = _done as i32;
        }

        if value.is_exception() {
            return Ok(done_resolve(
                resolving_funcs,
                ctx.take_exception(),
                1,
                promise.into(),
            ));
        }

        let value_wrapper_promise: JSValue = {
            let cc = CallContext::new(
                ctx,
                ctx.0.borrow().promise_ctor.clone(),
                std::slice::from_ref(&value),
                0,
                None,
            );
            if let Ok(r) = js_promise_resolve(ctx, cc) {
                r
            } else {
                return Ok(done_resolve(
                    resolving_funcs,
                    ctx.take_exception(),
                    1,
                    promise.into(),
                ));
            }
        };

        let resolve_reject: [JSValue; 2] = [
            unsafe {
                js_async_from_sync_iterator_unwrap_func_create(ctx, done != 0).check_exception()?
            },
            JSValue::UNDEFINED,
        ];

        unsafe {
            UncheckedInto::<PromiseValue>::unchecked_into(value_wrapper_promise).reaction(
                ctx,
                &resolve_reject,
                &resolving_funcs,
            )?
        };

        Ok(promise.into())
    } else {
        ctx.JS_ThrowTypeError("not an Async-from-Sync Iterator");
        let err = ctx.take_exception();
        Ok(done_resolve(resolving_funcs, err, 1, promise.into()))
    }
}

pub fn js_async_from_sync_iterator_finalizer(rt: &mut JSRuntime, isa: &JSObjectIsa, id: usize) {
    // if let JSObjectIsa::ASYNC_FROM_SYNC_ITERATOR(p) = val.object_unchecked().isa() {
    //     drop(Box::from_raw(p));
    // }
}

pub fn js_async_from_sync_iterator_mark(val: JSValue) {
    let p = unsafe { val.object_unchecked() };
    if let JSObjectIsa::ASYNC_FROM_SYNC_ITERATOR(it) = p.0.isa.borrow().deref() {
        it.sync_iter.gc_mark();
        it.next_method.gc_mark();
    }
}

pub const js_async_from_sync_iterator_proto_funcs: [NativeFieldDef; 3] = [
    JS_CFUNC_NATIVE_DEF!(
        "next",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_NEXT as _
    ),
    JS_CFUNC_NATIVE_DEF!(
        "return",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_RETURN as _
    ),
    JS_CFUNC_NATIVE_DEF!(
        "throw",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_THROW as _
    ),
];
