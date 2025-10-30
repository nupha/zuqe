use std::ops::Deref;

use crate::{
    JS_CFUNC_NATIVE_DEF, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
    atom::consts::*,
    callable::NativeFunctionWithData,
    class::ClassID,
    context::JSContext,
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState},
    generator::js_async_iterator_proto_funcs,
    iterator::ObjectIter,
    object::{JSObject, ObjectPayload},
    object_cast,
    promise::{Promise, js_promise_resolve},
    runtime::JSRuntime,
    value::JSValue,
};

use super::{JS_IteratorGetCompleteValue, js_create_iterator_result};

#[derive(Debug)]
pub struct JSAsyncFromSyncIteratorData {
    pub(crate) obj: JSObject,
    pub(crate) next_method: JSValue,
}

#[derive(Debug, Clone)]
#[repr(transparent)]
pub struct AsyncFromSyncIterator(JSObject);

object_cast!(AsyncFromSyncIterator, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR);

impl AsyncFromSyncIterator {
    pub fn class_initialize(ctx: &JSContext, _: ClassID) -> Result<Option<JSObject>, i32> {
        let proto = ctx.create_base_object_with_proto(Some(ctx.async_iterator_proto()?))?;
        proto.install_field_defs(ctx, &js_async_from_sync_iterator_proto_funcs)?;
        Ok(Some(proto))
    }
}

impl JSContext {
    /// get async iterator proto object which is used as proto of:
    /// AsyncFromSyncIterator, AsyncGenerator.
    /// if this proto is not initialized, will create it inside.
    pub(crate) fn async_iterator_proto(&self) -> Result<JSObject, i32> {
        if let Some(o) = self.0.borrow().async_iterator_proto.as_ref().cloned() {
            Ok(o)
        } else {
            let o = self.create_base_object()?;
            o.install_field_defs(self, &js_async_iterator_proto_funcs)?;
            self.0.borrow_mut().async_iterator_proto.replace(o.clone());
            Ok(o)
        }
    }
}

pub const GEN_MAGIC_NEXT: i32 = 0;
pub const GEN_MAGIC_RETURN: i32 = 1;
pub const GEN_MAGIC_THROW: i32 = 2;

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
                    value = JS_IteratorGetCompleteValue(ctx, &value, &mut _done);
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

pub const fn js_async_from_sync_iterator_finalizer(
    _rt: &mut JSRuntime,
    _isa: &ObjectPayload,
    _id: usize,
) {
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
