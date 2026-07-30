// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use crate::{
    JS_CLASS_ITERATOR, atom::Atom, context::ScopedContext, error::Exception,
    number::MAX_SAFE_INTEGER, object::Object, value::JSValue,
};

/// Save pending exception, close iterator, restore exception.
pub(crate) fn close_with_pending_exception(ctx: &mut ScopedContext, obj: Object) {
    let pending_exc = ctx.take_exception();
    let _ = iterator_return(ctx, obj);
    if pending_exc != Exception::No {
        ctx.throw(pending_exc);
    }
}

pub(crate) fn call_iterator_next(
    ctx: &mut ScopedContext,
    obj: Object,
    next: JSValue,
) -> Result<(JSValue, bool), i32> {
    let result = next.call(ctx, obj.into(), &[])?;
    let o = result.ensure_object(ctx)?;
    // Don't read value when done (per spec)
    let done = o
        .get_property(ctx, Atom::CONST_done)?
        .to_bool()
        .unwrap_or(false);
    if done {
        Ok((JSValue::UNDEFINED, true))
    } else {
        let value = o.get_property(ctx, Atom::CONST_value)?;
        Ok((value, false))
    }
}

/// Advance a manually driven iterator: call `next` on `iterator` and unpack
/// the IteratorResult, returning `Some(value)` or `None` when done. Unlike
/// [call_iterator_next] the result is not required to be an object and errors
/// from the `done` conversion are propagated as-is.
pub(crate) fn iterator_step(
    ctx: &mut ScopedContext,
    iterator: JSValue,
    next: Object,
) -> Result<Option<JSValue>, i32> {
    let item = next.call(ctx, iterator, &[])?;
    let done = item.get_property(ctx, Atom::CONST_done)?;
    if done.to_bool()? {
        return Ok(None);
    }
    Ok(Some(item.get_property(ctx, Atom::CONST_value)?))
}

pub(crate) fn iterator_return(ctx: &mut ScopedContext, obj: Object) -> Result<(), i32> {
    let method = obj.get_property(ctx, Atom::CONST_return)?;
    if !method.is_undefined_or_null() {
        method.call(ctx, obj.into(), &[])?;
    }
    Ok(())
}

pub(crate) fn limit_to_count(
    ctx: &mut ScopedContext,
    val: JSValue,
    name: &str,
) -> Result<i64, i32> {
    let limit: f64 = val.to_number(ctx)?.to_f64();
    if limit.is_nan() {
        return Err(ctx.throw_range_error(&format!("{name} value must be a non-negative number")));
    }
    if limit.is_infinite() {
        Ok(MAX_SAFE_INTEGER)
    } else {
        let int_limit = limit.trunc() as i64;
        if int_limit < 0 {
            return Err(ctx.throw_range_error(&format!("{name} value must be a non-negative number")));
        }
        Ok(int_limit)
    }
}

/// Get prototype from constructor for subclass instantiation.
/// Falls back to `JS_CLASS_ITERATOR`'s proto for realm-crossing scenarios.
pub(crate) fn get_proto_from_ctor(
    ctx: &mut ScopedContext,
    ctor: JSValue,
) -> Result<Option<Object>, i32> {
    let realm_ctx = ctor
        .as_object()
        .and_then(|o| o.realm(ctx.ctx_ptr).ok())
        .unwrap_or(ctx.ctx_ptr);

    if let Some(proto) = ctor
        .as_object()
        .and_then(|o| o.get_property(ctx, Atom::CONST_prototype).ok())
        .and_then(|v| v.as_object())
    {
        Ok(Some(proto))
    } else {
        Ok(realm_ctx.class_proto(JS_CLASS_ITERATOR))
    }
}
