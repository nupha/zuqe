// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! ES2024+ `Iterator` global object implementation.

use std::cell::Cell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    Array, JS_CLASS_ITERATOR,
    atom::Atom,
    class::ClassID,
    context::ScopedContext,
    error::{Exception, JSResult},
    function::{CallContext, Callback, FieldDef},
    number::MAX_SAFE_INTEGER,
    object::{JS_PROP_C_W_E, JS_PROP_CONFIGURABLE, JS_PROP_WRITABLE, Object, ObjectPayload},
    value::JSValue,
};

use super::make_iterator_next_result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum JSIteratorHelperKind {
    Drop,
    Filter,
    FlatMap,
    Map,
    Take,
}

#[derive(Debug)]
pub(crate) struct IteratorWrapData {
    obj: Object,
    next: JSValue,
}

impl GcTrace for IteratorWrapData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.obj.trace(tr);
        self.next.trace(tr);
    }
}

#[derive(Debug)]
pub(crate) struct IteratorHelperData {
    obj: Object,
    next: JSValue,
    func: JSValue,
    inner: Cell<JSValue>,
    inner_next: Cell<JSValue>,
    has_inner: Cell<bool>,
    count: Cell<i64>,
    kind: JSIteratorHelperKind,
    executing: Cell<bool>,
    done: Cell<bool>,
    source_closed: Cell<bool>,
}

impl GcTrace for IteratorHelperData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        self.obj.trace(tr);
        self.next.trace(tr);
        self.func.trace(tr);
        if self.has_inner.get() {
            self.inner.get().trace(tr);
            self.inner_next.get().trace(tr);
        }
    }
}

/// Payload for `Iterator.concat` — lazily concatenates iterables.
#[derive(Debug)]
pub(crate) struct IteratorConcatData {
    values: Vec<(JSValue, JSValue)>,
    index: Cell<usize>,
    iter: Cell<Option<Object>>,
    next: Cell<JSValue>,
    running: Cell<bool>,
}

impl GcTrace for IteratorConcatData {
    fn trace(&self, tr: &mut GcTraceCtx) {
        for (obj, method) in &self.values {
            obj.trace(tr);
            method.trace(tr);
        }
        if let Some(o) = self.iter.get() {
            o.trace(tr);
            self.next.get().trace(tr);
        }
    }
}

// =========================================================================
// Helpers
// =========================================================================

/// Helper: save pending exception, close iterator, restore exception.
/// Used when we need to call return() while holding an exception from a callback.
fn close_with_pending_exception(ctx: &mut ScopedContext, obj: Object) {
    let pending_exc = ctx.take_exception();
    let _ = iterator_return(ctx, obj);
    if pending_exc != Exception::No {
        ctx.throw(pending_exc);
    }
}

