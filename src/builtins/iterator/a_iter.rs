// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

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
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState, NativeFunction},
    object::{Object, ObjectImpl, ObjectPayload},
    object_cast,
    value::JSValue,
};

use super::{JS_IteratorGetCompleteValue, ObjectIter, js_create_iterator_result};

#[derive(Debug)]
pub struct JSAsyncFromSyncIteratorData {
    pub(super) obj: Object,
    pub(super) next_method: JSValue,
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
    pub fn class_init(ctx: &mut ScopedContext, class_id: ClassID) -> Result<Option<Object>, i32> {
        debug_assert!(ctx.class_slots[class_id as usize].proto.is_none());

        let aiter_proto = ctx.async_iterator_proto()?;
        let mut proto = ctx.create_base_object_with_proto(Some(aiter_proto))?;
        proto.install_fields(ctx, js_async_from_sync_iterator_proto_funcs)?;

        ctx.class_slots[class_id as usize].proto.replace(proto);

        Ok(Some(proto))
    }
}

fn js_async_from_sync_iterator_unwrap(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let arg0 = cc.arg(0);
    let done = cc.data(0).to_bool()?;
    js_create_iterator_result(ctx, arg0, done)
}

fn js_async_from_sync_iterator_unwrap_func_create(ctx: &mut ScopedContext, done: bool) -> JSValue {
    NativeFunction::create_internal(
        ctx,
        Callback::Func(js_async_from_sync_iterator_unwrap),
        Atom::CONST_empty_string,
        1,
        0,
        None,
        Some([JSValue::Bool(done)].into()),
    )
    .map_or(JSValue::EXCEPTION, |x| x.into())
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
                Ok(fval) => {
                    if fval.is_undefined_or_null() {
                        let (err, is_reject) = if cc.magic() == GEN_MAGIC_RETURN {
                            (js_create_iterator_result(ctx, cc.arg(0), true)?, 0)
                        } else {
                            (cc.arg(0), 1)
                        };
                        return Ok(done_resolve(ctx, resolving_funcs, err, is_reject, promise));
                    }

                    fval
                }
                Err(_) => {
                    let exc = ctx.rt_mut().take_exception();
                    return Ok(done_resolve(ctx, resolving_funcs, exc, 1, promise));
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
                        let err = ctx.rt_mut().take_exception();
                        return Ok(done_resolve(ctx, resolving_funcs, err, 1, promise));
                    }
                    _done
                }
            };

            let mut value_wrapper_promise: Promise = {
                if let Ok(r) = js_promise_resolve(
                    ctx,
                    CallContext::new_simple(
                        ctx.promise_ctor().into(),
                        std::slice::from_ref(&value),
                    ),
                ) {
                    r.try_into()?
                } else {
                    let exc = ctx.rt_mut().take_exception();
                    return Ok(done_resolve(ctx, resolving_funcs, exc, 1, promise));
                }
            };

            let resolve_reject: [JSValue; 2] = [
                js_async_from_sync_iterator_unwrap_func_create(ctx, done).err()?,
                JSValue::UNDEFINED,
            ];
            value_wrapper_promise.reaction(ctx, resolve_reject, resolving_funcs)?;
            Ok(promise.into())
        } else {
            let exc = ctx.rt_mut().take_exception();
            Ok(done_resolve(ctx, resolving_funcs, exc, 1, promise))
        }
    } else {
        ctx.throw_type_error("not an Async-from-Sync Iterator");
        let exc = ctx.rt_mut().take_exception();
        Ok(done_resolve(ctx, resolving_funcs, exc, 1, promise))
    }
}

const js_async_from_sync_iterator_proto_funcs: &[FieldDef] = &[
    FieldDef::CALL(
        "next",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_NEXT as _,
    ),
    FieldDef::CALL(
        "return",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_RETURN as _,
    ),
    FieldDef::CALL(
        "throw",
        1,
        js_async_from_sync_iterator_next,
        GEN_MAGIC_THROW as _,
    ),
];
