use gc_lite::{Collect, GcRef};

use crate::{
    JS_CLASS_ASYNC_FROM_SYNC_ITERATOR,
    atom::Atom,
    builtins::{
        generator::js_async_iterator_proto_funcs,
        promise::{Promise, js_promise_resolve},
    },
    class::ClassID,
    context::{Context, ContextImpl},
    error::JSResult,
    function::{CallContext, FieldDef, IteratorState, NativeFunctionWithData},
    object::{ObjectImpl, ObjectPayload, ObjectRef},
    object_cast,
    value::JSValue,
};

use super::{JS_IteratorGetCompleteValue, ObjectIter, js_create_iterator_result};

#[derive(Debug)]
pub struct JSAsyncFromSyncIteratorData {
    pub(super) obj: ObjectRef,
    pub(super) next_method: JSValue,
}

unsafe impl Collect for JSAsyncFromSyncIteratorData {
    fn trace(&self, tracer: &mut gc_lite::Tracer) {
        self.obj.trace(tracer);
        self.next_method.trace(tracer);
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct AsyncFromSyncIterator(GcRef<ObjectImpl>);

object_cast!(AsyncFromSyncIterator, JS_CLASS_ASYNC_FROM_SYNC_ITERATOR);

impl AsyncFromSyncIterator {
    pub fn class_initialize(mut ctx: Context, _: ClassID) -> Result<Option<ObjectRef>, i32> {
        let aiter_proto = ctx.async_iterator_proto()?;
        let mut proto = ctx.create_base_object_with_proto(Some(aiter_proto))?;
        proto.install_field_defs(&ctx, &js_async_from_sync_iterator_proto_funcs)?;
        Ok(Some(proto))
    }
}

impl ContextImpl {
    /// Initialize async iterator proto object which is used as proto of:
    /// AsyncFromSyncIterator, AsyncGenerator.
    /// if this proto is not initialized, will create it inside.
    pub(crate) fn async_iterator_proto(&mut self) -> Result<ObjectRef, i32> {
        if let Some(o) = self.async_iterator_proto {
            Ok(o)
        } else {
            let mut o = self.create_base_object()?;
            o.install_field_defs(self, &js_async_iterator_proto_funcs)?;
            self.async_iterator_proto.replace(o);
            Ok(o)
        }
    }
}

pub const GEN_MAGIC_NEXT: i32 = 0;
pub const GEN_MAGIC_RETURN: i32 = 1;
pub const GEN_MAGIC_THROW: i32 = 2;

pub fn js_async_from_sync_iterator_unwrap(cc: CallContext) -> JSResult {
    let done = cc.data(0).to_bool()?;
    js_create_iterator_result(cc.ctx(), *cc.arg(0), done)
}

pub unsafe fn js_async_from_sync_iterator_unwrap_func_create(
    ctx: &ContextImpl,
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

fn js_async_from_sync_iterator_next(cc: CallContext) -> JSResult {
    let done_resolve = |cx: &ContextImpl,
                        rr: [JSValue; 2],
                        err: JSValue,
                        is_reject: usize,
                        promise: Promise|
     -> JSValue {
        let _ = rr[is_reject].call(cx, JSValue::UNDEFINED, &[err]);
        promise.into()
    };

    let mut ctx = cc.ctx_ptr();
    let (promise, resolving_funcs) = Promise::create(&ctx)?;

    let this_val = cc.this();
    let p = unsafe { this_val.as_object_unchecked() };

    if let ObjectPayload::ASYNC_FROM_SYNC_ITERATOR(p) = p.payload() {
        let method = if cc.magic() == GEN_MAGIC_NEXT {
            p.next_method
        } else {
            match p.obj.get_property(
                &ctx,
                if cc.magic() == GEN_MAGIC_RETURN {
                    Atom::CONST_return
                } else {
                    Atom::CONST_throw
                },
            ) {
                Ok(fval) => {
                    if fval.is_undefined_or_null() {
                        let (err, is_reject) = if cc.magic() == GEN_MAGIC_RETURN {
                            (js_create_iterator_result(&ctx, *cc.arg(0), true)?, 0)
                        } else {
                            (*cc.arg(0), 1)
                        };
                        return Ok(done_resolve(&ctx, resolving_funcs, err, is_reject, promise));
                    }

                    fval
                }
                Err(_) => {
                    return Ok(done_resolve(
                        &ctx,
                        resolving_funcs,
                        ctx.rt().take_exception(),
                        1,
                        promise,
                    ));
                }
            }
        };

        if let Ok((mut value, state)) = ObjectIter::do_apply_next_method(
            &ctx,
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
                    value = JS_IteratorGetCompleteValue(&ctx, value, &mut _done);
                    if value.is_exception() {
                        let err = ctx.rt().take_exception();
                        return Ok(done_resolve(&ctx, resolving_funcs, err, 1, promise));
                    }
                    _done
                }
            };

            let mut value_wrapper_promise: Promise = {
                let cc =
                    CallContext::new_simple(ctx, ctx.promise_ctor, std::slice::from_ref(&value));
                if let Ok(r) = js_promise_resolve(cc) {
                    r.try_into()?
                } else {
                    return Ok(done_resolve(
                        &ctx,
                        resolving_funcs,
                        ctx.rt().take_exception(),
                        1,
                        promise,
                    ));
                }
            };

            let resolve_reject: [JSValue; 2] = [
                unsafe { js_async_from_sync_iterator_unwrap_func_create(&ctx, done).err()? },
                JSValue::UNDEFINED,
            ];
            value_wrapper_promise.reaction(&mut ctx, resolve_reject, resolving_funcs)?;
            Ok(promise.into())
        } else {
            Ok(done_resolve(
                &ctx,
                resolving_funcs,
                ctx.rt().take_exception(),
                1,
                promise,
            ))
        }
    } else {
        ctx.throw_type_error("not an Async-from-Sync Iterator");
        Ok(done_resolve(
            &ctx,
            resolving_funcs,
            ctx.rt().take_exception(),
            1,
            promise,
        ))
    }
}

pub static js_async_from_sync_iterator_proto_funcs: [FieldDef; 3] = [
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
