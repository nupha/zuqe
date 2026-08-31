// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::GcTrace;

use crate::{
    Callback,
    atom::Atom,
    builtins::{
        generator::{GEN_MAGIC_NEXT, GEN_MAGIC_RETURN, GEN_MAGIC_THROW},
        promise::{Promise, js_promise_resolve},
    },
    class::ClassID,
    context::ScopedContext,
    error::{Exception, JSResult},
    function::{CallContext, FieldDef, IteratorState, NativeFunction},
    object::{AsyncFromSyncIterator, Object},
    value::JSValue,
};

use super::{JS_IteratorGetCompleteValue, ObjectIter, make_iterator_next_result};

#[derive(Debug, GcTrace)]
pub struct JSAsyncFromSyncIteratorData {
    pub(super) obj: Object,
    pub(super) next_method: JSValue,
    pub(super) closed: Cell<bool>,
}

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
        Some([JSValue::from(done)].into()),
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
    let e = ctx.throw_value(err);

    if let Ok(async_obj) = Object::try_from(async_val)
        && let Some(p) = async_obj.payload_async_from_sync_iterator()
    {
        let _ = ObjectIter::close_iter_internal(ctx, p.obj);
        p.closed.set(true);
    }

    // close_iter_internal restores the original exception (err) if return() succeeds.
    // If return() throws, its error stays. Either way, return Err(-1)
    // to let the promise reaction use the current exception.
    Err(e)
}

/// Reject the promise with the current exception on the context.
fn reject_current(ctx: &mut ScopedContext, rr: [JSValue; 2], promise: Promise) -> JSResult {
    let err_val = ctx.take_exception().to_value(ctx);
    let _ = rr[1].call(ctx, JSValue::UNDEFINED, &[err_val]);
    Ok(promise.into())
}

/// Close the iterator (forgiving), mark closed, and reject with the result exception.
fn close_iterator_and_reject(
    ctx: &mut ScopedContext,
    p: &JSAsyncFromSyncIteratorData,
    rr: [JSValue; 2],
    promise: Promise,
) -> JSResult {
    let _ = ObjectIter::close_iter_internal(ctx, p.obj);
    p.closed.set(true);

    let err_val = match ctx.rt().exception() {
        Exception::No => {
            // throw() is undefined and IteratorClose succeeded:
            // reject with a new TypeError per spec
            let _ = ctx.throw_type_error("throw is not a method");
            ctx.take_exception().to_value(ctx)
        }
        _ => ctx.take_exception().to_value(ctx),
    };
    let _ = rr[1].call(ctx, JSValue::UNDEFINED, &[err_val]);
    Ok(promise.into())
}

fn js_async_from_sync_iterator_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
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

    let this_val = cc.this();
    let p = unsafe { this_val.as_object_unchecked() };

    if let Some(p) = p.payload_async_from_sync_iterator() {
        if cc.magic() == GEN_MAGIC_RETURN && p.closed.get() {
            let result = make_iterator_next_result(ctx, JSValue::UNDEFINED, true)?;
            let _ = resolving_funcs[0].call(ctx, JSValue::UNDEFINED, &[result]);
            return Ok(promise.into());
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
                        return if cc.magic() == GEN_MAGIC_RETURN {
                            let result = make_iterator_next_result(ctx, cc.arg(0), true)?;
                            let _ = resolving_funcs[0].call(ctx, JSValue::UNDEFINED, &[result]);
                            Ok(promise.into())
                        } else {
                            close_iterator_and_reject(ctx, p, resolving_funcs, promise)
                        };
                    }

                    func_val
                }
                Err(_) => {
                    return reject_current(ctx, resolving_funcs, promise);
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
                        return reject_current(ctx, resolving_funcs, promise);
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
                    return if close_on_rejection && !done {
                        let _ = ctx.throw_value(err_val);
                        close_iterator_and_reject(ctx, p, resolving_funcs, promise)
                    } else {
                        let _ = resolving_funcs[1].call(ctx, JSValue::UNDEFINED, &[err_val]);
                        Ok(promise.into())
                    };
                }
            };

            let close_on_reject_fn = if close_on_rejection && !done {
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
            reject_current(ctx, resolving_funcs, promise)
        }
    } else {
        let _ = ctx.throw_type_error("not an Async-from-Sync Iterator");
        reject_current(ctx, resolving_funcs, promise)
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

pub(crate) const js_async_iterator_proto_funcs: &[FieldDef] = &[FieldDef::FUNC(
    "[Symbol.asyncIterator]",
    0,
    crate::builtins::js_get_this,
    0,
)];