fn call_iterator_next(
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

fn iterator_return(ctx: &mut ScopedContext, obj: Object) -> Result<(), i32> {
    let method = obj.get_property(ctx, Atom::CONST_return)?;
    if !method.is_undefined_or_null() {
        method.call(ctx, obj.into(), &[])?;
    }
    Ok(())
}

fn limit_to_count(ctx: &mut ScopedContext, val: JSValue, name: &str) -> Result<i64, i32> {
    let limit: f64 = val.to_number(ctx)?.to_f64();
    if limit.is_nan() {
        ctx.throw_range_error(&format!("{name} value must be a non-negative number"));
        return Err(-1);
    }
    if limit.is_infinite() {
        Ok(MAX_SAFE_INTEGER)
    } else {
        let int_limit = limit.trunc() as i64;
        if int_limit < 0 {
            ctx.throw_range_error(&format!("{name} value must be a non-negative number"));
            return Err(-1);
        }
        Ok(int_limit)
    }
}

/// Get the prototype for a subclass instance from the constructor (newTarget).
/// Uses the same logic as `create_instance_with_ctor` but falls back to
/// `JS_CLASS_ITERATOR`'s proto for realm-crossing scenarios.
fn get_proto_from_ctor(ctx: &mut ScopedContext, ctor: JSValue) -> Result<Option<Object>, i32> {
    // Get realm context from ctor's realm, or fall back to current
    let realm_ctx = ctor
        .as_object()
        .and_then(|o| o.realm(ctx.ctx_ptr).ok())
        .unwrap_or(ctx.ctx_ptr);

    // Check if newTarget has an object .prototype — use it directly
    if let Some(proto) = ctor
        .as_object()
        .and_then(|o| o.get_property(ctx, Atom::CONST_prototype).ok())
        .and_then(|v| v.as_object())
    {
        Ok(Some(proto))
    } else {
        // Fall back to the realm's Iterator.prototype
        Ok(realm_ctx.class_proto(JS_CLASS_ITERATOR))
    }
}

// =========================================================================
// Constructor
// =========================================================================

/// Iterator constructor — abstract class but allows subclassing.
///
/// Per spec: `new Iterator()` throws TypeError, but subclass constructors
/// that call `super()` should succeed.
fn js_iterator_constructor(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let new_target = cc.this();

    // Check if new.target is the Iterator constructor itself
    let is_direct = ctx
        .class_constructor(JS_CLASS_ITERATOR)
        .is_some_and(|ctor| new_target.as_object() == Some(ctor));

    if is_direct {
        ctx.throw_type_error("Iterator is an abstract class and cannot be constructed directly");
        return Err(-1);
    }

    // Subclass call: create an object with proto from newTarget's realm
    let proto = get_proto_from_ctor(ctx, new_target)?;
    let obj = ctx.create_base_object_with_proto(proto)?;
    Ok(obj.into())
}

/// Getter for `Iterator.prototype.constructor` — returns the Iterator constructor.
fn js_iterator_constructor_getter(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    let ctor = ctx.class_constructor(JS_CLASS_ITERATOR).ok_or_else(|| {
        ctx.throw_type_error("Iterator constructor not initialized");
        -1
    })?;
    Ok(ctor.into())
}

/// Setter for `Iterator.prototype.constructor` — SetterThatIgnoresPrototypeProperties.
fn js_iterator_constructor_setter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();
    let v = cc.arg(0);

    // Step 1: If this is not an object, throw TypeError
    let this_is_obj = this.is_object();
    if !this_is_obj {
        ctx.throw_type_error("Cannot set property on non-object");
        return Err(-1);
    }

    // Step 2: If this is Iterator.prototype, throw TypeError
    if ctx
        .class_proto(JS_CLASS_ITERATOR)
        .is_some_and(|o| this.as_object() == Some(o))
    {
        ctx.throw_type_error("Cannot assign to constructor on Iterator.prototype");
        return Err(-1);
    }

    // Step 3/4: Define own property
    this.define_property_value(ctx, Atom::CONST_constructor, v, JS_PROP_C_W_E)?;

    Ok(JSValue::UNDEFINED)
}

/// Getter for `[Symbol.toStringTag]` — returns "Iterator".
fn js_iterator_toStringTag_getter(ctx: &mut ScopedContext, _cc: CallContext) -> JSResult {
    let s = Atom::from_str(ctx, "Iterator")?;
    s.force_string(ctx).map(JSValue::make_string)
}

/// Setter for `[Symbol.toStringTag]` — SetterThatIgnoresPrototypeProperties.
fn js_iterator_toStringTag_setter(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let mut this = cc.this();
    let v = cc.arg(0);

    if !this.is_object() {
        ctx.throw_type_error("Cannot set property on non-object");
        return Err(-1);
    }

    if let Some(iter_proto) = ctx.class_proto(JS_CLASS_ITERATOR)
        && let Some(o) = this.as_object()
        && o == iter_proto
    {
        ctx.throw_type_error("Cannot assign to Symbol.toStringTag on Iterator.prototype");
        return Err(-1);
    }

    this.define_property_value(ctx, Atom::CONST_Symbol_toStringTag, v, JS_PROP_C_W_E)?;

    Ok(JSValue::UNDEFINED)
}

// =========================================================================
// Class init
// =========================================================================

