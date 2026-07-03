// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::{GcRef, GcTrace, GcTraceCtx};

use crate::{
    Callback, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
    atom::Atom,
    builtins::{
        generator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW},
        promise::{Promise, js_promise_resolve},
    },
    class::ClassID,
    context::ScopedContext,
    error::{Exception, JSResult},
    function::{CallContext, FieldDef, IteratorState, NativeFunction},
    object::{Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

use super::{JS_IteratorGetCompleteValue, ObjectIter, make_iterator_next_result};

#[derive(Debug)]
pub struct JSAsyncFromSyncIteratorData {
    pub(super) obj: Object,
    pub(super) next_method: JSValue,
    pub(super) closed: Cell<bool>,
}

impl GcTrace for JSAsyncFromSyncIteratorData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.obj.trace(tr);
        self.next_method.trace(tr);
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct AsyncFromSyncIterator(GcRef<ObjectImpl>);

object_cast!(AsyncFromSyncIterator, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR);

impl AsyncFromSyncIterator {
    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<(), i32> {
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        let aiter_proto = ctx.async_iterator_proto()?;
        let proto = ctx.create_base_object_with_proto(Some(aiter_proto))?;
        ctx.init_class_proto_internal(class_id, proto, js_async_from_sync_iterator_proto_funcs)?;
        Ok(())
    }
}

fn js_async_from_sync_iterator_unwrap(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let arg0 = cc.arg(0);
    let done = cc.data(0).to_bool()?;
    make_iterator_next_result(ctx, arg0, done)
}

fn js_async_from_sync_iterator_unwrap_func_create(ctx: &mut ScopedContext, done: bool) -> JSValue {
    NativeFunction::create(
        ctx,
        Callback::Func(js_async_from_sync_iterator_unwrap),
        Atom::CONST_empty_string,
        1,
        0,
        ctx.function_proto(),
        Some([JSValue::Bool(done)].into()),
    )
    .map_or(JSValue::EXCEPTION, |x| x.into())
}

fn js_async_from_sync_iterator_close_on_reject(
    ctx: &mut ScopedContext,
    cc: CallContext,
) -> JSResult {
    // set the pending exception first, then best-effort close the iterator (forgiving: don't check return type)
    let err = cc.arg(0);
    let async_val = cc.data(0);
    ctx.throw_value(err);

    if let Ok(async_obj) = Object::try_from(async_val) {
        if let ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(p) = async_obj.payload() {
            let _ = ObjectIter::close_iter_internal(ctx, p.obj);
            p.closed.set(true);
        }
    }

    // close_iter_internal restores the original exception (err) if return() succeeds.
    // If return() throws, its error stays. Either way, return Err(-1)
    // to let the promise reaction use the current exception.
    Err(-1)
}

fn js_async_from_sync_iterator_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let done_resolve = |cx: &mut ScopedContext,
                        rr: [JSValue; 2],
                        err: JSValue,
                        is_reject: usize,
                        promise: Promise|
     -> JSValue {
        let _ = rr[is_reject].call(cx, JSValue::UNDEFINED, &[err]);
        promise.into()
    };

    let (promise, resolving_funcs) = Promise::create(ctx)?;
    debug_assert!(promise.gc_node().is_root_or_local());
    debug_assert!(
        resolving_funcs[0]
            .as_object()
            .unwrap()
            .gc_node()
            .is_root_or_local()
    );
    debug_assert!(
        resolving_funcs[1]
            .as_object()
            .unwrap()
            .gc_node()
            .is_root_or_local()
    );

    // protect resolving_funcs
    // ctx.gc_heap_mut().protect_nodes(&[
    //     resolving_funcs[0].gc_node_ptr().unwrap(),
    //     resolving_funcs[1].gc_node_ptr().unwrap(),
    // ]);

    let this_val = cc.this();
    let p = unsafe { this_val.as_object_unchecked() };

    if let ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(p) = p.payload() {
        if cc.magic() == GEN_MAGIC_RETURN && p.closed.get() {
            // Already closed by onRejected handler; return {value: undefined, done: true}
            let result = make_iterator_next_result(ctx, JSValue::UNDEFINED, true)?;
            return Ok(done_resolve(ctx, resolving_funcs, result, 0, promise));
        }

        let method = if cc.magic() == GEN_MAGIC_NEXT {
            p.next_method
        } else {
            match p.obj.get_property(
                ctx,
                if cc.magic() == GEN_MAGIC_RETURN {
                    Atom::CONST_return
                } else {
                    Atom::CONST_throw
                },
            ) {
                Ok(func_val) => {
                    if func_val.is_undefined_or_null() {
                        if cc.magic() == GEN_MAGIC_RETURN {
                            let result = make_iterator_next_result(ctx, cc.arg(0), true)?;
                            return Ok(done_resolve(ctx, resolving_funcs, result, 0, promise));
                        }

                        // GEN_MAGIC_THROW: IteratorClose then reject with TypeError
                        let _ = ObjectIter::close_iter_internal(ctx, p.obj);
                        let err_val = match ctx.rt().exception() {
                            Exception::No => {
                                ctx.throw_type_error("IteratorClose returned non-object");
                                ctx.take_exception().to_value(ctx)
                            }
                            _ => ctx.take_exception().to_value(ctx),
                        };
                        return Ok(done_resolve(ctx, resolving_funcs, err_val, 1, promise));
                    }
                    func_val
                }
                Err(_) => {
                    let err_val = ctx.take_exception().to_value(ctx);
                    return Ok(done_resolve(ctx, resolving_funcs, err_val, 1, promise));
                }
            }
        };

        if let Ok((mut value, state)) = ObjectIter::apply_next(
            ctx,
            p.obj,
            method,
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
                    value = JS_IteratorGetCompleteValue(ctx, value, &mut _done);
                    if value.is_exception() {
                        let err_val = ctx.take_exception().to_value(ctx);
                        return Ok(done_resolve(ctx, resolving_funcs, err_val, 1, promise));
                    }
                    _done
                }
            };
            let close_on_rejection = cc.magic() != GEN_MAGIC_RETURN;

            let mut value_wrapper_promise: Promise = {
                let cc2 = CallContext::new_simple(
                    ctx.promise_ctor().into(),
                    std::slice::from_ref(&value),
                );
                if let Ok(r) = js_promise_resolve(ctx, cc2) {
                    r.try_into()?
                } else {
                    let err_val = ctx.take_exception().to_value(ctx);

                    // AsyncFromSyncIteratorContinuation step 6:
                    // if valueWrapper is an abrupt completion, done is false,
                    // and closeOnRejection is true -> close the iterator
                    return if close_on_rejection && !done {
                        // Set pending exception first so close_iter_internal
                        // skips return type check (forgiving close, like quickjs)
                        ctx.throw_value(err_val);
                        let _ = ObjectIter::close_iter_internal(ctx, p.obj);
                        p.closed.set(true);
                        let close_val = ctx.take_exception().to_value(ctx);
                        Ok(done_resolve(ctx, resolving_funcs, close_val, 1, promise))
                    } else {
                        Ok(done_resolve(ctx, resolving_funcs, err_val, 1, promise))
                    };
                }
            };

            let close_on_reject_fn = if close_on_rejection && !done {
                // AsyncFromSyncIteratorContinuation step 13-14:
                // onRejected closes the iterator, then rejects.
                // Pass this_val (async-from-sync iterator) as data[0]
                // so the handler can set p.closed after closing.
                NativeFunction::create(
                    ctx,
                    Callback::Func(js_async_from_sync_iterator_close_on_reject),
                    Atom::CONST_empty_string,
                    1,
                    0,
                    ctx.function_proto(),
                    Some([this_val, resolving_funcs[1]].into()),
                )
                .map_or(JSValue::EXCEPTION, JSValue::from)
            } else {
                JSValue::UNDEFINED
            };

            let resolve_reject: [JSValue; 2] = [
                js_async_from_sync_iterator_unwrap_func_create(ctx, done).err()?,
                close_on_reject_fn,
            ];
            value_wrapper_promise.reaction(ctx, resolve_reject, resolving_funcs)?;
            Ok(promise.into())
        } else {
            let err_val = ctx.take_exception().to_value(ctx);
            Ok(done_resolve(ctx, resolving_funcs, err_val, 1, promise))
        }
    } else {
        ctx.throw_type_error("not an Async-from-Sync Iterator");
        let err_val = ctx.take_exception().to_value(ctx);
        Ok(done_resolve(ctx, resolving_funcs, err_val, 1, promise))
    }
}

pub(super) const js_async_from_sync_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC(
        "next",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_NEXT as _,
    ),
    FieldDef::FUNC(
        "return",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_RETURN as _,
    ),
    FieldDef::FUNC(
        "throw",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_THROW as _,
    ),
];

pub(crate) const js_async_iterator_proto_funcs: &[FieldDef] = &[
    #[cfg(feature = "builtin_symbol")]
    FieldDef::FUNC("[Symbol.asyncIterator]", 0, crate::builtins::js_get_this, 0),
];
