// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

use std::cell::Cell;

use gc_lite::{GcTrace, GcTraceCtx};

use crate::{
    class::ClassID,
    context::ScopedContext,
    error::JSResult,
    function::{CallContext, FieldDef},
    object::Object,
    value::JSValue,
};

use super::{
    call_iterator_next, close_with_pending_exception, iterator_return,
    js_iterator_proto_Symbol_iterator, make_iterator_next_result,
};

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
pub(crate) struct IteratorHelperData {
    pub(crate) obj: Object,
    pub(crate) next: JSValue,
    pub(crate) func: JSValue,
    pub(crate) inner: Cell<JSValue>,
    pub(crate) inner_next: Cell<JSValue>,
    pub(crate) has_inner: Cell<bool>,
    pub(crate) count: Cell<i64>,
    pub(crate) kind: JSIteratorHelperKind,
    pub(crate) executing: Cell<bool>,
    pub(crate) done: Cell<bool>,
    pub(crate) source_closed: Cell<bool>,
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

    // Check for IteratorZip payload first (zip results share IteratorHelper proto)
    if this_obj.payload().iterator_zip().is_some() {
        return if is_return {
            super::js_iterator_zip_return(ctx, cc)
        } else {
            super::js_iterator_zip_next(ctx, cc)
        };
    }

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
                // Predicate threw: close underlying, preserve original error
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

            // Get [Symbol.iterator]; fallback to direct .next if null/undefined
            let inner_method =
                inner_obj.get_property(ctx, crate::atom::Atom::CONST_Symbol_iterator)?;
            let (inner_iter_val, inner_next) = if inner_method.is_undefined_or_null() {
                // Fallback: treat object as iterator (has .next directly)
                (
                    mapped,
                    inner_obj.get_property(ctx, crate::atom::Atom::CONST_next)?,
                )
            } else {
                let inner_iter_val = inner_method.call(ctx, mapped, &[])?;
                let inner_iter = inner_iter_val.ensure_object(ctx)?;
                let inner_next = inner_iter.get_property(ctx, crate::atom::Atom::CONST_next)?;
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
    if ip.source_closed.get() {
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    let count = ip.count.get();
    if count <= 0 {
        // Limit reached: close source, mark as closed so subsequent calls don't retry.
        let close_result = iterator_return(ctx, ip.obj);
        ip.source_closed.set(true);
        ip.done.set(true);
        close_result?;
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }
    ip.count.set(count - 1);

    let (value, done) = call_iterator_next(ctx, ip.obj, ip.next)?;
    if done {
        ip.done.set(true);
        ip.source_closed.set(true);
        return make_iterator_next_result(ctx, JSValue::UNDEFINED, true);
    }

    // Next call will close the source if this was the last item
    if ip.count.get() == 0 {
        ip.done.set(true);
    }
    make_iterator_next_result(ctx, value, false)
}

// =========================================================================
// Field defs
// =========================================================================

pub(crate) const js_iterator_helper_proto_funcs: &[FieldDef] = &[
    FieldDef::FUNC("next", 0, js_iterator_helper_next, 0),
    FieldDef::FUNC("return", 0, js_iterator_helper_next, 1),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::FUNC("[Symbol.iterator]", 0, js_iterator_proto_Symbol_iterator, 0),
    #[cfg(feature = "builtin_symbol")]
    FieldDef::STR(
        "[Symbol.toStringTag]",
        "Iterator Helper",
        crate::object::JS_PROP_CONFIGURABLE,
    ),
];