pub(crate) fn js_iterator_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let mut proto = ctx.iterator_proto()?;
    proto.install_fields(ctx, js_iterator_proto_full_funcs)?;
    ctx.set_class_proto(class_id, proto);

    ctx.init_class_constructor(
        class_id,
        js_iterator_constructor,
        0,
        0,
        proto,
        js_iterator_funcs,
    )?;

    let ctor = ctx.class_constructor(class_id).ok_or_else(|| {
        ctx.throw_internal_error("Iterator ctor missing");
        -1
    })?;
    ctx.global_obj().define_property_value(
        ctx,
        Atom::CONST_Iterator,
        ctor.into(),
        JS_PROP_WRITABLE | JS_PROP_CONFIGURABLE,
    )?;

    // Per spec: Iterator.prototype.constructor should be an accessor property
    // Override the value property set by bind_ctor_proto.
    // Per spec: Iterator.prototype.constructor and @@toStringTag
    // should be accessor properties (not value properties).
    // Override the value properties set by install_fields.
    {
        let getter_fn: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_constructor_getter),
                "get constructor",
                0,
                0,
            )
            .map_err(|_| -1)?
            .into();
        let setter_fn: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_constructor_setter),
                "set constructor",
                1,
                0,
            )
            .map_err(|_| -1)?
            .into();
        proto
            .define_property_getset(
                ctx,
                Atom::CONST_constructor,
                getter_fn,
                setter_fn,
                JS_PROP_CONFIGURABLE,
            )
            .map(|_| ())?;
    }

    // @@toStringTag: getter returns "Iterator", setter is SetterThatIgnoresPrototypeProperties
    #[cfg(feature = "builtin_symbol")]
    {
        let getter_fn2: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_toStringTag_getter),
                "get [Symbol.toStringTag]",
                0,
                0,
            )
            .map_err(|_| -1)?
            .into();
        let setter_fn2: JSValue = ctx
            .create_native_function(
                Callback::Func(js_iterator_toStringTag_setter),
                "set [Symbol.toStringTag]",
                1,
                0,
            )
            .map_err(|_| -1)?
            .into();
        proto
            .define_property_getset(
                ctx,
                Atom::CONST_Symbol_toStringTag,
                getter_fn2,
                setter_fn2,
                JS_PROP_CONFIGURABLE,
            )
            .map(|_| ())?;
    }

    Ok(())
}

pub(crate) fn js_iterator_helper_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let iter_proto = ctx.iterator_proto()?;
    let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
    proto.install_fields(ctx, js_iterator_helper_proto_funcs)?;
    ctx.set_class_proto(class_id, proto);
    Ok(())
}

pub(crate) fn js_iterator_wrap_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let iter_proto = ctx.iterator_proto()?;
    let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
    proto.install_fields(ctx, js_iterator_wrap_proto_funcs)?;
    ctx.set_class_proto(class_id, proto);
    Ok(())
}

pub(crate) fn js_iterator_concat_class_init(
    ctx: &mut ScopedContext,
    class_id: ClassID,
) -> Result<(), i32> {
    let iter_proto = ctx.iterator_proto()?;
    let mut proto = ctx.create_base_object_with_proto(Some(iter_proto))?;
    proto.install_fields(ctx, js_iterator_concat_proto_funcs)?;
    ctx.set_class_proto(class_id, proto);
    Ok(())
}

// =========================================================================
// Iterator.concat()
// =========================================================================

fn js_iterator_concat(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let argc = cc.length();
    let mut values = Vec::with_capacity(argc);

    // Step: validate each argument in order, collect (object, method) pairs
    for i in 0..argc {
        let item = cc.arg(i);
        if !item.is_object() {
            ctx.throw_type_error("Iterator.concat requires all arguments to be objects");
            return Err(-1);
        }
        let method = item.get_property(ctx, Atom::CONST_Symbol_iterator)?;
        values.push((item, method));
    }

    let concat = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_CONCAT(Box::new(
            IteratorConcatData {
                values,
                index: Cell::new(0),
                iter: Cell::new(None),
                next: Cell::new(JSValue::UNDEFINED),
                running: Cell::new(false),
            },
        )))
        .map_err(|_| -1)?;

    Ok(concat.into())
}

