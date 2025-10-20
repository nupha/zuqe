use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF,
    atom::consts::*,
    callable::NativeFunctionWithData,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    iterator::ObjectIter,
    object::{JSObject, ObjectPayload},
    promise::{Promise, js_promise_resolve},
    runtime::JSRuntime,
    value::{JSValue, JSValueConst},
};

use super::{JS_IteratorGetCompleteValue, js_create_iterator_result};

#[derive(Debug)]
pub struct JSAsyncFromSyncIteratorData {
    pub(crate) obj: JSObject,
    pub(crate) next_method: JSValue,
}

pub const GEN_MAGIC_NEXT: i32 = 0;
pub const GEN_MAGIC_RETURN: i32 = 1;
pub const GEN_MAGIC_THROW: i32 = 2;

#[deprecated(note = "unused")]
pub unsafe fn JS_CreateAsyncFromSyncIterator(ctx: &JSContext, sync_iter: JSValueConst) -> JSValue {
    unreachable!("unused")
}

pub fn js_async_from_sync_iterator_unwrap(ctx: &JSContext, args: CallContext) -> JSResult {
    let done = args.data(0).to_bool()?;
    js_create_iterator_result(ctx, args.arg(0).clone(), done)
}

pub unsafe fn js_async_from_sync_iterator_unwrap_func_create(
    ctx: &JSContext,
    done: bool,
) -> JSValue {
    NativeFunctionWithData::create(
        ctx,
        js_async_from_sync_iterator_unwrap,
        1,
        0,
        [JSValue::Bool(done)].into(),
    )
    .map_or(JSValue::EXCEPTION, |x| x.into())
}

pub fn js_async_from_sync_iterator_next(ctx: &JSContext, cc: CallContext) -> JSResult {
    let done_resolve =
        |rr: [JSValue; 2], err: JSValue, is_reject: usize, promise: Promise| -> JSValue {
            let _ = rr[is_reject].call(ctx, &JSValue::UNDEFINED, &[err]);
            promise.into()
        };

    let (promise, resolving_funcs) = Promise::create(ctx)?;

    let this_val = cc.this();
    let p = unsafe { this_val.as_object_unchecked() };

    if let ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(p) = p.payload() {
        let method = if cc.magic() == GEN_MAGIC_NEXT {
            p.next_method.clone()
        } else {
            match p.obj.get_property(
                ctx,
                if cc.magic() == GEN_MAGIC_RETURN {
                    JS_ATOM_return
                } else {
                    JS_ATOM_throw
                }
                .deref(),
            ) {
                Ok(fval) => {
                    if fval.is_undefined_or_null() {
                        let (err, is_reject) = if cc.magic() == GEN_MAGIC_RETURN {
                            (js_create_iterator_result(ctx, cc.arg(0).clone(), true)?, 0)
                        } else {
                            (cc.arg(0).clone(), 1)
                        };
                        return Ok(done_resolve(resolving_funcs, err, is_reject, promise));
                    }

                    fval
                }
                Err(_) => {
                    return Ok(done_resolve(
                        resolving_funcs,
                        ctx.take_exception(),
                        1,
                        promise,
                    ));
                }
            }
        };

        if let Ok((mut value, state)) = ObjectIter::do_apply_next_method(
            ctx,
            &p.obj,
            &method,
            if cc.length() == 0 {
                &[]
            } else {
                &cc.arguments()[0..1]
            },
        ) {
            let done: bool = match state {
                IteratorState::NotDone => false,
                IteratorState::Done => true,
                IteratorState::NotParsed => {
                    let mut _done = false;
                    value = unsafe { JS_IteratorGetCompleteValue(ctx, &value, &mut _done) };
                    if value.is_exception() {
                        let err = ctx.take_exception();
                        return Ok(done_resolve(resolving_funcs, err, 1, promise));
                    }
                    _done
                }
            };

            let value_wrapper_promise: Promise = {
                let cc = CallContext::new(
                    ctx,
                    ctx.0.borrow().promise_ctor.clone(),
                    std::slice::from_ref(&value),
                    0,
                    None,
                );
                if let Ok(r) = js_promise_resolve(ctx, cc) {
                    r.try_into()?
                } else {
                    return Ok(done_resolve(
                        resolving_funcs,
                        ctx.take_exception(),
                        1,
                        promise,
                    ));
                }
            };

            let resolve_reject: [JSValue; 2] = [
                unsafe { js_async_from_sync_iterator_unwrap_func_create(ctx, done).err()? },
                JSValue::UNDEFINED,
            ];
            value_wrapper_promise.reaction(ctx, &resolve_reject, &resolving_funcs)?;
            Ok(promise.into())
        } else {
            Ok(done_resolve(
                resolving_funcs,
                ctx.take_exception(),
                1,
                promise,
            ))
        }
    } else {
        ctx.throw_type_error("not an Async-from-Sync Iterator");
        Ok(done_resolve(
            resolving_funcs,
            ctx.take_exception(),
            1,
            promise,
        ))
    }
}

pub fn js_async_from_sync_iterator_finalizer(rt: &mut JSRuntime, isa: &ObjectPayload, id: usize) {
    // if let ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(p) = val.object_unchecked().isa() {
    //     drop(Box::from_raw(p));
    // }
}

pub fn js_async_from_sync_iterator_mark(val: JSValue) {
    let p = unsafe { val.as_object_unchecked() };
    if let ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(s) = p.payload() {
        s.obj.gc_mark();
        s.next_method.gc_mark();
    }
}

pub static js_async_from_sync_iterator_proto_funcs: [FieldDef; 3] = [
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