// =========================================================================
// Iterator.concat next() / return()
// =========================================================================

fn js_iterator_concat_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let cd = this_obj.payload().iterator_concat().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Concat");
        -1
    })?;

    if cd.running.get() {
        ctx.throw_type_error("Iterator Concat is already executing");
        return Err(-1);
    }

    cd.running.set(true);

    let values = &cd.values;
    let mut idx = cd.index.get();

    loop {
        if idx >= values.len() {
            cd.running.set(false);
            break make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
        }

        let (ref obj, ref method) = values[idx];

        // If we don't have a current iterator, create one
        let cur_iter = cd.iter.get();
        if cur_iter.is_none() {
            let iter_val = method.call(ctx, *obj, &[])?;
            let iter_obj = iter_val.ensure_object(ctx)?;
            let next_method = iter_obj.get_property(ctx, Atom::CONST_next)?;
            cd.iter.set(Some(iter_obj));
            cd.next.set(next_method);
        }

        let cur_iter_obj = cd.iter.get().unwrap();
        let next_method = cd.next.get();

        let (value, done) = call_iterator_next(ctx, cur_iter_obj, next_method)?;

        if !done {
            cd.running.set(false);
            break make_iterator_next_result(ctx, value, false);
        }

        // This iterator is exhausted, move to next
        cd.index.set(idx + 1);
        idx = cd.index.get();
        cd.iter.set(None);
        cd.next.set(JSValue::UNDEFINED);
    }
}

fn js_iterator_concat_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let cd = this_obj.payload().iterator_concat().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Concat");
        -1
    })?;

    if cd.running.get() {
        ctx.throw_type_error("Iterator Concat is already executing");
        return Err(-1);
    }

    cd.running.set(true);

    let result = (|| -> JSResult {
        // Close current iterator if any (may re-enter via recursive return() call)
        if let Some(cur_iter) = cd.iter.get() {
            iterator_return(ctx, cur_iter)?;
            cd.iter.set(None);
            cd.next.set(JSValue::UNDEFINED);
        }

        // Skip remaining iterables
        cd.index.set(cd.values.len());
        make_iterator_next_result(ctx, cc.arg(0), true)
    })();

    cd.running.set(false);
    result
}

// =========================================================================
// Iterator.from()
// =========================================================================

fn js_iterator_from(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let obj = cc.arg(0);

    if !obj.is_object() && !obj.is_string() {
        ctx.throw_type_error("Iterator.from called on non-object");
        return Err(-1);
    }

    let method = obj.get_property(ctx, Atom::CONST_Symbol_iterator)?;

    let iter = if method.is_undefined_or_null() {
        obj
    } else {
        method.call(ctx, obj, &[])?
    };

    let iter_obj = iter.ensure_object(ctx)?;
    let next = iter_obj.get_property(ctx, Atom::CONST_next)?;

    // Use the Iterator constructor (not its prototype) for instanceof check
    if let Some(iter_ctor) = ctx.class_constructor(JS_CLASS_ITERATOR)
        && iter_obj.instance_of(ctx, iter_ctor.into())?
    {
        return Ok(iter_obj.into());
    }

    let wrap = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_WRAP(Box::new(IteratorWrapData {
            obj: iter_obj,
            next,
        })))
        .map_err(|_| -1)?;

    Ok(wrap.into())
}

// =========================================================================
// Lazy helper factory
// =========================================================================

fn js_create_iterator_helper(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let kind = match cc.magic() as usize {
        0 => JSIteratorHelperKind::Drop,
        1 => JSIteratorHelperKind::Filter,
        2 => JSIteratorHelperKind::FlatMap,
        3 => JSIteratorHelperKind::Map,
        4 => JSIteratorHelperKind::Take,
        _ => unreachable!(),
    };

    let this_val = cc.this();

    let this_obj = this_val.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    // Validate arguments BEFORE reading .next (per spec).
    // On validation failure, close the underlying iterator first.
    let validate_result = match kind {
        JSIteratorHelperKind::Drop | JSIteratorHelperKind::Take => {
            let name = if kind == JSIteratorHelperKind::Drop {
                "drop"
            } else {
                "take"
            };
            limit_to_count(ctx, cc.arg(0), name).map(|c| (JSValue::UNDEFINED, c))
        }
        JSIteratorHelperKind::Filter
        | JSIteratorHelperKind::FlatMap
        | JSIteratorHelperKind::Map => {
            let func = cc.arg(0);
            let what = match kind {
                JSIteratorHelperKind::Filter => "filter",
                JSIteratorHelperKind::FlatMap => "flatMap",
                _ => "map",
            };
            if func.is_callable() {
                Ok((func, 0))
            } else {
                ctx.throw_type_error(&format!("{what} argument must be a function"));
                Err(-1)
            }
        }
    };

    let (func, count) = match validate_result {
        Ok(v) => v,
        Err(_) => {
            // Argument validation failed: save exception, close underlying iterator,
            // then restore exception and propagate.
            let pending_exc = ctx.take_exception();
            let _ = iterator_return(ctx, this_obj);
            if pending_exc != crate::error::Exception::No {
                ctx.throw(pending_exc);
            }
            return Err(-1);
        }
    };

    // Now read .next (GetIteratorDirect)
    let next = this_obj.get_property(ctx, Atom::CONST_next)?;

    let helper = ctx
        .create_instance_with_payload(ObjectPayload::ITERATOR_HELPER(Box::new(
            IteratorHelperData {
                obj: this_obj,
                next,
                func,
                inner: Cell::new(JSValue::UNDEFINED),
                inner_next: Cell::new(JSValue::UNDEFINED),
                has_inner: Cell::new(false),
                count: Cell::new(count),
                kind,
                executing: Cell::new(false),
                done: Cell::new(false),
                source_closed: Cell::new(false),
            },
        )))
        .map_err(|_| -1)?;

    Ok(helper.into())
}

// =========================================================================
// Iterator helper next() / return()
// =========================================================================

fn js_iterator_helper_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let is_return = cc.magic() == 1;
    let this_val = cc.this();

    let this_obj = this_val.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let ip = this_obj.payload().iterator_helper().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Helper");
        -1
    })?;

    if ip.executing.get() {
        ctx.throw_type_error("Iterator Helper is already executing");
        return Err(-1);
    }

    if ip.done.get() {
        if ip.kind == JSIteratorHelperKind::Take && !ip.source_closed.get() {
            // take limit reached but source not yet closed: close now
            // (this runs for both next() and return() calls)
            ip.executing.set(false);
            let close_result = iterator_return(ctx, ip.obj);
            ip.source_closed.set(true);
            close_result?;
        }
        if is_return {
            return make_iterator_next_result(ctx, cc.arg(0), true);
        }
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    ip.executing.set(true);

    if is_return {
        ip.executing.set(false);
        // Close source only if not yet closed
        if !ip.source_closed.get() {
            // For flatMap, also close the inner iterator first
            if ip.kind == JSIteratorHelperKind::FlatMap && ip.has_inner.get() {
                let inner_val = ip.inner.get();
                if let Some(inner_obj) = inner_val.as_object() {
                    let _ = iterator_return(ctx, inner_obj);
                }
            }
            let close_result = iterator_return(ctx, ip.obj);
            ip.source_closed.set(true);
            close_result?;
        }
        ip.done.set(true);
        return make_iterator_next_result(ctx, cc.arg(0), true);
    }

    let result = match ip.kind {
        JSIteratorHelperKind::Drop => drop_next(ctx, ip),
        JSIteratorHelperKind::Filter => filter_next(ctx, ip),
        JSIteratorHelperKind::FlatMap => flatmap_next(ctx, ip),
        JSIteratorHelperKind::Map => map_next(ctx, ip),
        JSIteratorHelperKind::Take => take_next(ctx, ip),
    };

    ip.executing.set(false);
    result
}

fn drop_next(ctx: &mut ScopedContext, ip: &IteratorHelperData) -> JSResult {
    let mut count = ip.count.get();
    while count > 0 {
        let (_, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
        if done {
            ip.done.set(true);
            return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
        }
        count -= 1;
    }
    ip.count.set(count);

    let (value, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
    ip.done.set(done);
    make_iterator_next_result(ctx, value, done)
}

fn filter_next(ctx: &mut ScopedContext, ip: &IteratorHelperData) -> JSResult {
    let mut index = ip.count.get();
    loop {
        let (value, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
        if done {
            ip.done.set(true);
            return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
        }
        let result = ip
            .func
            .call(ctx, JSValue::UNDEFINED, &[value, JSValue::from(index)]);
        index += 1;
        match result {
            Ok(r) => {
                if r.to_bool().unwrap_or(false) {
                    ip.count.set(index);
                    return make_iterator_next_result(ctx, value, false);
                }
            }
            Err(_) => {
                // Predicate threw: close underlying iterator, preserve original error
                close_with_pending_exception(ctx, ip.obj);
                ip.done.set(true);
                return Err(-1);
            }
        }
    }
}

fn flatmap_next(ctx: &mut ScopedContext, ip: &IteratorHelperData) -> JSResult {
    let mut index = ip.count.get();
    loop {
        if !ip.has_inner.get() {
            let (value, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
            if done {
                ip.done.set(true);
                return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
            }
            let mapped = ip
                .func
                .call(ctx, JSValue::UNDEFINED, &[value, JSValue::from(index)]);
            index += 1;
            ip.count.set(index);
            let mapped = match mapped {
                Ok(v) => v,
                Err(_) => {
                    close_with_pending_exception(ctx, ip.obj);
                    ip.done.set(true);
                    return Err(-1);
                }
            };

            let inner_obj = mapped.as_object().ok_or_else(|| {
                ctx.throw_type_error("flatMap mapper must return an iterable");
                -1
            })?;

            // Get [Symbol.iterator] — fallback to using the object as iterator if null/undefined
            let inner_method = inner_obj.get_property(ctx, Atom::CONST_Symbol_iterator)?;
            let (inner_iter_val, inner_next) = if inner_method.is_undefined_or_null() {
                // Fallback: treat object as iterator (has .next directly)
                (mapped, inner_obj.get_property(ctx, Atom::CONST_next)?)
            } else {
                // Use [Symbol.iterator] to get an iterator
                let inner_iter_val = inner_method.call(ctx, mapped, &[])?;
                let inner_iter = inner_iter_val.ensure_object(ctx)?;
                let inner_next = inner_iter.get_property(ctx, Atom::CONST_next)?;
                (inner_iter.into(), inner_next)
            };

            ip.inner.set(inner_iter_val);
            ip.inner_next.set(inner_next);
            ip.has_inner.set(true);
        }

        let inner_val = ip.inner.get();
        let inner_next = ip.inner_next.get();

        if let Some(inner_obj) = inner_val.as_object() {
            let (iv, inner_done) = call_iterator_next(ctx, inner_obj, inner_next)?;
            if !inner_done {
                return make_iterator_next_result(ctx, iv, false);
            }
        }

        ip.has_inner.set(false);
        ip.inner.set(JSValue::UNDEFINED);
        ip.inner_next.set(JSValue::UNDEFINED);
    }
}

fn map_next(ctx: &mut ScopedContext, ip: &IteratorHelperData) -> JSResult {
    let mut index = ip.count.get();
    let (value, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
    if done {
        ip.done.set(true);
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }
    let result = ip
        .func
        .call(ctx, JSValue::UNDEFINED, &[value, JSValue::from(index)]);
    index += 1;
    ip.count.set(index);
    match result {
        Ok(r) => make_iterator_next_result(ctx, r, false),
        Err(_) => {
            close_with_pending_exception(ctx, ip.obj);
            ip.done.set(true);
            Err(-1)
        }
    }
}

fn take_next(ctx: &mut ScopedContext, ip: &IteratorHelperData) -> JSResult {
    // If source already closed, just return done
    if ip.source_closed.get() {
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    let count = ip.count.get();
    if count <= 0 {
        // Limit reached: close source. Even if close fails, mark source as closed
        // so subsequent calls don't try again.
        let close_result = iterator_return(ctx, ip.obj);
        ip.source_closed.set(true);
        ip.done.set(true);
        close_result?;
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }
    ip.count.set(count - 1);

    let (value, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
    if done {
        // Source naturally exhausted.
        ip.done.set(true);
        ip.source_closed.set(true);
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    // If this was the last requested item, next call will close the source
    if ip.count.get() == 0 {
        ip.done.set(true);
    }
    make_iterator_next_result(ctx, value, false)
}

// =========================================================================
// IteratorWrap next() / return()
// =========================================================================

fn js_iterator_wrap_next(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let wrap = this_obj.payload().iterator_wrap().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Wrap");
        -1
    })?;

    let (value, done) = call_iterator_next(ctx, wrap.obj, wrap.next)?;
    make_iterator_next_result(ctx, value, done)
}

fn js_iterator_wrap_return(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let wrap = this_obj.payload().iterator_wrap().ok_or_else(|| {
        ctx.throw_type_error("not an Iterator Wrap");
        -1
    })?;

    let return_method = wrap.obj.get_property(ctx, Atom::CONST_return)?;
    if !return_method.is_undefined_or_null() {
        let result = return_method.call(ctx, wrap.obj.into(), &[cc.arg(0)])?;
        return make_iterator_next_result(ctx, result, true);
    }

    make_iterator_next_result(ctx, cc.arg(0), true)
}

// =========================================================================
// Eager methods: every, find, forEach, some
// =========================================================================

fn eager_iterate(ctx: &mut ScopedContext, this_val: JSValue, func: JSValue, mode: i32) -> JSResult {
    let this_obj = this_val.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;
    let next = this_obj.get_property(ctx, Atom::CONST_next)?;
    let mut index: i64 = 0;

    loop {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Ok(match mode {
                0 => JSValue::TRUE,
                2 => JSValue::UNDEFINED,
                3 => JSValue::FALSE,
                _ => JSValue::UNDEFINED,
            });
        }
        // Per spec: callback's this is undefined
        let result = func.call(ctx, JSValue::UNDEFINED, &[value, JSValue::from(index)]);
        index += 1;
        let val = match result {
            Ok(r) => r,
            Err(_) => {
                // Callback threw: close underlying, preserve original error
                close_with_pending_exception(ctx, this_obj);
                return Err(-1);
            }
        };
        let truthy = val.to_bool().unwrap_or(false);

        match mode {
            0 => {
                if !truthy {
                    iterator_return(ctx, this_obj)?;
                    return Ok(JSValue::FALSE);
                }
            }
            1 => {
                if truthy {
                    iterator_return(ctx, this_obj)?;
                    return Ok(value);
                }
            }
            2 => {}
            3 => {
                if truthy {
                    iterator_return(ctx, this_obj)?;
                    return Ok(JSValue::TRUE);
                }
            }
            _ => unreachable!(),
        }
    }
}

fn validate_iterate_arg_and_close_on_fail(
    ctx: &mut ScopedContext,
    this_val: JSValue,
    func: JSValue,
    name: &str,
) -> Result<(), i32> {
    if !func.is_callable() {
        // Save exception, close underlying, restore exception
        let this_obj = this_val.as_object();
        ctx.throw_type_error(&format!("{name} argument must be a function"));
        if let Some(o) = this_obj {
            close_with_pending_exception(ctx, o);
        }
        return Err(-1);
    }
    Ok(())
}

fn js_iterator_proto_every(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "every")?;
    eager_iterate(ctx, this, func, 0)
}

fn js_iterator_proto_find(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "find")?;
    eager_iterate(ctx, this, func, 1)
}

fn js_iterator_proto_forEach(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "forEach")?;
    eager_iterate(ctx, this, func, 2)
}

fn js_iterator_proto_some(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let func = cc.arg(0);
    validate_iterate_arg_and_close_on_fail(ctx, this, func, "some")?;
    eager_iterate(ctx, this, func, 3)
}

// =========================================================================
// reduce
// =========================================================================

fn js_iterator_proto_reduce(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;
    let func = cc.arg(0);
    if !func.is_callable() {
        ctx.throw_type_error("reduce argument must be a function");
        close_with_pending_exception(ctx, this_obj);
        return Err(-1);
    }

    let next = this_obj.get_property(ctx, Atom::CONST_next)?;
    let mut accumulator: JSValue;
    let mut index: i64;

    if cc.length() >= 2 {
        accumulator = cc.arg(1);
        index = 0;
    } else {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            ctx.throw_type_error("reduce of empty iterator with no initial value");
            return Err(-1);
        }
        accumulator = value;
        index = 1;
    }

    loop {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Ok(accumulator);
        }
        let result = func.call(
            ctx,
            JSValue::UNDEFINED,
            &[accumulator, value, JSValue::from(index)],
        );
        match result {
            Ok(r) => {
                accumulator = r;
            }
            Err(_) => {
                close_with_pending_exception(ctx, this_obj);
                return Err(-1);
            }
        }
        index += 1;
    }
}

// =========================================================================
// toArray
// =========================================================================

fn js_iterator_proto_toArray(ctx: &mut ScopedContext, cc: CallContext) -> JSResult {
    let this = cc.this();
    let this_obj = this.as_object().ok_or_else(|| {
        ctx.throw_type_error_not_an_object();
        -1
    })?;

    let next = this_obj.get_property(ctx, Atom::CONST_next)?;
    let mut arr = Array::create(ctx)?;
    let mut i: u32 = 0;

    loop {
        let (value, done) = call_iterator_next(ctx, this_obj, next)?;
        if done {
            return Ok(arr.into());
        }
        arr.define_property_value_i64(ctx, i as i64, value, JS_PROP_C_W_E)?;
        i += 1;
    }
}

// =========================================================================
// Field definition tables
// =========================================================================

pub(crate) const js_iterator_funcs: &[FieldDef] = &[
    FieldDef::FUNC("from", 1, js_iterator_from, 0),
    FieldDef::FUNC("concat", 0, js_iterator_concat, 0),
];

pub(crate) const js_iterator_proto_full_funcs: &[FieldDef] = &[
    FieldDef::FUNC("every", 1, js_iterator_proto_every, 0),
    FieldDef::FUNC("find", 1, js_iterator_proto_find, 0),
    FieldDef::FUNC("forEach", 1, js_iterator_proto_forEach, 0),
    FieldDef::FUNC("some", 1, js_iterator_proto_some, 0),
    FieldDef::FUNC("reduce", 1, js_iterator_proto_reduce, 0),
    FieldDef::FUNC("toArray", 0, js_iterator_proto_toArray, 0),
    FieldDef::FUNC(
        "drop",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Drop as i16,
    ),
    FieldDef::FUNC(
        "filter",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Filter as i16,
    ),
    FieldDef::FUNC(
        "flatMap",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::FlatMap as i16,
    ),
    FieldDef::FUNC(
        "map",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Map as i16,
    ),
    FieldDef::FUNC(
        "take",
        1,
        js_create_iterator_helper,
        JSIteratorHelperKind::Take as i16,
    ),
];

pub(crate) const js_iterator_helper_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 0, js_iterator_helper_next, 0),
    FieldDef::FUNC("return", 0, js_iterator_helper_next, 1),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Iterator Helper",
        JS_PROP_CONFIGURABLE,
    ),
];

pub(crate) const js_iterator_wrap_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 0, js_iterator_wrap_next, 0),
    FieldDef::FUNC("return", 0, js_iterator_wrap_return, 0),
];

pub(crate) const js_iterator_concat_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 0, js_iterator_concat_next, 0),
    FieldDef::FUNC("return", 0, js_iterator_concat_return, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Iterator Concat",
        JS_PROP_CONFIGURABLE,
    ),
];
